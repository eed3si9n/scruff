//! Checks that rewritten code means the same as the original, for testing
//! formatters.
//!
//! Two texts are equivalent when:
//!
//! 1. The rewrite has no syntax errors unless the original did.
//! 2. Their trees have the same shape once whitespace and comments are set
//!    aside: the same nodes, nested the same way, with the same token text.
//!    Comparing the shape, not only the tokens, catches Scala 3 code moved
//!    into a different block by re-indenting.
//! 3. They have the same comments, in the same order, each attached to the
//!    same token in the same position (leading, trailing, or end of file).
//!
//! A trailing comma, one followed by a line break and then `)` or `]`, may
//! be added or removed: it means nothing. And a dialect's new syntax means
//! the same as the old: `import a.*` as `import a._`, `b as c` as `b => c`
//! in an import or export, and braces around a lone rename mean nothing,
//! `import a.b as c` as `import a.{b => c}`. A type wildcard `?` means the
//! same as `_`: `List[?]` as `List[_]`. A vararg splice `xs*` means the same
//! as `xs: _*`, and in a pattern as `xs @ _*`. And Scala 3's control syntax means
//! the same as the old: `if c then a` as `if (c) a`, `while c do x` as
//! `while (c) x`, and `for x <- xs do f(x)` as `for (x <- xs) f(x)`. Braces
//! mean the same as Scala 3's indentation: `def f = { a }` as `def f =` with
//! `a` indented below, and `class A { ... }` as `class A:`. A braced argument
//! means the same as a colon argument: `xs.map { x => a }` as `xs.map: x =>`
//! with `a` indented below. An end marker, as `end f` after `def f =`, may
//! be added or removed where it names what it ends: it means nothing.
//! And an `implicit` parameter clause means the same as a `using` one, as
//! `def f(implicit ec: EC)` as `def f(using ec: EC)`, unless a class
//! parameter in it is a `val` or `var` or has modifiers. An intersection
//! type `A & B & C` means the same as a compound one, `A with B with C`:
//! tree-sitter-scala reads the first as nested infix types and the second
//! as one compound type, so a chain of `&` counts as one compound type.
//! In a pattern, as `case x: (A & B)`, the type needs parentheses, which
//! `A with B` may go without: `case x: A & B` is the pattern `x: A` and `B`.
//! Commas between the parents a class extends mean the same as `with`:
//! `extends A, B` as `extends A with B`. And parentheses around a boolean
//! chain mean nothing, as `(a && b)` as `a && b`: the tree holds the
//! grouping, so a change of precedence is still a different shape. In
//! Scala 2 they let a broken chain start its lines with the operators.

use std::fmt;

use super::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, SyntaxTrivia, TextSize, Tree,
    TriviaKind, WalkEvent, parse, scaladoc::scaladoc_lines,
};

/// The first way two texts differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub kind: DifferenceKind,
    pub message: String,
    /// Where the difference is in the original, if it's there.
    pub before: Option<Location>,
    /// Where the difference is in the rewrite, if it's there.
    pub after: Option<Location>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferenceKind {
    /// The rewrite has a syntax error and the original doesn't.
    NewSyntaxError,
    /// The code differs: a token, or how nodes are nested.
    Code,
    /// A comment was dropped, added, changed, or attached somewhere else.
    Comment,
}

/// A position in one of the two texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub offset: TextSize,
    /// 1-based.
    pub line: usize,
    /// 1-based, in characters.
    pub column: usize,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        let at = |loc: Option<Location>| loc.map(|l| format!("{}:{}", l.line, l.column));
        match (at(self.before), at(self.after)) {
            (Some(b), Some(a)) => write!(f, " (before {b}, after {a})"),
            (Some(b), None) => write!(f, " (before {b})"),
            (None, Some(a)) => write!(f, " (after {a})"),
            (None, None) => Ok(()),
        }
    }
}

impl std::error::Error for Difference {}

/// Returns the first difference between `before` and `after`, or `Ok` if a
/// formatter could have produced `after` from `before`.
pub fn check_equivalent(before: &str, after: &str) -> Result<(), Difference> {
    check_equivalent_trees(&parse(before), &parse(after))
}

/// `check_equivalent` for texts already parsed.
pub fn check_equivalent_trees(before: &Tree, after: &Tree) -> Result<(), Difference> {
    check_errors(before, after)?;
    check_code(before, after)?;
    check_comments(before, after)
}

fn location(parse: &Tree, offset: TextSize) -> Location {
    let (line, column) = parse.line_col(offset);
    Location {
        offset,
        line,
        column,
    }
}

fn check_errors(before: &Tree, after: &Tree) -> Result<(), Difference> {
    match after.errors().first() {
        Some(error) if before.errors().is_empty() => Err(Difference {
            kind: DifferenceKind::NewSyntaxError,
            message: format!("the rewrite has a syntax error: {error}"),
            before: None,
            after: Some(location(after, error.range().start())),
        }),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Code shape.

/// A node's kind in a dialect's new syntax: an `=>` rename is an `as` one,
/// and a block or cases in braces are indented ones.
fn new_kind(kind: SyntaxKind) -> SyntaxKind {
    match kind {
        SyntaxKind::ArrowRenamedIdentifier => SyntaxKind::AsRenamedIdentifier,
        SyntaxKind::Block => SyntaxKind::IndentedBlock,
        SyntaxKind::CaseBlock => SyntaxKind::IndentedCases,
        kind => kind,
    }
}

/// A node's kind in a dialect's new syntax, as `new_kind`, and an
/// intersection type's, `A & B`, as a compound type's, `A with B`.
fn node_kind(node: &SyntaxNode<'_>) -> SyntaxKind {
    if is_and_type(node) {
        return SyntaxKind::CompoundType;
    }
    new_kind(node.kind())
}

/// An intersection type, `A & B`: an infix type whose operator is `&`.
/// Not one right after a pattern's `:` or `given`, which Scala 3 reads as a
/// pattern of its own: `case x: A & B` is `x: A` and `B`.
fn is_and_type(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::InfixType
        && node
            .child_by_field_name("operator")
            .is_some_and(|op| op.text() == "&")
        && !node.parent().is_some_and(|p| is_type_pattern(&p))
}

/// A pattern with a type: `x: T` or `given T`.
fn is_type_pattern(node: &SyntaxNode<'_>) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::TypedPattern | SyntaxKind::GivenPattern
    )
}

/// Parentheses around a compound or intersection type in a pattern, as in
/// `case x: (A & B)`, which `A with B` may go without: they mean nothing.
fn is_pattern_type_parens(node: &SyntaxNode<'_>) -> bool {
    if node.kind() != SyntaxKind::TupleType
        || node.field_name() != Some("type")
        || !node.parent().is_some_and(|p| is_type_pattern(&p))
    {
        return false;
    }
    let mut children = node.non_trivia_children();
    matches!(
        (children.next(), children.next(), children.next(), children.next()),
        (Some(_), Some(NodeOrToken::Node(inner)), Some(_), None)
            if is_and_type(&inner)
                || inner.kind() == SyntaxKind::CompoundType
                    && inner
                        .children_with_tokens()
                        .any(|c| c.as_token().is_some_and(|t| t.text() == "with"))
                    && !inner.children().any(|c| c.kind() == SyntaxKind::Refinement)
    )
}

/// Parentheses around a boolean chain, as in `(a && b)`, which mean
/// nothing: the chain is one node either way.
fn is_chain_parens(node: &SyntaxNode<'_>) -> bool {
    if node.kind() != SyntaxKind::ParenthesizedExpression {
        return false;
    }
    let mut children = node.non_trivia_children();
    matches!(
        (children.next(), children.next(), children.next(), children.next()),
        (
            Some(NodeOrToken::Token(open)),
            Some(NodeOrToken::Node(inner)),
            Some(NodeOrToken::Token(close)),
            None,
        ) if open.text() == "(" && close.text() == ")"
            && inner.kind() == SyntaxKind::InfixExpression
            && inner
                .non_trivia_children()
                .find(|c| c.field_name() == Some("operator"))
                .and_then(|o| o.as_token().map(|t| matches!(t.text(), "&&" | "||")))
                .unwrap_or(false)
    )
}

/// The left part of an intersection type that is itself one, as `A & B` in
/// `A & B & C`, which counts as part of the outer one: `A with B with C` is
/// one compound type.
fn is_nested_and_type(node: &SyntaxNode<'_>) -> bool {
    is_and_type(node)
        && node.field_name() == Some("left")
        && node.parent().is_some_and(|p| is_and_type(&p))
}

/// Whether `token` is the `with` of a compound type or the `&` of an
/// intersection one, which mean the same. Not the `with` of `new A with B`,
/// which makes a class.
fn is_and_type_operator(token: &SyntaxToken<'_>) -> bool {
    match token.text() {
        "with" => token.parent().is_some_and(|p| {
            p.kind() == SyntaxKind::CompoundType
                && !p
                    .parent()
                    .is_some_and(|q| q.kind() == SyntaxKind::InstanceExpression)
        }),
        "&" => token.parent().is_some_and(|p| is_and_type(&p)),
        _ => false,
    }
}

/// Whether `token` is the `with` or `,` between two parents in an
/// `extends` clause, which mean the same.
pub(crate) fn is_extends_separator(token: &SyntaxToken<'_>) -> bool {
    matches!(token.text(), "with" | ",")
        && token
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::ExtendsClause)
}

/// A token's text in a dialect's new syntax: `*` for an import's `_`, `as`
/// for a rename's `=>`, `?` for a type wildcard's `_`. Its kind changes with
/// its text, so the text stands for both.
fn new_token<'t>(token: &SyntaxToken<'t>) -> (Option<SyntaxKind>, &'t str) {
    if is_type_wildcard_token(token) {
        return (None, "?");
    }
    if is_implicit_clause_keyword(token) {
        return (None, "using");
    }
    if is_and_type_operator(token) {
        return (None, "&");
    }
    if is_extends_separator(token) {
        return (None, ",");
    }
    let parent = token.parent().map(|p| p.kind());
    match (parent, token.text()) {
        (Some(SyntaxKind::NamespaceWildcard), "_") => (None, "*"),
        (Some(SyntaxKind::ArrowRenamedIdentifier), "=>" | "\u{21D2}") => (None, "as"),
        (Some(SyntaxKind::NamespaceWildcard), "*")
        | (Some(SyntaxKind::AsRenamedIdentifier), "as") => (None, token.text()),
        (_, text) => (Some(token.kind()), text),
    }
}

/// Whether `token` is the `implicit` or `using` that starts a parameter
/// clause, where one means the same as the other: not where a class
/// parameter is a `val` or `var` or has modifiers, since a `using` one is a
/// `given` member, which `import a.*` leaves out.
pub(crate) fn is_implicit_clause_keyword(token: &SyntaxToken<'_>) -> bool {
    if !matches!(token.text(), "implicit" | "using") {
        return false;
    }
    let Some(clause) = token.parent() else {
        return false;
    };
    match clause.kind() {
        SyntaxKind::Parameters => true,
        SyntaxKind::ClassParameters => !clause.children().any(|parameter| {
            parameter.children_with_tokens().any(|part| match part {
                NodeOrToken::Node(node) => node.kind() == SyntaxKind::Modifiers,
                NodeOrToken::Token(token) => matches!(token.text(), "val" | "var"),
            })
        }),
        _ => false,
    }
}

/// Whether `element` is where a type wildcard goes: a type argument, as in
/// `List[_]`, or what a bound limits in one, as in `List[_ <: A]` or
/// `List[_ >: A <: B]`. Type parameters, as in `def f[F[_]]`, are not.
pub(crate) fn is_type_wildcard_place(element: SyntaxElement<'_>) -> bool {
    let mut element = element;
    loop {
        let Some(parent) = element.parent() else {
            return false;
        };
        match parent.kind() {
            SyntaxKind::TypeArguments => return true,
            SyntaxKind::InfixType
                if element.field_name() == Some("left")
                    && parent
                        .child_by_field_name("operator")
                        .is_some_and(|op| matches!(op.text(), "<:" | ">:")) =>
            {
                element = NodeOrToken::Node(parent);
            }
            _ => return false,
        }
    }
}

/// A type wildcard written `_`, a node around its token.
fn is_type_wildcard_node(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::Wildcard && is_type_wildcard_place(NodeOrToken::Node(*node))
}

/// The token of a type wildcard, `_` or `?`.
fn is_type_wildcard_token(token: &SyntaxToken<'_>) -> bool {
    match token.text() {
        "_" => token.parent().is_some_and(|p| is_type_wildcard_node(&p)),
        "?" => {
            token.kind() == SyntaxKind::TypeIdentifier
                && is_type_wildcard_place(NodeOrToken::Token(*token))
        }
        _ => false,
    }
}

/// An element's field, or, where the node around it is skipped, the
/// node's: `c` in `if (c) a` is the condition, as in `if c then a`.
fn field(element: SyntaxElement<'_>) -> Option<&'static str> {
    match element.parent() {
        // An intersection type's parts are a compound type's: the first
        // is its base, the others extras, and the operator has no field.
        Some(parent) if is_and_type(&parent) => match element.field_name() {
            Some("left") => Some("base"),
            Some("right") => Some("extra"),
            _ => None,
        },
        // A colon argument's lambda parameters are a lambda's, and its
        // body or cases are the call's arguments.
        Some(parent) if parent.kind() == SyntaxKind::ColonArgument => match element.field_name() {
            Some("lambda_start") => match element.as_token() {
                Some(t) if matches!(t.text(), "=>" | "?=>" | "\u{21D2}") => None,
                _ => Some("parameters"),
            },
            _ if is_colon_lambda(&parent) => element.field_name(),
            _ => parent.field_name(),
        },
        // Parentheses around a chain may be inside another node that's
        // skipped, as a block around one expression.
        Some(parent) if is_chain_parens(&parent) => field(NodeOrToken::Node(parent)),
        Some(parent)
            if is_condition_parens(&parent)
                || is_transparent_block(&parent)
                || is_case_body_block(&parent)
                || is_type_wildcard_node(&parent)
                || is_old_splice_node(&parent)
                || is_pattern_type_parens(&parent) =>
        {
            parent.field_name()
        }
        _ => element.field_name(),
    }
}

/// One step of a walk over the code, with trivia left out.
#[derive(Clone, Copy)]
enum Step<'t> {
    Enter(SyntaxNode<'t>),
    Leave(SyntaxNode<'t>),
    Token(SyntaxToken<'t>),
}

impl Step<'_> {
    fn same_as(&self, other: &Step<'_>) -> bool {
        match (self, other) {
            (Step::Enter(a), Step::Enter(b)) => {
                node_kind(a) == node_kind(b)
                    && field(NodeOrToken::Node(*a)) == field(NodeOrToken::Node(*b))
            }
            (Step::Leave(a), Step::Leave(b)) => node_kind(a) == node_kind(b),
            (Step::Token(a), Step::Token(b)) => {
                new_token(a) == new_token(b)
                    && a.is_missing() == b.is_missing()
                    && field(NodeOrToken::Token(*a)) == field(NodeOrToken::Token(*b))
            }
            _ => false,
        }
    }

    fn offset(&self) -> TextSize {
        match self {
            Step::Enter(node) => node.text_range().start(),
            Step::Leave(node) => node.text_range().end(),
            Step::Token(token) => token.text_range().start(),
        }
    }

    fn describe(&self) -> String {
        let field = |name: Option<&str>| name.map_or(String::new(), |n| format!("{n}: "));
        match self {
            Step::Enter(node) => format!("{}{}", field(node.field_name()), node.kind()),
            Step::Leave(node) => format!("end of {}", node.kind()),
            Step::Token(token) if token.is_missing() => format!("missing {:?}", token.kind()),
            Step::Token(token) if token.kind().is_named() => {
                format!(
                    "{}{} {:?}",
                    field(token.field_name()),
                    token.kind(),
                    token.text()
                )
            }
            Step::Token(token) => format!("{}{:?}", field(token.field_name()), token.text()),
        }
    }
}

fn code_steps(root: SyntaxNode<'_>) -> impl Iterator<Item = Step<'_>> {
    root.preorder_with_tokens().filter_map(|event| match event {
        WalkEvent::Enter(element) | WalkEvent::Leave(element) if element.is_trivia() => None,
        WalkEvent::Enter(NodeOrToken::Node(node)) | WalkEvent::Leave(NodeOrToken::Node(node))
            if is_transparent_block(&node)
                || is_lone_rename_braces(&node)
                || is_type_wildcard_node(&node)
                || is_old_splice_node(&node)
                || is_condition_parens(&node)
                || is_case_body_block(&node)
                || is_argument_wrapper(&node)
                || is_lone_catch_case(&node)
                || is_nested_and_type(&node)
                || is_pattern_type_parens(&node)
                || is_chain_parens(&node)
                || is_optional_end_marker(&node) =>
        {
            None
        }
        WalkEvent::Enter(NodeOrToken::Token(token)) if may_come_or_go(&token) => None,
        WalkEvent::Enter(NodeOrToken::Node(node)) => Some(Step::Enter(node)),
        WalkEvent::Leave(NodeOrToken::Node(node)) => Some(Step::Leave(node)),
        WalkEvent::Enter(NodeOrToken::Token(token)) => Some(Step::Token(token)),
        WalkEvent::Leave(NodeOrToken::Token(_)) => None,
    })
}

/// A token that means nothing, so it may come or go: a trailing comma,
/// braces around a lone rename, `: _` or `@ _` in a vararg splice, and an
/// end marker's.
fn may_come_or_go(token: &SyntaxToken<'_>) -> bool {
    is_trailing_comma(token)
        || token.parent().is_some_and(|p| is_optional_end_marker(&p))
        || is_lone_rename_brace(token)
        || is_old_splice_token(token)
        || (matches!(token.text(), "(" | ")")
            && token
                .parent()
                .is_some_and(|p| is_pattern_type_parens(&p) || is_chain_parens(&p)))
        || is_control_token(token)
        || is_optional_brace(token)
        || (token.text() == ":"
            && token
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::CallExpression)
            && std::iter::successors(token.next_sibling_or_token(), |e| e.next_sibling_or_token())
                .find(|e| !e.is_trivia())
                .is_some_and(|e| e.kind() == SyntaxKind::ColonArgument))
}

/// An end marker that names what tree-sitter-scala attached it to, as
/// `end f` in `def f =`. One attached to something else, as an `end if`
/// read as ending an outer `if`, counts: the code would mean something
/// else.
fn is_optional_end_marker(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::EndMarker && node.end_marker_matches_parent()
}

/// A `case` clause's body in braces, as in `case a => { b; c }`, which
/// means the same as its statements without them: `case a => b; c`.
fn is_case_body_block(node: &SyntaxNode<'_>) -> bool {
    matches!(node.kind(), SyntaxKind::Block | SyntaxKind::IndentedBlock)
        && node.field_name() == Some("body")
        && node.parent().is_some_and(|p| {
            // Or a `catch case` on one line's.
            matches!(p.kind(), SyntaxKind::CaseClause | SyntaxKind::CatchClause)
        })
}

/// A brace that Scala 3's indentation can stand for: around a block, cases,
/// or a class body, and the `:` that opens a class body without them.
/// `def f = { a }` means `def f =` with `a` indented below, and
/// `class A { ... }` means `class A:` with the body indented.
fn is_optional_brace(token: &SyntaxToken<'_>) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    match token.text() {
        "{" | "}" => matches!(
            parent.kind(),
            SyntaxKind::Block
                | SyntaxKind::CaseBlock
                | SyntaxKind::TemplateBody
                | SyntaxKind::EnumBody
        ),
        ":" => {
            matches!(
                parent.kind(),
                SyntaxKind::TemplateBody | SyntaxKind::EnumBody
            ) && parent.first_token().is_some_and(|t| t == *token)
        }
        _ => false,
    }
}

/// The parentheses around an `if` or `while` condition, as in `if (c) a`,
/// which Scala 3 may leave out: `if c then a`.
fn is_condition_parens(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::ParenthesizedExpression
        && node.field_name() == Some("condition")
        && node.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                SyntaxKind::IfExpression | SyntaxKind::WhileExpression
            )
        })
}

/// What tells a control expression's parts apart, in either syntax: the
/// parentheses around a condition and the `then` or `do` after it, and the
/// parentheses or braces around a `for`'s enumerators and the `do` after
/// them. `if (c) a` means `if c then a`, and `for (x <- xs) f(x)` means
/// `for x <- xs do f(x)`.
fn is_control_token(token: &SyntaxToken<'_>) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    match (parent.kind(), token.text()) {
        (SyntaxKind::ParenthesizedExpression, "(" | ")") => is_condition_parens(&parent),
        (SyntaxKind::IfExpression, "then") | (SyntaxKind::WhileExpression, "do") => {
            token.field_name() == Some("condition")
        }
        (SyntaxKind::ForExpression, "(" | ")" | "{" | "}") => {
            token.field_name() == Some("enumerators")
        }
        (SyntaxKind::ForExpression, "do") => true,
        _ => false,
    }
}

/// What Scala 3 drops from a vararg splice: the wildcard in `xs: _*`, and in
/// a pattern `xs @ _*`'s binding and wildcard, which leave `xs*`. A lone
/// `_*` stays.
fn is_old_splice_node(node: &SyntaxNode<'_>) -> bool {
    let parent = node.parent();
    let parent_kind = parent.map(|p| p.kind());
    match node.kind() {
        SyntaxKind::Wildcard => {
            parent_kind == Some(SyntaxKind::Vararg)
                || parent.is_some_and(|p| is_old_splice_node(&p))
        }
        SyntaxKind::CapturePattern => {
            parent_kind == Some(SyntaxKind::RepeatPattern)
                && node
                    .child_by_field_name("pattern")
                    .is_some_and(|p| p.kind() == SyntaxKind::Wildcard)
        }
        _ => false,
    }
}

/// The tokens of what `is_old_splice_node` drops: `:`, `@` and `_`.
fn is_old_splice_token(token: &SyntaxToken<'_>) -> bool {
    let Some(parent) = token.parent() else {
        return false;
    };
    match token.text() {
        ":" => parent.kind() == SyntaxKind::Vararg,
        "@" | "_" => is_old_splice_node(&parent),
        _ => false,
    }
}

/// Braces around a lone rename in an import or export, as in
/// `import a.{b => c}`, which mean the same as `import a.b as c`.
fn is_lone_rename_braces(node: &SyntaxNode<'_>) -> bool {
    if node.kind() != SyntaxKind::NamespaceSelectors {
        return false;
    }
    let mut children = node.non_trivia_children();
    matches!(
        (children.next(), children.next(), children.next(), children.next()),
        (Some(_), Some(rename), Some(_), None)
            if matches!(
                rename.kind(),
                SyntaxKind::ArrowRenamedIdentifier | SyntaxKind::AsRenamedIdentifier
            )
    )
}

/// A lambda body, an `if`'s `then` or `else` body, a `try` or `finally`
/// body, or a definition's body after `=`, that is a block holding one
/// expression means the same as the expression alone: `x =>` with `a + 1`
/// on the next line is `x => a + 1`, `if c then` with `a` on the next line
/// is `if c then a`, and `def f =` with `a` on the next line is
/// `def f = a`. tree-sitter-scala wraps only the first
/// in a block, so the block is skipped, which lets a formatter move a body
/// to its own line or back.
fn is_transparent_block(node: &SyntaxNode<'_>) -> bool {
    let if_body = node.parent().is_some_and(|p| match p.kind() {
        SyntaxKind::IfExpression => {
            matches!(node.field_name(), Some("consequence" | "alternative"))
        }
        SyntaxKind::TryExpression => node.field_name() == Some("body"),
        SyntaxKind::FinallyClause => true,
        _ => false,
    });
    let lambda_body = node.kind() == SyntaxKind::IndentedBlock
        && node
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::LambdaExpression || is_colon_lambda(&p));
    let definition_body = node.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            SyntaxKind::FunctionDefinition
                | SyntaxKind::GivenDefinition
                | SyntaxKind::ValDefinition
                | SyntaxKind::VarDefinition
        )
    });
    let block = matches!(node.kind(), SyntaxKind::IndentedBlock | SyntaxKind::Block);
    if !(block && (if_body || definition_body)) && !lambda_body {
        return false;
    }
    // Its statements: not a block's braces.
    let mut children = node
        .non_trivia_children()
        .filter(|c| !matches!(c.as_token().map(|t| t.text()), Some("{" | "}")));
    match (children.next(), children.next()) {
        (Some(child), None) => {
            let name = child.kind().name();
            !(name.ends_with("_definition") || name.ends_with("_declaration"))
        }
        _ => false,
    }
}

/// What holds a `catch`'s only `case`, its cases and the clause, which
/// `catch case e => b` on one line does without.
fn is_lone_catch_case(node: &SyntaxNode<'_>) -> bool {
    let lone_cases = |cases: &SyntaxNode<'_>| {
        matches!(
            cases.kind(),
            SyntaxKind::IndentedCases | SyntaxKind::CaseBlock
        ) && cases
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::CatchClause)
            && cases
                .children()
                .filter(|c| c.kind() == SyntaxKind::CaseClause)
                .count()
                == 1
    };
    match node.kind() {
        SyntaxKind::CaseClause => node.parent().is_some_and(|p| lone_cases(&p)),
        _ => lone_cases(node),
    }
}

/// A colon argument that is a lambda, as `xs.map: x =>`.
fn is_colon_lambda(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::ColonArgument
        && node
            .children_with_tokens()
            .any(|c| c.field_name() == Some("lambda_start"))
}

/// What a braced argument and a colon argument differ by, besides their
/// braces and colon: the colon argument itself, and a braced argument
/// that's only a lambda, with that lambda. `xs.map { x => a }` means
/// `xs.map: x =>` with `a` indented below.
fn is_argument_wrapper(node: &SyntaxNode<'_>) -> bool {
    match node.kind() {
        SyntaxKind::ColonArgument => true,
        SyntaxKind::Block => is_lambda_argument(node),
        SyntaxKind::LambdaExpression => node.parent().is_some_and(|p| is_lambda_argument(&p)),
        _ => false,
    }
}

/// A braced argument holding only a lambda.
fn is_lambda_argument(node: &SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::Block && node.field_name() == Some("arguments") && {
        let mut statements = node.non_trivia_children().filter(|c| c.kind().is_named());
        matches!(
            (statements.next(), statements.next()),
            (Some(only), None) if only.kind() == SyntaxKind::LambdaExpression
        )
    }
}

/// A comma that may come or go: followed by a line break and then `)` or
/// `]`, with no comments to keep with it.
fn is_trailing_comma(token: &SyntaxToken<'_>) -> bool {
    if token.text() != ","
        || token
            .leading_trivia()
            .chain(token.trailing_trivia())
            .any(|t| t.is_comment())
    {
        return false;
    }
    let Some(next) = std::iter::successors(token.next_token(), SyntaxToken::next_token)
        .find(|t| !t.is_trivia() && !t.is_missing())
    else {
        return false;
    };
    matches!(next.text(), ")" | "]")
        && next
            .leading_trivia()
            .any(|t| t.kind() == TriviaKind::Newline)
        && !next.leading_trivia().any(|t| t.is_comment())
}

fn check_code(before: &Tree, after: &Tree) -> Result<(), Difference> {
    let mut b = code_steps(before.syntax());
    let mut a = code_steps(after.syntax());
    loop {
        let (x, y) = (b.next(), a.next());
        let message = match (&x, &y) {
            (None, None) => return Ok(()),
            (Some(x), Some(y)) if x.same_as(y) => continue,
            (Some(x), Some(y)) => format!("expected {}, found {}", x.describe(), y.describe()),
            (Some(x), None) => format!("expected {}, found the end", x.describe()),
            (None, Some(y)) => format!("expected the end, found {}", y.describe()),
        };
        return Err(Difference {
            kind: DifferenceKind::Code,
            message,
            before: x.map(|s| location(before, s.offset())),
            after: y.map(|s| location(after, s.offset())),
        });
    }
}

// ---------------------------------------------------------------------------
// Comments.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Placement {
    Leading,
    Trailing,
    EndOfFile,
}

struct Comment<'t> {
    trivia: SyntaxTrivia<'t>,
    placement: Placement,
    /// Position of the owner among the code tokens, and its text. The code
    /// already matched, so the same index means the same token.
    owner: Option<(usize, &'t str)>,
}

impl Comment<'_> {
    fn describe_place(&self) -> String {
        match (self.placement, self.owner) {
            (Placement::Leading, Some((_, text))) => format!("before {text:?}"),
            (Placement::Trailing, Some((_, text))) => format!("after {text:?}"),
            _ => "at the end of the file".to_string(),
        }
    }
}

fn comments(parse: &Tree) -> Vec<Comment<'_>> {
    let mut out = Vec::new();
    // Tokens that may come or go don't own comments: a comment before one
    // goes with the next token, a comment after it with the one before.
    let mut index = 0;
    let mut previous = None;
    let mut before_next = Vec::new();
    for token in parse.syntax().tokens().filter(SyntaxToken::owns_trivia) {
        if may_come_or_go(&token) {
            before_next.extend(token.leading_trivia().filter(SyntaxTrivia::is_comment));
            for trivia in token.trailing_trivia().filter(SyntaxTrivia::is_comment) {
                out.push(Comment {
                    trivia,
                    placement: Placement::Trailing,
                    owner: previous,
                });
            }
            continue;
        }
        let owner = Some((index, new_token(&token).1));
        let leading: Vec<_> = before_next
            .drain(..)
            .chain(token.leading_trivia().filter(SyntaxTrivia::is_comment))
            .collect();
        for trivia in leading {
            out.push(Comment {
                trivia,
                placement: Placement::Leading,
                owner,
            });
        }
        for trivia in token.trailing_trivia().filter(SyntaxTrivia::is_comment) {
            out.push(Comment {
                trivia,
                placement: Placement::Trailing,
                owner,
            });
        }
        previous = owner;
        index += 1;
    }
    for trivia in before_next
        .into_iter()
        .chain(parse.eof_trivia().filter(SyntaxTrivia::is_comment))
    {
        out.push(Comment {
            trivia,
            placement: Placement::EndOfFile,
            owner: None,
        });
    }
    out
}

/// A `{` or `}` around a lone rename, which may come or go.
fn is_lone_rename_brace(token: &SyntaxToken<'_>) -> bool {
    matches!(token.text(), "{" | "}") && token.parent().is_some_and(|p| is_lone_rename_braces(&p))
}

/// Whether two comments say the same. The indentation of a comment's lines
/// after the first carries no meaning, so a formatter may move them with
/// the code: a Scaladoc's ` *` lines follow its `/**`. Nor does a
/// Scaladoc's gutter: where its `*`s go, the gap after them, or whether its
/// text starts on `/**`'s line.
fn same_comment(a: &str, b: &str) -> bool {
    fn lines(s: &str) -> impl Iterator<Item = &str> {
        s.lines().enumerate().map(|(i, l)| {
            if i == 0 {
                l
            } else {
                l.trim_start_matches([' ', '\t'])
            }
        })
    }
    a == b
        || lines(a).eq(lines(b))
        || matches!((scaladoc_lines(a), scaladoc_lines(b)), (Some(x), Some(y)) if x == y)
}

fn check_comments(before: &Tree, after: &Tree) -> Result<(), Difference> {
    let b = comments(before);
    let a = comments(after);
    for i in 0..b.len().max(a.len()) {
        let (x, y) = (b.get(i), a.get(i));
        let message = match (x, y) {
            (Some(x), Some(y)) if !same_comment(x.trivia.text(), y.trivia.text()) => {
                format!("comment {:?} became {:?}", x.trivia.text(), y.trivia.text())
            }
            (Some(x), Some(y))
                if x.placement != y.placement || x.owner.map(|o| o.0) != y.owner.map(|o| o.0) =>
            {
                format!(
                    "comment {:?} moved from {} to {}",
                    x.trivia.text(),
                    x.describe_place(),
                    y.describe_place()
                )
            }
            (Some(_), Some(_)) => continue,
            (Some(x), None) => format!("comment {:?} is missing", x.trivia.text()),
            (None, Some(y)) => format!("comment {:?} was added", y.trivia.text()),
            (None, None) => unreachable!("i is below the longer length"),
        };
        return Err(Difference {
            kind: DifferenceKind::Comment,
            message,
            before: x.map(|c| location(before, c.trivia.text_range().start())),
            after: y.map(|c| location(after, c.trivia.text_range().start())),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn differs(before: &str, after: &str) -> Difference {
        check_equivalent(before, after).expect_err("should differ")
    }

    #[test]
    fn new_import_syntax_means_the_same() {
        let old = "import a._\nimport a.{b => c, d => _, _}\nexport e._\n";
        let new = "import a.*\nimport a.{b as c, d as _, *}\nexport e.*\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
    }

    #[test]
    fn question_mark_type_wildcards_mean_the_same() {
        let old = "val a: Map[_, _ <: A] = b /* c */\nval d: List[_ >: A <: B] = e\n";
        let new = "val a: Map[?, ? <: A] = b /* c */\nval d: List[? >: A <: B] = e\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // Only where a type wildcard goes.
        differs("def f[F[_]] = 1\n", "def f[F[?]] = 1\n");
        differs("val a: List[_ + A] = b\n", "val a: List[? + A] = b\n");
        differs("val a: List[A] = b\n", "val a: List[?] = b\n");
    }

    #[test]
    fn postfix_vararg_splices_mean_the_same() {
        let old = "f(xs: _*)\nx match { case Seq(a, xs @ _*) => }\n";
        let new = "f(xs*)\nx match { case Seq(a, xs*) => }\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // A comment after a dropped token goes with the one before.
        assert_eq!(
            check_equivalent("f(xs: /* c */ _*)\n", "f(xs /* c */ *)\n"),
            Ok(())
        );
        differs(
            "x match { case Seq(_*) => }\n",
            "x match { case Seq(xs*) => }\n",
        );
        differs(
            "x match { case Seq(xs @ _*) => }\n",
            "x match { case Seq(ys*) => }\n",
        );
        differs(
            "x match { case Seq(xs @ y*) => }\n",
            "x match { case Seq(xs*) => }\n",
        );
    }

    #[test]
    fn using_clauses_mean_implicit_ones() {
        let old = "def f(x: A)(implicit /* c */ ec: EC, b: B) = 1\nclass C(implicit d: D)\n";
        let new = "def f(x: A)(using /* c */ ec: EC, b: B) = 1\nclass C(using d: D)\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // A `using` class parameter that is a member is a `given` one.
        differs("class C(implicit val d: D)\n", "class C(using val d: D)\n");
        differs(
            "class C(implicit private val d: D)\n",
            "class C(using private val d: D)\n",
        );
        // Only the keyword.
        differs("def f(implicit ec: EC) = 1\n", "def f(using ed: EC) = 1\n");
        differs("def f(ec: EC) = 1\n", "def f(using ec: EC) = 1\n");
    }

    #[test]
    fn intersection_types_mean_compound_ones() {
        let old = "val x: A with B /* c */ with C = y\ndef f(x: F[A with B]): A with B\n";
        let new = "val x: A & B /* c */ & C = y\ndef f(x: F[A & B]): A & B\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // Grouped differently.
        differs("type T = A with (B with C)\n", "type T = A & B & C\n");
        differs("type T = (A with B) with C\n", "type T = A & B & C\n");
        // A class, not a type.
        differs("val a = new A with B\n", "val a = new A & B\n");
        // In a pattern, `&` needs parentheses.
        let old = "x match { case y: A with B => 1; case given A with B => 2 }\n";
        let new = "x match { case y: (A & B) => 1; case given (A & B) => 2 }\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        assert_eq!(
            check_equivalent(
                "x match { case y: (A with B) => 1 }\n",
                "x match { case y: A with B => 1 }\n"
            ),
            Ok(())
        );
        differs(
            "x match { case y: A with B => 1 }\n",
            "x match { case y: A & B => 1 }\n",
        );
        differs(
            "x match { case y: (A & B) => 1 }\n",
            "x match { case y: A & B => 1 }\n",
        );
        // Other operators and parts.
        differs("type T = A with B\n", "type T = A | B\n");
        differs("type T = A with B\n", "type T = A & C\n");
    }

    #[test]
    fn commas_in_extends_mean_with() {
        let old = "class C(x: X) extends A(x) with B[Y] /* c */ with\n  D\n";
        let new = "class C(x: X) extends A(x), B[Y] /* c */,\n  D\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // Only between parents.
        differs("class C extends A with B\n", "class C extends A, D\n");
        differs("val a = new A with B\n", "val a = new A, B\n");
        differs("class C extends A[X with Y]\n", "class C extends A[X, Y]\n");
    }

    #[test]
    fn parentheses_around_a_boolean_chain_mean_nothing() {
        assert_eq!(
            check_equivalent("val x = a && b\n", "val x = (a\n  && b)\n"),
            Ok(())
        );
        assert_eq!(
            check_equivalent("if (a || b) c\n", "if (a\n  || b) c\n"),
            Ok(())
        );
        // Still a different grouping.
        assert!(check_equivalent("val x = (a || b) && c\n", "val x = a || b && c\n").is_err());
        assert!(check_equivalent("val x = !(a && b)\n", "val x = !a && b\n").is_err());
        // Only around a chain of `&&` or `||`.
        assert!(check_equivalent("val x = (a + b)\n", "val x = a + b\n").is_err());
    }

    #[test]
    fn comments_stay_on_a_rewritten_token() {
        assert_eq!(
            check_equivalent(
                "val a: List[_ /* c */] = b\n",
                "val a: List[? /* c */] = b\n"
            ),
            Ok(())
        );
    }

    #[test]
    fn control_syntax_means_the_same() {
        let old = "if (c) a else b\nwhile (c) x\nfor (x <- xs) f(x)\nfor {\n  x <- xs\n} yield x\n";
        let new =
            "if c then a else b\nwhile c do x\nfor x <- xs do f(x)\nfor\n  x <- xs\nyield x\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        assert_eq!(check_equivalent("if (c) then a\n", "if c then a\n"), Ok(()));
        // Parentheses inside the condition still count.
        differs("if ((a)) b\n", "if (a) b\n");
        differs("if ((a)) b\n", "if a then b\n");
        differs("if (a) + b then c\n", "if a + b then c\n");
    }

    #[test]
    fn braces_mean_the_same_as_indentation() {
        let old = "class A {\n  def f = {\n    a\n    b\n  }\n  x match {\n    case a => {\n      b\n      c\n    }\n  }\n}\n";
        let new = "class A:\n  def f =\n    a\n    b\n  x match\n    case a =>\n      b\n      c\n";
        assert_eq!(check_equivalent(old, new), Ok(()));
        assert_eq!(check_equivalent(new, old), Ok(()));
        // A comment before `}` goes with the next token in both.
        assert_eq!(
            check_equivalent(
                "def f = {\n  a\n  // c\n}\nval y = 1\n",
                "def f =\n  a\n  // c\nval y = 1\n"
            ),
            Ok(())
        );
        // Statements that move out of the block don't.
        differs("def f = {\n  a\n  b\n}\n", "def f =\n  a\nb\n");
    }

    #[test]
    fn colon_arguments_mean_braced_ones() {
        for (braced, colon) in [
            ("xs.map { x =>\n  a\n  b\n}\n", "xs.map: x =>\n  a\n  b\n"),
            ("xs.map { x =>\n  a\n}\n", "xs.map: x =>\n  a\n"),
            ("xs.map { (x, y) =>\n  a\n}\n", "xs.map: (x, y) =>\n  a\n"),
            ("xs.foreach {\n  a\n  b\n}\n", "xs.foreach:\n  a\n  b\n"),
            (
                "xs.collect {\n  case a => b\n}\n",
                "xs.collect:\n  case a => b\n",
            ),
            ("f(a) { x =>\n  b\n}\n", "f(a): x =>\n  b\n"),
        ] {
            assert_eq!(check_equivalent(braced, colon), Ok(()), "{braced}");
            assert_eq!(check_equivalent(colon, braced), Ok(()), "{colon}");
        }
        differs("xs.map { x =>\n  a\n  b\n}\n", "xs.map: y =>\n  a\n  b\n");
        differs("xs.foreach {\n  a\n  b\n}\n", "xs.foreach:\n  a\nb\n");
    }

    #[test]
    fn an_if_body_of_one_expression_means_the_expression() {
        let braced = "if (c) {\n  a\n} else {\n  b\n}\n";
        let indented = "if c then\n  a\nelse\n  b\n";
        let joined = "if c then a\nelse b\n";
        assert_eq!(check_equivalent(braced, joined), Ok(()));
        assert_eq!(check_equivalent(indented, joined), Ok(()));
        assert_eq!(check_equivalent(joined, braced), Ok(()));
        // Not a definition, which a block scopes.
        differs("if (c) {\n  val a = 1\n}\n", "if c then val a = 1\n");
    }

    #[test]
    fn try_bodies_and_a_lone_catch_case_mean_the_same_hugged() {
        let braced = "try {\n  a\n} catch {\n  case e: E => b\n} finally {\n  c\n}\n";
        let indented = "try\n  a\ncatch\n  case e: E => b\nfinally\n  c\n";
        let hugged = "try a\ncatch case e: E => b\nfinally c\n";
        assert_eq!(check_equivalent(braced, hugged), Ok(()));
        assert_eq!(check_equivalent(indented, hugged), Ok(()));
        assert_eq!(check_equivalent(hugged, braced), Ok(()));
        assert_eq!(
            check_equivalent(
                "try a\ncatch { case e: E => b }\n",
                "try a\ncatch case e: E => b\n"
            ),
            Ok(())
        );
        differs(
            "try a\ncatch case e: E => b\n",
            "try a\ncatch case f: E => b\n",
        );
    }

    #[test]
    fn braces_around_a_lone_rename_mean_nothing() {
        assert_eq!(
            check_equivalent("import a.b as c\n", "import a.{b => c}\n"),
            Ok(())
        );
        assert_eq!(
            check_equivalent("import a.{b => c}\n", "import a.b as c\n"),
            Ok(())
        );
        differs("import a.{b => c, d}\n", "import a.b as c\n");
    }

    #[test]
    fn comments_stay_with_their_token_when_braces_come() {
        assert_eq!(
            check_equivalent(
                "import a.b as c // x\nval y = 1 // y\n",
                "import a.{b => c} // x\nval y = 1 // y\n"
            ),
            Ok(())
        );
    }

    #[test]
    fn new_import_syntax_still_compares_names() {
        differs("import a._\n", "import b.*\n");
        differs("import a.{b => c}\n", "import a.{b as d}\n");
        // Only in imports: a `*` elsewhere is a different name.
        differs("val x = a _\n", "val x = a *\n");
    }

    #[test]
    fn an_end_marker_may_come_or_go() {
        let without = "object A:\n  def f =\n    a\n    b\n  def g = 1\n";
        let with = "object A:\n  def f =\n    a\n    b\n  end f\n  def g = 1\nend A\n";
        assert_eq!(check_equivalent(without, with), Ok(()));
        assert_eq!(check_equivalent(with, without), Ok(()));
        // Moving the statements after it into the region does not.
        let moved = "object A:\n  def f =\n    a\n    b\n    def g = 1\n  end f\n";
        assert!(check_equivalent(without, moved).is_err());
    }

    #[test]
    fn a_trailing_comma_may_come_or_go() {
        let without = "val x = f(\n  a,\n  b\n)\nval y = 1 // y\n";
        let with = "val x = f(\n  a,\n  b,\n)\nval y = 1 // y\n";
        assert_eq!(check_equivalent(without, with), Ok(()));
        assert_eq!(check_equivalent(with, without), Ok(()));
        let types = "type T = Map[\n  A,\n  B,\n]\n";
        assert_eq!(check_equivalent("type T = Map[A, B]\n", types), Ok(()));
    }

    #[test]
    fn a_comma_before_a_closing_parenthesis_on_its_line_counts() {
        // Not a trailing comma: Scala allows one only before a line break.
        let d = differs("val x = f(a)\n", "val x = f(a,)\n");
        assert_eq!(d.kind, DifferenceKind::Code);
    }

    #[test]
    fn a_trailing_comma_with_a_comment_counts() {
        differs("val x = f(\n  a, // a\n)\n", "val x = f(\n  a // a\n)\n");
    }

    #[test]
    fn identical_text_is_equivalent() {
        let code = "object A {\n  def f(x: Int) = x + 1 // inc\n}\n";
        assert_eq!(check_equivalent(code, code), Ok(()));
    }

    #[test]
    fn whitespace_changes_in_brace_code_are_fine() {
        assert_eq!(
            check_equivalent(
                "object A{def f(x:Int)=x+1;val y=2}",
                "object A {\n  def f(x: Int) = x + 1; val y = 2\n}\n"
            ),
            Ok(())
        );
    }

    #[test]
    fn reindenting_own_line_comments_is_fine() {
        assert_eq!(
            check_equivalent(
                "object A {\n// note\n    val x = 1\n}",
                "object A {\n  // note\n  val x = 1\n}"
            ),
            Ok(())
        );
    }

    #[test]
    fn a_lambda_body_can_move_to_its_own_line() {
        let one_line = "val a = f(x => x + 1)\n";
        let two_lines = "val a = f(x =>\n  x + 1\n)\n";
        assert_eq!(check_equivalent(one_line, two_lines), Ok(()));
        assert_eq!(check_equivalent(two_lines, one_line), Ok(()));
        // Only a block holding one expression: two statements are not one.
        differs(
            "val a = f(x => g(x))\n",
            "val a = f(x =>\n  g(x)\n  h(x)\n)\n",
        );
        // A definition's body on its own line is the expression too.
        assert_eq!(check_equivalent("def f = 1\n", "def f =\n  1\n"), Ok(()));
        differs("def f = 1\n", "def f =\n  1\n  2\n");
    }

    #[test]
    fn a_comment_can_be_reindented() {
        let before = "object A {\n    /** Doc.\n      * More.\n      */\n    def f = 1\n}\n";
        let after = "object A {\n  /** Doc.\n    * More.\n    */\n  def f = 1\n}\n";
        assert_eq!(check_equivalent(before, after), Ok(()));
        let changed = "object A {\n  /** Doc.\n    * Less.\n    */\n  def f = 1\n}\n";
        assert_eq!(differs(before, changed).kind, DifferenceKind::Comment);
    }

    #[test]
    fn changed_token_is_reported_with_both_positions() {
        let d = differs("val x = 1\nval y = 2", "val x = 1\n\nval y = 3");
        assert_eq!(d.kind, DifferenceKind::Code);
        assert_eq!(
            d.message,
            r#"expected value: integer_literal "2", found value: integer_literal "3""#
        );
        assert_eq!((d.before.unwrap().line, d.before.unwrap().column), (2, 9));
        assert_eq!((d.after.unwrap().line, d.after.unwrap().column), (3, 9));
    }

    #[test]
    fn scala3_dedent_that_changes_blocks_is_caught() {
        // Same tokens, but `def g` leaves object A.
        let before = "object A:\n  def f = 1\n  def g = 2\n";
        let after = "object A:\n  def f = 1\ndef g = 2\n";
        assert_eq!(differs(before, after).kind, DifferenceKind::Code);
        // Shifting the whole object keeps its structure.
        let shifted = "object A:\n    def f = 1\n    def g = 2\n";
        assert_eq!(check_equivalent(before, shifted), Ok(()));
    }

    #[test]
    fn whitespace_inside_strings_matters() {
        assert_eq!(
            differs(r#"val s = "a b""#, r#"val s = "a  b""#).kind,
            DifferenceKind::Code
        );
        assert_eq!(
            differs(r#"val s = s"a ${x} b""#, r#"val s = s"a ${x}  b""#).kind,
            DifferenceKind::Code
        );
        let multiline = "val s = \"\"\"a\n  b\"\"\"";
        assert_eq!(
            differs(multiline, "val s = \"\"\"a\n b\"\"\"").kind,
            DifferenceKind::Code
        );
    }

    #[test]
    fn new_syntax_error_is_reported() {
        let d = differs("val x = (1 + 2)", "val x = (1 + 2");
        assert_eq!(d.kind, DifferenceKind::NewSyntaxError);
        assert!(d.before.is_none() && d.after.is_some(), "{d}");
    }

    #[test]
    fn untouched_broken_code_is_equivalent() {
        let broken = "object A {\n  def f( = 1\n";
        assert_eq!(check_equivalent(broken, broken), Ok(()));
    }

    #[test]
    fn dropped_comment_is_reported() {
        let d = differs("val x = 1 // keep me\n", "val x = 1\n");
        assert_eq!(d.kind, DifferenceKind::Comment);
        assert_eq!(d.message, r#"comment "// keep me" is missing"#);
    }

    #[test]
    fn added_comment_is_reported() {
        let d = differs("val x = 1\n", "val x = 1 /* new */\n");
        assert_eq!(d.message, r#"comment "/* new */" was added"#);
        assert!(d.before.is_none());
    }

    #[test]
    fn edited_comment_is_reported() {
        let d = differs("val x = 1 // teh\n", "val x = 1 // the\n");
        assert_eq!(d.message, r#"comment "// teh" became "// the""#);
    }

    #[test]
    fn comment_moved_to_another_token_is_reported() {
        let d = differs("f(a, // first\n  b)", "f(a,\n  // first\n  b)");
        assert_eq!(d.kind, DifferenceKind::Comment);
        assert_eq!(
            d.message,
            r#"comment "// first" moved from after "," to before "b""#
        );
    }

    #[test]
    fn comment_moved_to_end_of_file_is_reported() {
        let d = differs("val x = 1 // c\n", "val x = 1\n// c\n");
        assert_eq!(
            d.message,
            r#"comment "// c" moved from after "1" to at the end of the file"#
        );
    }

    #[test]
    fn display_includes_positions() {
        let d = differs("val x = 1", "val x = 2");
        assert_eq!(
            d.to_string(),
            r#"expected value: integer_literal "1", found value: integer_literal "2" (before 1:9, after 1:9)"#
        );
    }
}
