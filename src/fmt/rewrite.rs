//! Rewrites to the syntax of a file's dialect, made before formatting:
//! `import a._` to `import a.*`, and `import a.{b => c}` to
//! `import a.{b as c}`, in imports and exports alike, where the dialect
//! allows the new syntax, and back where it doesn't. A rename without
//! braces, `import a.b as c`, gets them: `import a.{b => c}`.
//!
//! A type wildcard `_` becomes `?`, as in `List[?]`, where the dialect
//! allows it. Not back: without it, `?` is a type's name.
//!
//! A vararg splice `f(xs: _*)` becomes `f(xs*)`, and `case Seq(xs @ _*)`
//! becomes `case Seq(xs*)`, and back. Outside Scala 3, where `*` splices
//! only a simple expression, `f(a ++ b: _*)` becomes `f((a ++ b)*)`.
//!
//! Scala 3's control syntax: `if (c) a` becomes `if c then a`,
//! `while (c) x` becomes `while c do x`, and `for (x <- xs) f(x)` becomes
//! `for x <- xs do f(x)`, and back. `for` braces stay; back in Scala 2, a
//! `for` written over several lines gets them.
//!
//! An `implicit` parameter clause becomes a `using` one: `def f(implicit
//! ec: EC)` becomes `def f(using ec: EC)`, where the dialect allows it. Not
//! where a class parameter is a `val` or `var` or has modifiers: there,
//! `using` would make it a `given`, which `import a.*` doesn't import. Not
//! in a class's first clause either: `class C(implicit ec: EC)` gets an
//! empty clause before it, which callers pass as `new C()(ec)`, and
//! `class C(using ec: EC)` doesn't. And not back: `using EC`, without a
//! name, has no Scala 2 form. Callers that pass the argument explicitly
//! must then write `f(using ec)`.
//!
//! A compound type becomes an intersection one: `A with B` becomes `A & B`,
//! where the dialect allows it. Not back: in Scala 2, `&` is a type's name.
//! In a pattern it gets parentheses, as Scala 3's own rewrite does:
//! `case x: A with B` becomes `case x: (A & B)`, since `case x: A & B` is
//! the pattern `x: A` and `B`. Not in a lambda's parameter, as
//! `{ x: A with B => x }`.
//! Not in `new A with B`, which makes a class, and not in `extends A with
//! B`. Not where it has a refinement, as `A with B { def f: C }`: there,
//! the refinement would go with `B` alone. And not inside an infix type, as
//! `A with B + C`: `&` binds less tightly than `+`, and tree-sitter-scala
//! reads infix types left to right whatever their operators, so the
//! equivalence check couldn't tell.
//!
//! The parents a class extends are separated by commas: `extends A with
//! B` becomes `extends A, B`, where the dialect allows it, and back. A
//! `with` that starts a line leaves its comma at the end of the line
//! before. Not with Scala 2's early definitions, `extends { val x = 1 }
//! with A`, which Scala 3 doesn't have, and not in an enum case, where
//! tree-sitter-scala reads `case C extends E, B` as two cases.
//!
//! Only tokens change; comments and whitespace stay where they are, and
//! code copied verbatim, such as `format: off` blocks, is left alone.
//! `check_equivalent` treats each old form and its new one as the same.

use crate::config::Dialect;
use crate::lossless_tree::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize, Tree,
    check_equivalent_trees, is_extends_separator, is_implicit_clause_keyword,
    is_type_wildcard_place, parse,
};

/// A rewrite tree-sitter-scala reads as the same code.
pub(super) struct Rewritten {
    pub(super) tree: Tree,
    /// How many constructs were left as written because, rewritten, they
    /// were read as other code. Read by the corpus check.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(super) dropped: usize,
}

/// A replacement of a range of the source.
type Edit = (TextRange, String);

/// One construct's edits, which are kept or left out together, and the
/// range they're in.
struct Unit {
    range: TextRange,
    edits: Vec<Edit>,
}

/// How many times a rewrite is parsed again after leaving out a construct
/// that tree-sitter-scala read differently.
const TRIES: usize = 16;

/// The source rewritten for `dialect` and parsed, or `None` if nothing
/// changes. Each construct's edits are kept only if the result has no
/// syntax errors and is equivalent: where it isn't, the innermost construct
/// around the first difference is left as written and the rest tried again.
pub(super) fn rewrite(tree: &Tree, dialect: Dialect) -> Option<Rewritten> {
    let text = tree.syntax().text();
    let units = units(tree, dialect);
    let mut kept = vec![true; units.len()];
    for dropped in 0..TRIES {
        let mut chosen: Vec<(usize, &Edit)> = units
            .iter()
            .enumerate()
            .filter(|&(i, _)| kept[i])
            .flat_map(|(i, unit)| unit.edits.iter().map(move |edit| (i, edit)))
            .collect();
        if chosen.is_empty() {
            return None;
        }
        // Units come outer first: where two insert at the same place, as two
        // `}` after the same line, the inner one's goes first.
        chosen.sort_by_key(|&(unit, (range, _))| {
            (range.start(), range.end(), std::cmp::Reverse(unit))
        });
        let chosen: Vec<&Edit> = chosen.into_iter().map(|(_, edit)| edit).collect();
        let rewritten = parse(&apply(text, &chosen));
        // Where the problem is in the original: a syntax error's range, or
        // the first difference.
        let at = match rewritten.errors().first() {
            Some(error) => TextRange::new(
                original_offset(&chosen, error.range().start()),
                original_offset(&chosen, error.range().end()),
            ),
            None => match check_equivalent_trees(tree, &rewritten) {
                Ok(()) => {
                    return Some(Rewritten {
                        tree: rewritten,
                        dropped,
                    });
                }
                Err(difference) => {
                    let offset = match (difference.before, difference.after) {
                        (Some(before), _) => before.offset,
                        (None, Some(after)) => original_offset(&chosen, after.offset),
                        (None, None) => return None,
                    };
                    TextRange::empty(offset)
                }
            },
        };
        // The innermost construct it touches, or else the nearest before
        // it, or after it.
        let kept_units = || (0..units.len()).filter(|&i| kept[i]);
        let touching = kept_units()
            .filter(|&i| {
                units[i].range.contains_inclusive(at.start()) || at.contains_range(units[i].range)
            })
            .min_by_key(|&i| units[i].range.len());
        let before = || {
            kept_units()
                .filter(|&i| units[i].range.start() <= at.start())
                .max_by_key(|&i| units[i].range.start())
        };
        let after = || kept_units().min_by_key(|&i| units[i].range.start());
        let drop = touching.or_else(before).or_else(after)?;
        kept[drop] = false;
    }
    None
}

/// Whether `dialect` has anything to rewrite in `tree`, before checking
/// how tree-sitter-scala reads the result.
pub(super) fn changes(tree: &Tree, dialect: Dialect) -> bool {
    !units(tree, dialect).is_empty()
}

/// `text` with `edits`, sorted by where they start, made.
fn apply(text: &str, edits: &[&Edit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (range, replacement) in edits {
        out.push_str(&text[copied..usize::from(range.start())]);
        out.push_str(replacement);
        copied = usize::from(range.end());
    }
    out.push_str(&text[copied..]);
    out
}

/// Where an offset in the text `edits` made was in the original.
fn original_offset(edits: &[&Edit], offset: TextSize) -> TextSize {
    let offset = i64::from(u32::from(offset));
    let mut shift: i64 = 0;
    for (range, replacement) in edits {
        let start = i64::from(u32::from(range.start())) + shift;
        if offset < start {
            break;
        }
        if offset < start + replacement.len() as i64 {
            return range.start();
        }
        shift += replacement.len() as i64 - i64::from(u32::from(range.len()));
    }
    TextSize::from(u32::try_from(offset - shift).unwrap_or(0))
}

/// Whether an edit at `range` changes code copied verbatim: it overlaps a
/// verbatim range, or inserts strictly inside one.
fn in_verbatim(tree: &Tree, range: TextRange) -> bool {
    tree.verbatim_ranges().iter().any(|v| {
        if range.is_empty() {
            v.range.start() < range.start() && range.start() < v.range.end()
        } else {
            v.range.intersect(range).is_some_and(|r| !r.is_empty())
        }
    })
}

/// Every construct's edits for `dialect`.
fn units(tree: &Tree, dialect: Dialect) -> Vec<Unit> {
    let text = tree.syntax().text();
    let mut units = Vec::new();
    for node in tree.syntax().descendants() {
        let mut edits = Vec::new();
        edits_for(tree, text, dialect, node, &mut edits);
        // All or nothing: a construct with an edit in code copied verbatim,
        // such as a `format: off` block, is left as written.
        if !edits.is_empty() && !edits.iter().any(|(range, _)| in_verbatim(tree, *range)) {
            let range = edits
                .iter()
                .fold(node.text_range(), |range, (edit, _)| range.cover(*edit));
            units.push(Unit { range, edits });
        }
    }
    units
}

/// The edits for `dialect` that `node` makes, added to `edits`.
fn edits_for(
    tree: &Tree,
    text: &str,
    dialect: Dialect,
    node: SyntaxNode<'_>,
    edits: &mut Vec<Edit>,
) {
    {
        let tokens = || {
            node.children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .filter(|t| !t.is_trivia() && tree.verbatim_at(t.text_range().start()).is_none())
        };
        match node.kind() {
            SyntaxKind::NamespaceWildcard if dialect.star_wildcard_import => {
                for token in tokens().filter(|t| t.text() == "_") {
                    edits.push((token.text_range(), "*".to_string()));
                }
            }
            SyntaxKind::NamespaceWildcard => {
                for token in tokens().filter(|t| t.text() == "*") {
                    edits.push((token.text_range(), "_".to_string()));
                }
            }
            SyntaxKind::Wildcard
                if dialect.question_mark_type_wildcard
                    && is_type_wildcard_place(NodeOrToken::Node(node)) =>
            {
                for token in tokens().filter(|t| t.text() == "_") {
                    // `_<:A` is fine, but `?<:` would be one operator.
                    let end = usize::from(token.text_range().end());
                    let joins = text[end..].chars().next().is_some_and(is_operator_char);
                    let mark = if joins { "? " } else { "?" };
                    edits.push((token.text_range(), mark.to_string()));
                }
            }
            SyntaxKind::Parameters | SyntaxKind::ClassParameters if dialect.given_using => {
                for token in tokens().filter(|t| t.text() == "implicit") {
                    if is_implicit_clause_keyword(&token) {
                        edits.push((token.text_range(), "using".to_string()));
                    }
                }
            }
            SyntaxKind::CompoundType if dialect.and_types && and_type(node) => {
                // All or nothing: not where part of it is copied verbatim.
                if node
                    .tokens()
                    .any(|t| !t.is_trivia() && tree.verbatim_at(t.text_range().start()).is_some())
                {
                    return;
                }
                // In a pattern, `&` needs parentheses.
                let in_pattern = node.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        SyntaxKind::TypedPattern | SyntaxKind::GivenPattern
                    )
                });
                if let (true, Some(first), Some(last)) =
                    (in_pattern, node.first_token(), node.last_code_token())
                {
                    edits.push((TextRange::empty(first.text_range().start()), "(".into()));
                    edits.push((TextRange::empty(last.text_range().end()), ")".into()));
                }
                for token in tokens().filter(|t| t.text() == "with") {
                    edits.push((token.text_range(), "&".to_string()));
                }
            }
            SyntaxKind::ExtendsClause => {
                let separators = extends_separators(tree, node, dialect.comma_separated_extends);
                edits.extend(separators.into_iter().flatten());
            }
            SyntaxKind::Vararg | SyntaxKind::RepeatPattern => {
                if let Some(splice) = splice(text, node, dialect).filter(|edits| {
                    edits
                        .iter()
                        .all(|(range, _)| tree.verbatim_at(range.start()).is_none())
                }) {
                    edits.extend(splice);
                }
            }
            SyntaxKind::IfExpression | SyntaxKind::WhileExpression | SyntaxKind::ForExpression => {
                let control = control(text, node, dialect.quiet_syntax).filter(|edits| {
                    edits
                        .iter()
                        .all(|(range, _)| tree.verbatim_at(range.start()).is_none())
                });
                edits.extend(control.into_iter().flatten());
                if node.kind() == SyntaxKind::ForExpression && dialect.significant_indentation {
                    edits.extend(braces(tree, node).into_iter().flatten());
                }
            }
            SyntaxKind::Block
            | SyntaxKind::CaseBlock
            | SyntaxKind::TemplateBody
            | SyntaxKind::EnumBody
                if dialect.significant_indentation =>
            {
                edits.extend(braces(tree, node).into_iter().flatten());
            }
            SyntaxKind::IndentedBlock
            | SyntaxKind::IndentedCases
            | SyntaxKind::TemplateBody
            | SyntaxKind::EnumBody
                if !dialect.significant_indentation =>
            {
                edits.extend(braces_back(tree, node).into_iter().flatten());
            }
            // `catch case e => b`, one case on the `catch` line, is Scala 3's.
            SyntaxKind::CatchClause if !dialect.significant_indentation => {
                let case = tokens().find(|t| t.text() == "case");
                if let (Some(case), Some(last)) = (case, node.last_code_token()) {
                    let at = |offset: TextSize| usize::from(offset);
                    let start = at(case.text_range().start());
                    let end = at(last.text_range().end());
                    edits.push((TextRange::empty(case.text_range().start()), "{ ".into()));
                    if text[start..end].contains('\n') {
                        // Over several lines, the `}` on its own at the
                        // `catch` line's width, after the last line.
                        let line = text[..start].rfind('\n').map_or(0, |i| i + 1);
                        let width = text[line..].len() - text[line..].trim_start_matches(' ').len();
                        let eol = text[end..].find('\n').map_or(text.len(), |i| end + i);
                        let eol = TextSize::try_from(eol).unwrap_or_default();
                        edits.push((TextRange::empty(eol), format!("\n{}}}", " ".repeat(width))));
                    } else {
                        edits.push((TextRange::empty(last.text_range().end()), " }".into()));
                    }
                }
            }
            SyntaxKind::CallExpression if dialect.fewer_braces => {
                edits.extend(fewer_braces(tree, node).into_iter().flatten());
            }
            SyntaxKind::ColonArgument if !dialect.fewer_braces => {
                edits.extend(fewer_braces_back(tree, node).into_iter().flatten());
            }
            SyntaxKind::ArrowRenamedIdentifier if dialect.as_for_import_rename => {
                for token in tokens().filter(|t| matches!(t.text(), "=>" | "\u{21D2}")) {
                    edits.push((token.text_range(), spaced(text, &token, "as")));
                }
            }
            SyntaxKind::AsRenamedIdentifier if !dialect.as_for_import_rename => {
                let arrows: Vec<_> = tokens().filter(|t| t.text() == "as").collect();
                // `import foo as f` has no Scala 2 form: braces need a path
                // before them.
                if arrows.is_empty() || !after_a_path(node) {
                    return;
                }
                for token in arrows {
                    edits.push((token.text_range(), spaced(text, &token, "=>")));
                }
                // Scala 2 renames only in braces.
                let braced = node
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::NamespaceSelectors);
                if let (false, Some(first), Some(last)) =
                    (braced, node.first_token(), node.last_code_token())
                {
                    let start = first.text_range().start();
                    let end = last.text_range().end();
                    edits.push((TextRange::new(start, start), "{".to_string()));
                    edits.push((TextRange::new(end, end), "}".to_string()));
                }
            }
            _ => {}
        }
    }
}

/// The edits that separate the parents in an `extends` clause by commas,
/// `extends A, B`, or else by `with`, `extends A with B`, or `None` if
/// there's nothing to change. Not where it mixes the two, has early
/// definitions, `extends { val x = 1 } with A`, is an enum case's, or has a
/// `with` that starts a line with comments around it or alone on its line.
pub(super) fn extends_separators(
    tree: &Tree,
    clause: SyntaxNode<'_>,
    commas: bool,
) -> Option<Vec<Edit>> {
    let text = tree.syntax().text();
    // tree-sitter-scala reads `case C extends E, B` as two enum cases.
    let enum_case = clause.parent().is_some_and(|p| {
        matches!(
            p.kind(),
            SyntaxKind::SimpleEnumCase | SyntaxKind::FullEnumCase
        )
    });
    let parts: Vec<_> = clause.non_trivia_children().collect();
    if enum_case || parts.iter().any(|p| p.kind() == SyntaxKind::StructuralType) {
        return None;
    }
    let separators: Vec<SyntaxToken<'_>> = parts
        .iter()
        .filter_map(|p| p.as_token().copied())
        .filter(is_extends_separator)
        .collect();
    let old = if commas { "with" } else { "," };
    if separators.is_empty()
        || separators
            .iter()
            .any(|t| t.text() != old || tree.verbatim_at(t.text_range().start()).is_some())
    {
        return None;
    }
    let mut edits = Vec::new();
    for separator in separators {
        let previous = std::iter::successors(separator.prev_token(), SyntaxToken::prev_token)
            .find(|t| !t.is_trivia())?;
        let next = std::iter::successors(separator.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia())?;
        let at = |offset: TextSize| usize::from(offset);
        let before = &text[at(previous.text_range().end())..at(separator.text_range().start())];
        let after = &text[at(separator.text_range().end())..at(next.text_range().start())];
        let blank = |gap: &str| gap.chars().all(char::is_whitespace);
        if !commas {
            // `A,B` needs spaces around `with`.
            let space = if blank(after) && !after.is_empty() {
                ""
            } else {
                " "
            };
            edits.push((separator.text_range(), format!(" with{space}")));
        } else if before.contains('\n') {
            // `A` then `with B` on the next line: `A,` then `B`.
            if !blank(before) || !blank(after) || after.contains('\n') {
                return None;
            }
            edits.push((TextRange::empty(previous.text_range().end()), ",".into()));
            edits.push((
                TextRange::new(separator.text_range().start(), next.text_range().start()),
                String::new(),
            ));
        } else if blank(before) {
            let start = previous.text_range().end();
            edits.push((
                TextRange::new(start, separator.text_range().end()),
                ",".into(),
            ));
        } else {
            edits.push((separator.text_range(), ",".into()));
        }
    }
    Some(edits)
}

/// Whether `node` is a compound type, as `A with B`, that becomes an
/// intersection one.
pub(super) fn and_type(node: SyntaxNode<'_>) -> bool {
    node.kind() == SyntaxKind::CompoundType
        && is_plain_compound_type(node)
        && node
            .children_with_tokens()
            .any(|c| c.as_token().is_some_and(|t| t.text() == "with"))
}

/// Whether a compound type, as `A with B`, means the same as `A & B`: not
/// in `new A with B`, not with a refinement, whose braces would go with the
/// last part alone, not inside an infix type, where `&` would bind less
/// tightly than other operators, and not as a lambda's parameter.
fn is_plain_compound_type(node: SyntaxNode<'_>) -> bool {
    let refined = |n: &SyntaxNode<'_>| n.children().any(|c| c.kind() == SyntaxKind::Refinement);
    !refined(&node)
        && !node.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                SyntaxKind::InstanceExpression
                    | SyntaxKind::InfixType
                    | SyntaxKind::CompoundType
                    | SyntaxKind::ExtendsClause
                    | SyntaxKind::LambdaExpression
            )
        })
}

/// Whether a rename comes after a path and its `.`, as `c` in
/// `import a.b.c as d`, or in braces after one.
pub(crate) fn after_a_path(rename: SyntaxNode<'_>) -> bool {
    if rename
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::NamespaceSelectors)
    {
        return true;
    }
    rename
        .first_token()
        .and_then(|t| {
            std::iter::successors(t.prev_token(), SyntaxToken::prev_token).find(|t| !t.is_trivia())
        })
        .is_some_and(|t| t.text() == ".")
}

/// The edits that write a vararg splice, `node`, as `xs*` where `dialect`
/// allows it, or else as `xs: _*`, or in a pattern `xs @ _*`: the text
/// between `xs` and `*` replaced. None if it's already written so, or if a
/// comment is in the way. `_*` in a pattern is the same in both. Outside
/// Scala 3, `*` splices only a simple expression, so `a ++ b: _*` becomes
/// `(a ++ b)*`: without the parentheses, Scala 2 reads `a ++ (b*)`.
pub(super) fn splice(
    text: &str,
    node: SyntaxNode<'_>,
    dialect: Dialect,
) -> Option<Vec<(TextRange, String)>> {
    let postfix = dialect.postfix_star_vararg_splices;
    let mut children = node.non_trivia_children();
    let (first, star) = (children.next()?, node.last_child_or_token()?);
    if star.text() != "*" {
        return None;
    }
    // The spliced name or expression.
    let (spliced, written) = match (node.kind(), first.kind()) {
        (SyntaxKind::Vararg, _) => {
            let colon = children.next().is_some_and(|c| c.text() == ":");
            (first, colon)
        }
        (_, SyntaxKind::CapturePattern) => {
            let first = first.into_node()?;
            if first.child_by_field_name("pattern")?.kind() != SyntaxKind::Wildcard {
                return None;
            }
            (first.child_by_field_name("name")?, true)
        }
        (_, SyntaxKind::Identifier) => (first, false),
        _ => return None,
    };
    if postfix != written {
        return None;
    }
    let range = TextRange::new(spliced.text_range().end(), star.text_range().start());
    let between = &text[range];
    let parens = postfix
        && !dialect.infix_postfix_star_vararg_splices
        && matches!(
            spliced.kind(),
            SyntaxKind::InfixExpression
                | SyntaxKind::PostfixExpression
                | SyntaxKind::AscriptionExpression
                | SyntaxKind::AssignmentExpression
                | SyntaxKind::LambdaExpression
                | SyntaxKind::IfExpression
                | SyntaxKind::MatchExpression
                | SyntaxKind::TryExpression
                | SyntaxKind::WhileExpression
                | SyntaxKind::DoWhileExpression
                | SyntaxKind::ForExpression
                | SyntaxKind::ReturnExpression
                | SyntaxKind::ThrowExpression
        );
    let in_the_way = if postfix {
        // Only `: _` or `@ _` and spaces; and `xs*` mustn't make one name,
        // as `foo_*` or `++*` would, unless `)` comes between.
        let last = spliced.text().chars().next_back()?;
        !between
            .chars()
            .all(|c| c.is_whitespace() || matches!(c, ':' | '@' | '_'))
            || !parens && (last == '_' || is_operator_char(last))
    } else {
        !between.chars().all(char::is_whitespace)
    };
    if in_the_way {
        return None;
    }
    let replacement = match (postfix, node.kind()) {
        (true, _) if parens => ")",
        (true, _) => "",
        (false, SyntaxKind::Vararg) => ": _",
        (false, _) => " @ _",
    };
    let mut edits = Vec::new();
    if parens {
        edits.push((TextRange::empty(spliced.text_range().start()), "(".to_string()));
    }
    edits.push((range, replacement.to_string()));
    Some(edits)
}

/// The edits that write a control expression, `node`, in Scala 3's syntax
/// when `quiet`, or else in Scala 2's. None if it's already written so, or
/// if it's left as written: where a line break or comment is in the way,
/// or where a condition starts with `(` or `{`, so taking the parentheses
/// away could leave it looking like the old syntax or a block.
pub(super) fn control(
    text: &str,
    node: SyntaxNode<'_>,
    quiet: bool,
) -> Option<Vec<(TextRange, String)>> {
    let child = |text_of: &str, field: Option<&str>| {
        node.children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .find(|t| t.text() == text_of && (field.is_none() || t.field_name() == field))
    };
    if misread_either_way(text, node, quiet) {
        return None;
    }
    let between =
        |start: TextRange, end: TextRange| &text[TextRange::new(start.end(), end.start())];
    let blank = |s: &str| s.chars().all(char::is_whitespace);
    let one_line = |s: &str| !s.contains('\n');
    let mut edits = Vec::new();
    if node.kind() == SyntaxKind::ForExpression {
        let open = child("(", Some("enumerators")).or_else(|| child("{", Some("enumerators")));
        let close = child(")", Some("enumerators")).or_else(|| child("}", Some("enumerators")));
        let (done, yields) = (child("do", None), child("yield", None));
        // The node, not the brackets around it, which share its field.
        let enums = node
            .children()
            .find(|c| c.kind() == SyntaxKind::Enumerators)
            .map(NodeOrToken::Node);
        match (quiet, open, close) {
            // `for (x <- xs) f(x)` to `for x <- xs do f(x)`; braces stay.
            (true, Some(open), Some(close)) if open.text() == "(" => {
                let enums = code_range(&enums?)?;
                if !one_line(between(open.text_range(), enums))
                    || !one_line(between(enums, close.text_range()))
                {
                    return None;
                }
                edits.push((open.text_range(), unspaced(text, &open)));
                let after = if done.is_some() || yields.is_some() {
                    unspaced(text, &close)
                } else {
                    spaced(text, &close, "do")
                };
                edits.push((close.text_range(), after));
            }
            // A `do` after parentheses or braces goes.
            (false, Some(_), Some(close)) => {
                let done = done?;
                let gap = TextRange::new(close.text_range().end(), done.text_range().end());
                if !blank(&text[TextRange::new(gap.start(), done.text_range().start())]) {
                    return None;
                }
                edits.push((gap, String::new()));
            }
            (false, None, None) => {
                let keyword = done.or(yields)?;
                let first = node.first_token()?;
                let enums = code_range(&enums?)?;
                let gap = between(enums, keyword.text_range());
                if !blank(gap) {
                    return None;
                }
                let several_lines =
                    !one_line(between(first.text_range(), enums)) || !one_line(&text[enums]);
                let done = keyword.text() == "do";
                if !several_lines {
                    // `for x <- xs do f(x)` to `for (x <- xs) f(x)`.
                    edits.push((TextRange::new(enums.start(), enums.start()), "(".into()));
                    if done {
                        let range = TextRange::new(enums.end(), keyword.text_range().end());
                        edits.push((range, ")".into()));
                    } else {
                        edits.push((TextRange::new(enums.end(), enums.end()), ")".into()));
                    }
                } else {
                    // In braces, which allow line breaks between
                    // enumerators.
                    let end = first.text_range().end();
                    edits.push((TextRange::new(end, end), " {".into()));
                    let (range, close) = match (one_line(gap), done) {
                        // `}` on the keyword's line.
                        (false, true) => (keyword.text_range(), "}"),
                        (false, false) => (keyword.text_range(), "} yield"),
                        (true, true) => (
                            TextRange::new(enums.end(), keyword.text_range().end()),
                            " }",
                        ),
                        (true, false) => (TextRange::new(enums.end(), enums.end()), " }"),
                    };
                    edits.push((range, close.into()));
                }
            }
            _ => return None,
        }
        return Some(edits);
    }
    let keyword_text = if node.kind() == SyntaxKind::IfExpression {
        "then"
    } else {
        "do"
    };
    let keyword = child(keyword_text, Some("condition"));
    let condition = node.child_by_field_name("condition")?;
    let parens = condition.kind() == SyntaxKind::ParenthesizedExpression;
    match (quiet, parens, keyword) {
        // `if (c) a` to `if c then a`.
        (true, true, keyword) => {
            let condition = condition.into_node()?;
            let mut children = condition.non_trivia_children();
            let (open, inner, close) = (children.next()?, children.next()?, children.next()?);
            if children.next().is_some() || open.text() != "(" || close.text() != ")" {
                return None;
            }
            let (open, close) = (open.into_token()?, close.into_token()?);
            let inner = code_range(&inner)?;
            if !one_line(between(open.text_range(), inner))
                || !one_line(between(inner, close.text_range()))
                || text[inner].starts_with(['(', '{'])
            {
                return None;
            }
            edits.push((open.text_range(), unspaced(text, &open)));
            let after = if keyword.is_some() {
                unspaced(text, &close)
            } else {
                spaced(text, &close, keyword_text)
            };
            edits.push((close.text_range(), after));
        }
        // `if (c) then a` to `if (c) a`.
        (false, true, Some(keyword)) => {
            let condition = code_range(&condition)?;
            if !blank(between(condition, keyword.text_range())) {
                return None;
            }
            let range = TextRange::new(condition.end(), keyword.text_range().end());
            edits.push((range, String::new()));
        }
        // `if c then a` to `if (c) a`.
        (false, false, Some(keyword)) => {
            let first = node.first_token()?;
            let condition = code_range(&condition)?;
            if !one_line(between(first.text_range(), condition))
                || !blank(between(condition, keyword.text_range()))
                || !one_line(between(condition, keyword.text_range()))
            {
                return None;
            }
            let start = condition.start();
            edits.push((TextRange::new(start, start), "(".into()));
            let range = TextRange::new(condition.end(), keyword.text_range().end());
            edits.push((range, ")".into()));
        }
        _ => return None,
    }
    Some(edits)
}

/// The edits that take a construct's braces out for Scala 3's indentation:
/// `def f = {` to `def f =`, `x match {` to `x match`, `class A {` to
/// `class A:`, and `for {` ... `} yield` to `for` ... `yield`, with the `}`
/// line gone, or the `}` before `else`, `catch`, `finally` or `yield`. A
/// symbolic name before the `:` gets backticks, `` object `#::`: ``, and a
/// constructor's `private` or `protected` an empty clause,
/// `class A private ():`, as Scala 3 deprecates a `:` right after either.
/// None where the layout leaves any doubt: unless `{` ends its line, `}`
/// starts its own, every line between is deeper than the `}` and the line
/// the construct starts on, the statements start at one width and the lines
/// keep to Scala 3's indentation rules (see [`lines_aligned`]), and the
/// next line isn't deeper than the `}`.
pub(super) fn braces(tree: &Tree, node: SyntaxNode<'_>) -> Option<Vec<(TextRange, String)>> {
    let text = tree.syntax().text();
    let kind = node.kind();
    let (open, close) = if kind == SyntaxKind::ForExpression {
        let token = |t: &str| {
            node.children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .find(|x| x.text() == t && x.field_name() == Some("enumerators"))
        };
        (token("{")?, token("}")?)
    } else {
        let mut children = node.non_trivia_children();
        let open = children.next()?.into_token()?;
        let close = children.last()?.into_token()?;
        (open, close)
    };
    if open.text() != "{" || close.text() != "}" {
        return None;
    }
    let before = previous_code(open)?;
    // What the construct belongs to: it starts the line its body is placed
    // from.
    let owner = if kind == SyntaxKind::ForExpression {
        node
    } else {
        node.parent()?
    };
    let allowed = match (kind, owner.kind(), before.text()) {
        (SyntaxKind::ForExpression, _, "for") => true,
        (SyntaxKind::Block, SyntaxKind::CaseClause, "=>")
        | (SyntaxKind::Block, SyntaxKind::IfExpression, "then" | "else" | ")")
        | (SyntaxKind::Block, SyntaxKind::WhileExpression, "do" | ")")
        | (SyntaxKind::Block, SyntaxKind::ForExpression, "do" | "yield" | ")")
        | (SyntaxKind::Block, SyntaxKind::TryExpression, "try")
        | (SyntaxKind::Block, SyntaxKind::FinallyClause, "finally")
        | (SyntaxKind::CaseBlock, SyntaxKind::MatchExpression, "match")
        | (SyntaxKind::CaseBlock, SyntaxKind::CatchClause, "catch") => true,
        (SyntaxKind::Block, owner, "=") => matches!(
            owner,
            SyntaxKind::FunctionDefinition | SyntaxKind::ValDefinition | SyntaxKind::VarDefinition
        ),
        (SyntaxKind::TemplateBody | SyntaxKind::EnumBody, owner, _) => matches!(
            owner,
            SyntaxKind::ClassDefinition
                | SyntaxKind::ObjectDefinition
                | SyntaxKind::TraitDefinition
                | SyntaxKind::EnumDefinition
        ),
        _ => false,
    };
    if !allowed {
        return None;
    }
    let at = |offset: TextSize| usize::from(offset);
    let line_start = |offset: TextSize| text[..at(offset)].rfind('\n').map_or(0, |i| i + 1);
    let indent = |offset: TextSize| {
        let start = line_start(offset);
        text[start..].len() - text[start..].trim_start_matches(' ').len()
    };
    let blank = |from: TextSize, to: TextSize| text[at(from)..at(to)].trim().is_empty();
    // Only spaces between the brace and what it follows, `{` ending its line
    // and `}` starting its own, with something between.
    let first = next_code(open)?;
    let last = previous_code(close)?;
    if first == close
        || !blank(before.text_range().end(), open.text_range().start())
        || text[at(before.text_range().end())..at(open.text_range().start())].contains('\n')
        || !text[at(open.text_range().end())..at(first.text_range().start())].contains('\n')
        || !text[at(last.text_range().end())..at(close.text_range().start())].contains('\n')
        || !blank(
            TextSize::try_from(line_start(close.text_range().start())).ok()?,
            close.text_range().start(),
        )
    {
        return None;
    }
    // Every line between deeper than the `}` and the construct's first line.
    let owner_start = owner.tokens().find(|t| !t.is_trivia())?;
    let floor = indent(close.text_range().start()).max(indent(owner_start.text_range().start()));
    if !lines_deeper(
        tree,
        at(open.text_range().end()),
        line_start(close.text_range().start()),
        floor,
    ) || !lines_aligned(tree, open, close)
        || (kind != SyntaxKind::ForExpression && !statements_share_width(node.non_trivia_children()))
    {
        return None;
    }
    // Without braces, a `case` clause's body must be deeper than its `case`.
    if kind == SyntaxKind::CaseBlock {
        for clause in node
            .children()
            .filter(|c| c.kind() == SyntaxKind::CaseClause)
        {
            let first = clause.tokens().find(|t| !t.is_trivia())?;
            let end = clause.last_code_token()?.text_range().end();
            let width = indent(first.text_range().start());
            if !lines_deeper(tree, at(first.text_range().end()), at(end), width) {
                return None;
            }
        }
    }
    let mut edits = Vec::new();
    let template = matches!(kind, SyntaxKind::TemplateBody | SyntaxKind::EnumBody);
    let symbolic = before.text().chars().all(is_operator_char);
    let opening = match kind {
        // `object #:::` would be one name, and Scala 3 deprecates
        // `object #:: :`, so the name gets backticks: `` object `#::`: ``.
        _ if template && symbolic => {
            edits.push((before.text_range(), format!("`{}`", before.text())));
            ":"
        }
        // `foo_+` ends in an operator too, but `foo_+:` is one name.
        _ if template && before.text().chars().next_back().is_some_and(is_operator_char) => {
            return None;
        }
        // Scala 3 warns about `class A private:` as it does about
        // `object #:: :`, so it gets an empty clause: `class A private ():`.
        _ if template
            && matches!(before.text(), "private" | "protected")
            && before
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::AccessModifier) =>
        {
            " ():"
        }
        SyntaxKind::TemplateBody | SyntaxKind::EnumBody => ":",
        _ => "",
    };
    let gap = TextRange::new(before.text_range().end(), open.text_range().end());
    edits.push((gap, opening.to_string()));
    // After the `}`: a keyword that goes on, or nothing on its line and no
    // deeper line next.
    let after = next_code(close);
    let same_line = after.is_some_and(|a| {
        !text[at(close.text_range().end())..at(a.text_range().start())].contains('\n')
    });
    match after {
        Some(after) if same_line => {
            let goes_on = matches!(after.text(), "else" | "catch" | "finally" | "yield");
            if !goes_on || !blank(close.text_range().end(), after.text_range().start()) {
                return None;
            }
            let range = TextRange::new(close.text_range().start(), after.text_range().start());
            edits.push((range, String::new()));
        }
        // A `for`'s enumerators end at its `yield`.
        _ if kind == SyntaxKind::ForExpression => return None,
        _ => {
            let eol = text[at(close.text_range().end())..]
                .find('\n')
                .map_or(text.len(), |i| at(close.text_range().end()) + i);
            if !text[at(close.text_range().end())..eol].trim().is_empty()
                || after.is_some_and(|a| {
                    indent(a.text_range().start()) > indent(close.text_range().start())
                })
            {
                return None;
            }
            let line_break = text[..at(close.text_range().start())].rfind('\n')?;
            let range = TextRange::new(
                TextSize::try_from(line_break).ok()?,
                close.text_range().end(),
            );
            edits.push((range, String::new()));
        }
    }
    Some(edits)
}

/// The edits that make a call's braced argument a colon argument, as
/// SIP-44 allows: `xs.map { x =>` to `xs.map: x =>`, `xs.foreach {` to
/// `xs.foreach:` and `xs.collect {` with `case` clauses to `xs.collect:`.
/// Only where the lambda's parameters are a name, `_`, one name with a
/// type, which gets parentheses, `{ x: A =>` to `: (x: A) =>`, or in
/// parentheses, and what follows the `}` on its line is nothing, closing brackets, or a
/// `.` and what it selects.
///
/// Over several lines, `{` or the lambda's `=>` ends its line, every line
/// between is deeper than the `}`, the `{` line and the call's first line,
/// the statements start at one width and the lines keep to Scala 3's
/// indentation rules (see [`lines_aligned`]), and the `}` starts its line
/// and goes, with its line if nothing follows it, when the next line isn't
/// deeper. On one line, with one expression or `case` and no comments, the
/// body goes on the next line, a step deeper
/// than the line the call's argument lists start on, and a `.` after the
/// `}` on a line of its own at that line's width.
pub(super) fn fewer_braces(tree: &Tree, call: SyntaxNode<'_>) -> Option<Vec<Edit>> {
    let text = tree.syntax().text();
    let argument = call.child_by_field_name("arguments")?.into_node()?;
    if !matches!(argument.kind(), SyntaxKind::Block | SyntaxKind::CaseBlock) {
        return None;
    }
    let mut children = argument.non_trivia_children();
    let open = children.next()?.into_token()?;
    let close = argument.non_trivia_children().last()?.into_token()?;
    if open.text() != "{" || close.text() != "}" {
        return None;
    }
    let before = previous_code(open)?;
    // scalac rejects a colon argument after a braced one, `f { a }: x =>`,
    // and `case` clauses after a colon in a `for` generator on the `for`
    // line; to be safe, nothing in a `for`'s enumerators is rewritten.
    if before.text() == "}"
        || call
            .ancestors()
            .any(|a| a.kind() == SyntaxKind::Enumerators)
    {
        return None;
    }
    let at = |offset: TextSize| usize::from(offset);
    let line_start = |offset: TextSize| text[..at(offset)].rfind('\n').map_or(0, |i| i + 1);
    let indent = |offset: TextSize| {
        let start = line_start(offset);
        text[start..].len() - text[start..].trim_start_matches(' ').len()
    };
    let breaks = |from: TextSize, to: TextSize| text[at(from)..at(to)].contains('\n');
    // What starts the body: the `{`, or a lambda's `=>`.
    let statements: Vec<_> = argument
        .non_trivia_children()
        .filter(|c| c.kind().is_named())
        .collect();
    // A typed parameter without parentheses, `{ x: A =>`, gets them, which
    // a colon argument needs: `: (x: A) =>`.
    let mut parens = Vec::new();
    let line_end = match statements.as_slice() {
        [NodeOrToken::Node(lambda)] if lambda.kind() == SyntaxKind::LambdaExpression => {
            let parameters = lambda.child_by_field_name("parameters")?;
            if !matches!(
                parameters.kind(),
                SyntaxKind::Identifier | SyntaxKind::Wildcard | SyntaxKind::Bindings
            ) || breaks(open.text_range().end(), parameters.text_range().start())
            {
                return None;
            }
            let parts: Vec<_> = lambda
                .non_trivia_children()
                .filter(|c| c.field_name() == Some("parameters"))
                .collect();
            match parts.as_slice() {
                [_] => {}
                [_, NodeOrToken::Token(colon), ty] if colon.text() == ":" => {
                    let end = match ty {
                        NodeOrToken::Node(n) => n.last_code_token()?.text_range().end(),
                        NodeOrToken::Token(t) => t.text_range().end(),
                    };
                    parens.push((
                        TextRange::empty(parameters.text_range().start()),
                        "(".into(),
                    ));
                    parens.push((TextRange::empty(end), ")".into()));
                }
                _ => return None,
            }
            lambda
                .children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .find(|t| t.text() == "=>")?
        }
        [] => return None,
        _ => open,
    };
    let first = next_code(line_end)?;
    let last = previous_code(close)?;
    if first == close
        || before
            .text()
            .chars()
            .next_back()
            .is_some_and(is_operator_char)
        || !text[at(before.text_range().end())..at(open.text_range().start())]
            .chars()
            .all(|c| c == ' ')
    {
        return None;
    }
    // What follows the `}` on its line: nothing, closing brackets, or a
    // `.` and what it selects.
    let after = next_code(close);
    let after_on_line = after.filter(|a| !breaks(close.text_range().end(), a.text_range().start()));
    let selects = after_on_line.is_some_and(|a| a.text() == ".");
    if after_on_line.is_some_and(|a| !matches!(a.text(), "." | ")" | "]"))
        || after_on_line.is_some_and(|a| {
            !text[at(close.text_range().end())..at(a.text_range().start())]
                .trim()
                .is_empty()
        })
    {
        return None;
    }
    // scalac reads XML elements separated by whitespace as one `NodeSeq`,
    // whatever their indentation, so without the `}` a body ending in XML
    // would take in XML that follows it.
    let in_xml = last
        .parent()
        .is_some_and(|p| p.ancestors().any(|a| a.kind() == SyntaxKind::XmlExpression));
    if in_xml && after.is_some_and(|a| a.text().starts_with('<')) {
        return None;
    }
    let colon = (
        TextRange::new(before.text_range().end(), open.text_range().end()),
        ":".to_string(),
    );
    if !breaks(open.text_range().start(), close.text_range().end()) {
        // On one line: one expression or `case`, and no comments.
        let tokens: Vec<_> = argument.tokens().filter(|t| t.owns_trivia()).collect();
        let single = match argument.kind() {
            SyntaxKind::CaseBlock => statements.len() == 1,
            _ => tokens.iter().all(|t| t.text() != ";"),
        };
        let commented = tokens.iter().any(|t| {
            t.leading_trivia()
                .chain(t.trailing_trivia())
                .any(|c| c.is_comment())
                && *t != close
        });
        if !single || commented || close.leading_trivia().any(|c| c.is_comment()) {
            return None;
        }
        let width = lists_start_width(text, open)?;
        let mut edits = vec![
            colon,
            (
                TextRange::new(line_end.text_range().end(), first.text_range().start()),
                format!("\n{}", " ".repeat(width + 2)),
            ),
        ];
        match after_on_line {
            Some(dot) if selects => edits.push((
                TextRange::new(last.text_range().end(), dot.text_range().start()),
                format!("\n{}", " ".repeat(width)),
            )),
            _ => edits.push((
                TextRange::new(last.text_range().end(), close.text_range().end()),
                String::new(),
            )),
        }
        edits.extend(parens);
        return Some(edits);
    }
    if !breaks(line_end.text_range().end(), first.text_range().start())
        || !breaks(last.text_range().end(), close.text_range().start())
        || !text[line_start(close.text_range().start())..at(close.text_range().start())]
            .trim()
            .is_empty()
    {
        return None;
    }
    let call_first = call.tokens().find(|t| t.owns_trivia())?;
    let floor = indent(close.text_range().start())
        .max(indent(open.text_range().start()))
        .max(indent(call_first.text_range().start()));
    // A lambda's statements are in its body.
    let body = match statements.as_slice() {
        [NodeOrToken::Node(lambda)] if lambda.kind() == SyntaxKind::LambdaExpression => lambda
            .child_by_field_name("body")
            .and_then(NodeOrToken::into_node)
            .filter(|b| matches!(b.kind(), SyntaxKind::Block | SyntaxKind::IndentedBlock)),
        _ => Some(argument),
    };
    if !lines_deeper(
        tree,
        at(line_end.text_range().end()),
        line_start(close.text_range().start()),
        floor,
    ) || !lines_aligned(tree, line_end, close)
        || body.is_some_and(|b| !statements_share_width(b.non_trivia_children()))
    {
        return None;
    }
    if argument.kind() == SyntaxKind::CaseBlock {
        for clause in argument
            .children()
            .filter(|c| c.kind() == SyntaxKind::CaseClause)
        {
            let first = clause.tokens().find(|t| !t.is_trivia())?;
            let end = clause.last_code_token()?.text_range().end();
            if !lines_deeper(
                tree,
                at(first.text_range().end()),
                at(end),
                indent(first.text_range().start()),
            ) {
                return None;
            }
        }
    }
    let close_edit = match after_on_line {
        // `}.filter(p)` to `.filter(p)`, and `})` to `)`.
        Some(after) => TextRange::new(close.text_range().start(), after.text_range().start()),
        // The `}` line goes, if no deeper line follows.
        None => {
            let eol = text[at(close.text_range().end())..]
                .find('\n')
                .map_or(text.len(), |i| at(close.text_range().end()) + i);
            if !text[at(close.text_range().end())..eol].trim().is_empty()
                || after.is_some_and(|a| {
                    indent(a.text_range().start()) > indent(close.text_range().start())
                })
            {
                return None;
            }
            let line_break = text[..at(close.text_range().start())].rfind('\n')?;
            TextRange::new(
                TextSize::try_from(line_break).ok()?,
                close.text_range().end(),
            )
        }
    };
    let mut edits = vec![colon, (close_edit, String::new())];
    edits.extend(parens);
    Some(edits)
}

/// The width of the line a call's argument lists start on, from a token
/// after them: back over each `(...)` and `[...]` before it, so `fn(` for
/// `): x =>` on a later line.
fn lists_start_width(text: &str, token: SyntaxToken<'_>) -> Option<usize> {
    let mut anchor = token;
    while let Some(previous) = previous_code(anchor).filter(|t| matches!(t.text(), ")" | "]")) {
        anchor = previous.parent()?.tokens().find(|t| t.owns_trivia())?;
    }
    let start = text[..usize::from(anchor.text_range().start())]
        .rfind('\n')
        .map_or(0, |i| i + 1);
    Some(text[start..].len() - text[start..].trim_start_matches(' ').len())
}

/// The edits that make a colon argument a braced one, for a dialect
/// without SIP-44: `xs.map: x =>` to `xs.map { x =>`, with a `}` on a line
/// of its own after the body, at the width of the line the call's argument
/// lists start on, as `fn(` when they end on a later line: `): x =>`. A
/// `.` or closing bracket starting the next line goes after the `}`, as
/// in `}.filter(p)`, and a closing bracket after the body on its line goes
/// after the `}` too. None if it ends in an end marker.
pub(super) fn fewer_braces_back(tree: &Tree, argument: SyntaxNode<'_>) -> Option<Vec<Edit>> {
    let text = tree.syntax().text();
    let colon = previous_code(argument.tokens().find(|t| t.owns_trivia())?)?;
    let last = argument.last_code_token()?;
    let after = next_code(last);
    if colon.text() != ":" || after.is_some_and(|t| t.text() == "end") {
        return None;
    }
    let width = lists_start_width(text, colon)?;
    let at = |offset: TextSize| usize::from(offset);
    let close = match after {
        Some(after) if matches!(after.text(), "." | ")" | "]") => {
            let start = after.text_range().start();
            let starts_line = text[..at(start)].trim_end_matches(' ').ends_with('\n');
            if starts_line {
                // `}.filter(p)` and `})`.
                (TextRange::empty(start), "}".to_string())
            } else {
                (
                    TextRange::empty(start),
                    format!("\n{}}}", " ".repeat(width)),
                )
            }
        }
        _ => {
            let end = at(last.text_range().end());
            let eol = text[end..].find('\n').map_or(text.len(), |i| end + i);
            let eol = TextSize::try_from(eol).ok()?;
            (TextRange::empty(eol), format!("\n{}}}", " ".repeat(width)))
        }
    };
    Some(vec![(colon.text_range(), " {".to_string()), close])
}

/// Whether every line starting in `start..end` is indented deeper than
/// `width`, with spaces: not the line `start` is on, blank lines, lines
/// inside a string or comment, or a closing bracket's, which ends what it
/// closes wherever it is.
fn lines_deeper(tree: &Tree, start: usize, end: usize, width: usize) -> bool {
    let text = tree.syntax().text();
    let mut offset = start;
    text[start..end]
        .split_inclusive('\n')
        .enumerate()
        .all(|(i, line)| {
            let line_start = offset;
            offset += line.len();
            let content = line.trim_start_matches(' ');
            let inside = TextSize::try_from(line_start)
                .ok()
                .and_then(|s| tree.verbatim_at(s).map(|v| v.range.start() < s))
                .unwrap_or(false);
            i == 0
                || content.trim().is_empty()
                || inside
                || content.starts_with(['}', ')', ']'])
                || (!content.starts_with('\t') && line.len() - content.len() > width)
        })
}

/// Whether the lines from the one after `open` to the one `close` starts
/// keep to Scala 3's indentation rules without braces around them: a line
/// less deep than the one before it goes back to the width of a region
/// that is open, not between two. A region opens on the line after one
/// that ends in `=`, `=>`, `then`, `match` and the like; a deeper line
/// after any other is a continuation, which opens none. In braces, scalac
/// lets `else` sit between its `if` line and the `then` branch, and a
/// statement between two widths; without them, it doesn't. Closing
/// brackets' lines are skipped, and so are lines in a string; a tab in the
/// indentation fails.
fn lines_aligned(tree: &Tree, open: SyntaxToken<'_>, close: SyntaxToken<'_>) -> bool {
    // Scala 3's list, with `{`, a template's `:` and a given's `with`, and
    // the `)` that ends an old-style `if` or `while` condition or `for`
    // enumerators: `if (c)` and the body indented below.
    let opens_region = |token: SyntaxToken<'_>| {
        matches!(
            token.text(),
            "{" | "=" | "=>" | "?=>" | "<-" | ":" | "with" | "if" | "then" | "else" | "while"
                | "do" | "for" | "yield" | "match" | "try" | "catch" | "finally" | "return"
                | "throw"
        ) || token.text() == ")"
            && token.parent().is_some_and(|p| {
                p.kind() == SyntaxKind::ForExpression
                    || p.field_name() == Some("condition")
                        && p.parent().is_some_and(|q| {
                            matches!(
                                q.kind(),
                                SyntaxKind::IfExpression | SyntaxKind::WhileExpression
                            )
                        })
            })
    };
    let mut widths: Vec<usize> = Vec::new();
    let mut previous = open;
    let mut token = next_code(open);
    while let Some(t) = token.filter(|t| *t != close) {
        let start = t.text_range().start();
        let in_string = tree.verbatim_at(start).is_some_and(|v| v.range.start() < start);
        if t.is_first_on_line() && !in_string && !matches!(t.text(), "}" | ")" | "]") {
            let indentation = t.indentation();
            if indentation.contains('\t') {
                return false;
            }
            let width = indentation.len();
            if widths.last().is_none_or(|&w| w < width) {
                if widths.is_empty() || opens_region(previous) {
                    widths.push(width);
                }
            } else {
                while widths.last().is_some_and(|&w| w > width) {
                    widths.pop();
                }
                if widths.last() != Some(&width) {
                    return false;
                }
            }
        }
        previous = t;
        token = next_code(t);
    }
    true
}

/// Whether the statements among `elements` (its named nodes) that start a
/// line all start at one width. In braces scruff keeps a statement deeper
/// than the one before it where it is; without them, scalac would read it
/// as that statement's continuation.
fn statements_share_width<'t>(elements: impl Iterator<Item = SyntaxElement<'t>>) -> bool {
    let mut widths = elements
        .filter(|e| e.kind().is_named())
        .filter_map(|e| match e {
            NodeOrToken::Node(node) => node.tokens().find(SyntaxToken::owns_trivia),
            NodeOrToken::Token(token) => Some(token),
        })
        .filter(SyntaxToken::is_first_on_line)
        .map(|t| t.indentation());
    match widths.next() {
        Some(first) => widths.all(|w| w == first),
        None => true,
    }
}

/// The edits that put braces around an indented construct, `node`, for a
/// dialect without significant indentation: `def f =` + statements to
/// `def f = {` + statements + `}`, `x match` + cases to `x match {` ... `}`,
/// and `class A:` to `class A {` ... `}`. The `}` goes on a line of its own
/// at the width of the construct's first line, or before an `else`,
/// `catch`, `finally` or `yield` on the next line. None where Scala 2 needs
/// no braces, a single expression, or where the construct isn't one that
/// can have them, or ends in an end marker, which Scala 2 doesn't have.
pub(super) fn braces_back(tree: &Tree, node: SyntaxNode<'_>) -> Option<Vec<(TextRange, String)>> {
    let text = tree.syntax().text();
    let kind = node.kind();
    let owner = node.parent()?;
    let statements: Vec<_> = node.region_statements().collect();
    let first = node.tokens().find(|t| t.owns_trivia())?;
    let last = node.last_code_token()?;
    let opener = match kind {
        // `class A:`'s colon is the template body's first token.
        SyntaxKind::TemplateBody | SyntaxKind::EnumBody => {
            if first.text() != ":" {
                return None;
            }
            first
        }
        _ => previous_code(first)?,
    };
    let allowed = match (kind, owner.kind(), opener.text()) {
        (SyntaxKind::IndentedBlock, SyntaxKind::IfExpression, "then" | "else" | ")")
        | (SyntaxKind::IndentedBlock, SyntaxKind::WhileExpression, "do" | ")")
        | (SyntaxKind::IndentedBlock, SyntaxKind::ForExpression, "do" | "yield" | ")" | "}")
        | (SyntaxKind::IndentedBlock, SyntaxKind::TryExpression, "try")
        | (SyntaxKind::IndentedBlock, SyntaxKind::FinallyClause, "finally")
        | (SyntaxKind::IndentedCases, SyntaxKind::MatchExpression, "match")
        | (SyntaxKind::IndentedCases, SyntaxKind::CatchClause, "catch") => true,
        (SyntaxKind::IndentedBlock, owner, "=") => matches!(
            owner,
            SyntaxKind::FunctionDefinition | SyntaxKind::ValDefinition | SyntaxKind::VarDefinition
        ),
        (SyntaxKind::TemplateBody | SyntaxKind::EnumBody, owner, ":") => matches!(
            owner,
            SyntaxKind::ClassDefinition
                | SyntaxKind::ObjectDefinition
                | SyntaxKind::TraitDefinition
                | SyntaxKind::EnumDefinition
        ),
        _ => false,
    };
    // A block of one expression needs no braces in Scala 2.
    let one_expression = kind == SyntaxKind::IndentedBlock
        && statements.len() == 1
        && !statements[0].kind().name().ends_with("_definition")
        && !statements[0].kind().name().ends_with("_declaration")
        && statements[0].kind() != SyntaxKind::ImportDeclaration;
    let at = |offset: TextSize| usize::from(offset);
    let starts_line = |token: SyntaxToken<'_>| {
        text[..at(token.text_range().start())]
            .trim_end_matches([' ', '\t'])
            .ends_with('\n')
    };
    let body_first = statements.first().and_then(|s| match s {
        NodeOrToken::Node(n) => n.tokens().find(|t| t.owns_trivia()),
        NodeOrToken::Token(t) => Some(*t),
    })?;
    let after = next_code(last);
    // An end marker, which the `}` would have to stand for.
    let ends_marked = after.is_some_and(|a| a.text() == "end")
        || owner.children().any(|c| c.kind() == SyntaxKind::EndMarker);
    if !allowed || one_expression || ends_marked || !starts_line(body_first) {
        return None;
    }
    let line_start = |offset: TextSize| text[..at(offset)].rfind('\n').map_or(0, |i| i + 1);
    let owner_first = owner.tokens().find(|t| t.owns_trivia())?;
    let start = line_start(owner_first.text_range().start());
    let width = text[start..].len() - text[start..].trim_start_matches(' ').len();
    let mut edits = Vec::new();
    match kind {
        SyntaxKind::TemplateBody | SyntaxKind::EnumBody => {
            edits.push((opener.text_range(), " {".to_string()));
        }
        _ => {
            let end = opener.text_range().end();
            edits.push((TextRange::new(end, end), " {".to_string()));
        }
    }
    match after {
        // `} else` where the `else` starts the next line.
        Some(after)
            if matches!(after.text(), "else" | "catch" | "finally" | "yield")
                && starts_line(after) =>
        {
            let start = after.text_range().start();
            edits.push((TextRange::new(start, start), "} ".to_string()));
        }
        _ => {
            // After the rest of the last line, comments included.
            let end = at(last.text_range().end());
            let eol = text[end..].find('\n').map_or(text.len(), |i| end + i);
            let eol = TextSize::try_from(eol).ok()?;
            let close = format!("\n{}}}", " ".repeat(width));
            edits.push((TextRange::new(eol, eol), close));
        }
    }
    Some(edits)
}

/// The code token before `token`, skipping trivia.
fn previous_code<'t>(token: SyntaxToken<'t>) -> Option<SyntaxToken<'t>> {
    std::iter::successors(token.prev_token(), SyntaxToken::prev_token).find(|t| t.owns_trivia())
}

/// The code token after `token`, skipping trivia.
fn next_code<'t>(token: SyntaxToken<'t>) -> Option<SyntaxToken<'t>> {
    std::iter::successors(token.next_token(), SyntaxToken::next_token).find(|t| t.owns_trivia())
}

/// Whether tree-sitter-scala reads `node` differently in one of the two
/// syntaxes, so rewriting it would change the tree:
///
/// - A body that is a `match`: `for (x <- xs) x match { ... }` is read
///   with the `for` as what's matched, unlike `for x <- xs do x match`.
/// - An `if` ending another's consequence, where only one has an `else`:
///   which one it goes to can change, as in `if a then x = if b then 1
///   else 2`, where it goes to the first.
/// - An end marker: `end if` is one only after `if c then`, and a call
///   after `if (c)`.
/// - `inline if`, which Scala 2 doesn't have.
fn misread_either_way(text: &str, node: SyntaxNode<'_>, quiet: bool) -> bool {
    let body = node
        .child_by_field_name("body")
        .or_else(|| node.child_by_field_name("consequence"));
    let matched = node.field_name() == Some("value")
        && node
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::MatchExpression);
    if matched || body.is_some_and(|b| b.kind() == SyntaxKind::MatchExpression) {
        return true;
    }
    if node.kind() == SyntaxKind::IfExpression && dangling_else(node) {
        return true;
    }
    let mut tokens = node.tokens().filter(|t| !t.is_trivia());
    let mut keyword = tokens.next().map_or("", |t| t.text());
    let inline = keyword == "inline";
    if inline {
        keyword = tokens.next().map_or("", |t| t.text());
    }
    let after = text[usize::from(node.text_range().end())..].trim_start();
    let end_after = after
        .strip_prefix("end")
        .is_some_and(|rest| rest.trim_start_matches([' ', '\t']).starts_with(keyword));
    let end_inside = node.children().any(|c| c.kind() == SyntaxKind::EndMarker);
    end_after || end_inside || (!quiet && inline)
}

/// Whether `node`, an `if`, ends another's consequence or another ends
/// its own, where only one of the two has an `else`.
fn dangling_else(node: SyntaxNode<'_>) -> bool {
    let has_else = |n: &SyntaxNode<'_>| {
        n.children_with_tokens()
            .any(|c| c.as_token().is_some_and(|t| t.text() == "else"))
    };
    let end = |n: &NodeOrToken<SyntaxNode<'_>, SyntaxToken<'_>>| code_range(n).map(|r| r.end());
    // An `if` ending this one's consequence.
    if let Some(NodeOrToken::Node(c)) = node.child_by_field_name("consequence") {
        let c_end = end(&NodeOrToken::Node(c));
        if c.descendants().any(|d| {
            d.kind() == SyntaxKind::IfExpression
                && end(&NodeOrToken::Node(d)) == c_end
                && has_else(&d) != has_else(&node)
        }) {
            return true;
        }
    }
    // An `if` whose consequence this one ends.
    let node_end = end(&NodeOrToken::Node(node));
    std::iter::successors(node.parent(), SyntaxNode::parent).any(|a| {
        a.kind() == SyntaxKind::IfExpression
            && has_else(&a) != has_else(&node)
            && a.child_by_field_name("consequence").is_some_and(|c| {
                c.text_range().contains_range(node.text_range()) && end(&c) == node_end
            })
    })
}

/// Where an element's code starts and ends, without the trivia and line
/// breaks at its edges.
fn code_range(element: &NodeOrToken<SyntaxNode<'_>, SyntaxToken<'_>>) -> Option<TextRange> {
    let node = match element {
        NodeOrToken::Token(token) => return Some(token.text_range()),
        NodeOrToken::Node(node) => node,
    };
    let code = |t: &SyntaxToken<'_>| t.owns_trivia() && !t.text().trim().is_empty();
    let first = node.tokens().find(code)?;
    let start = node.text_range().start();
    let last = std::iter::successors(node.last_token(), SyntaxToken::prev_token)
        .take_while(|t| t.text_range().start() >= start)
        .find(code)?;
    Some(TextRange::new(
        first.text_range().start(),
        last.text_range().end(),
    ))
}

/// What replaces `token` when it goes: nothing, or a space where the text
/// on each side would otherwise join, as in `if(c)`.
fn unspaced(text: &str, token: &SyntaxToken<'_>) -> String {
    let (start, end) = (
        usize::from(token.text_range().start()),
        usize::from(token.text_range().end()),
    );
    let before = text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let after = text[end..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace());
    if before && after { " " } else { "" }.to_string()
}

/// A character of an operator such as `<:`, which `?` would join.
fn is_operator_char(c: char) -> bool {
    "!#%&*+-/:<=>?@\\^|~".contains(c)
}

/// `word` in place of `token`, with a space on each side where the source
/// has none, so `b=>c` doesn't become `basc`.
fn spaced(text: &str, token: &SyntaxToken<'_>, word: &str) -> String {
    let (start, end) = (
        usize::from(token.text_range().start()),
        usize::from(token.text_range().end()),
    );
    let before = text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| !c.is_whitespace());
    let after = text[end..]
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace());
    format!(
        "{}{word}{}",
        if before { " " } else { "" },
        if after { " " } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless_tree::parse;

    const NEW: Dialect = Dialect::SCALA3;
    const OLD: Dialect = Dialect {
        star_wildcard_import: false,
        as_for_import_rename: false,
        question_mark_type_wildcard: false,
        postfix_star_vararg_splices: false,
        infix_postfix_star_vararg_splices: false,
        quiet_syntax: false,
        significant_indentation: false,
        fewer_braces: false,
        given_using: false,
        and_types: false,
        comma_separated_extends: false,
        leading_infix_operators: false,
    };

    /// The rewritten text, if anything changed.
    fn changed(source: &str, dialect: Dialect) -> Option<String> {
        rewrite(&parse(source), dialect).map(|r| r.tree.syntax().text().to_string())
    }

    fn rewritten(source: &str, dialect: Dialect) -> String {
        changed(source, dialect).unwrap_or_else(|| source.to_string())
    }

    #[test]
    fn wildcards_and_renames() {
        assert_eq!(rewritten("import a._\n", NEW), "import a.*\n");
        assert_eq!(
            rewritten("import a.{b => c, d => _, _}\n", NEW),
            "import a.{b as c, d as _, *}\n"
        );
        assert_eq!(
            rewritten("import a.{given, _}\n", NEW),
            "import a.{given, *}\n"
        );
        assert_eq!(rewritten("export a._\n", NEW), "export a.*\n");
        assert_eq!(
            rewritten("object A {\n  import b.c._\n}\n", NEW),
            "object A:\n  import b.c.*\n"
        );
    }

    #[test]
    fn already_new_or_not_a_wildcard() {
        for source in [
            "import a.*\n",
            "import a.{b as c, *}\n",
            "import a.b as c\n",
            "import a.`_`\n",
            "import a.given\n",
            "val f = (_: Int) + 1\n",
            "def g(x: Int => Int) = x\n",
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
    }

    #[test]
    fn implicit_clauses_become_using_ones() {
        assert_eq!(
            rewritten("def f(x: A)(implicit ec: EC, b: B): C = c\n", NEW),
            "def f(x: A)(using ec: EC, b: B): C = c\n"
        );
        assert_eq!(
            rewritten("def f(implicit ec: EC): C\n", NEW),
            "def f(using ec: EC): C\n"
        );
        assert_eq!(
            rewritten("class C(x: A)(implicit ec: EC)\n", NEW),
            "class C(x: A)(using ec: EC)\n"
        );
        assert_eq!(
            rewritten("def this(x: A)(implicit ec: EC) = this()\n", NEW),
            "def this(x: A)(using ec: EC) = this()\n"
        );
        assert_eq!(
            rewritten("def f(implicit /* c */ ec: EC) = 1\n", NEW),
            "def f(using /* c */ ec: EC) = 1\n"
        );
        for source in [
            // `new C()(ec)` needs the empty clause `implicit` adds.
            "class C(implicit ec: EC)\n",
            "class C[A](implicit ec: EC) extends B\n",
            "class C(implicit val ec: EC)\n",
            "class C(implicit var ec: EC)\n",
            "class C(implicit private val ec: EC)\n",
            "def f(using ec: EC) = 1\n",
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
        // Not in Scala 2, and not back.
        let source3 = Dialect::named("scala213source3").unwrap();
        assert_eq!(changed("def f(implicit ec: EC) = 1\n", source3), None);
        assert_eq!(changed("def f(using ec: EC) = 1\n", OLD), None);
    }

    #[test]
    fn compound_types_become_intersection_ones() {
        assert_eq!(
            rewritten("val x: A with B with C = y\n", NEW),
            "val x: A & B & C = y\n"
        );
        assert_eq!(
            rewritten(
                "def f[T <: A with B](x: F[A with B]): A /* c */ with B = x\n",
                NEW
            ),
            "def f[T <: A & B](x: F[A & B]): A /* c */ & B = x\n"
        );
        assert_eq!(
            rewritten("x match { case _: A with B => 1 }\n", NEW),
            "x match { case _: (A & B) => 1 }\n"
        );
        assert_eq!(
            rewritten("x match { case given A with B => 1 }\n", NEW),
            "x match { case given (A & B) => 1 }\n"
        );
        assert_eq!(
            rewritten("x match { case y: (A with B) => 1 }\n", NEW),
            "x match { case y: (A & B) => 1 }\n"
        );
        assert_eq!(
            rewritten("type T = (A with B) => C with D\n", NEW),
            "type T = (A & B) => C & D\n"
        );
        for source in [
            "val a = new A with B\n",
            "val x: A with B { def f: Int } = y\n",
            "type T = A with B + C\n",
            "type T = A | B with C\n",
            "type T = A & B\n",
            "val f = { x: A with B => x }\n",
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
        // Not in Scala 2, and not back.
        let source3 = Dialect::named("scala213source3").unwrap();
        assert_eq!(changed("type T = A with B\n", source3), None);
        assert_eq!(changed("type T = A & B\n", OLD), None);
    }

    #[test]
    fn extends_gets_commas() {
        assert_eq!(
            rewritten("trait T extends A with B[X] with C\n", NEW),
            "trait T extends A, B[X], C\n"
        );
        assert_eq!(
            rewritten("class K(x: Int) extends A(x) with B\n", NEW),
            "class K(x: Int) extends A(x), B\n"
        );
        // A leading `with` leaves its comma on the line before.
        assert_eq!(
            rewritten("object O\n    extends A\n    with B\n    with C\n", NEW),
            "object O\n    extends A,\n    B,\n    C\n"
        );
        for source in [
            "object O extends { val x = 1 } with A\n",
            "enum E:\n  case C extends E with B\n",
            "object O extends A\n    /* c */ with B\n",
            "trait T extends A, B\n",
            "val a = new A with B\n",
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
        // Inside a body too, in a region or braces that become one.
        assert_eq!(
            rewritten(
                "object O:\n  class C\n      extends A\n      with B:\n    def f = 1\n",
                NEW
            ),
            "object O:\n  class C\n      extends A,\n      B:\n    def f = 1\n"
        );
        assert_eq!(
            rewritten(
                "object O {\n  trait T\n      extends A\n      with B\n}\n",
                NEW
            ),
            "object O:\n  trait T\n      extends A,\n      B\n"
        );
        // And back.
        assert_eq!(
            rewritten("trait T extends A, B[X],C\n", OLD),
            "trait T extends A with B[X] with C\n"
        );
        assert_eq!(
            rewritten("object O extends A,\n    B\n", OLD),
            "object O extends A with\n    B\n"
        );
        let source3 = Dialect::named("scala213source3").unwrap();
        assert_eq!(
            rewritten("trait T extends A, B\n", source3),
            "trait T extends A with B\n"
        );
    }

    #[test]
    fn a_rename_without_spaces_gets_them() {
        assert_eq!(rewritten("import a.{b=>c}\n", NEW), "import a.{b as c}\n");
    }

    #[test]
    fn comments_stay() {
        assert_eq!(
            rewritten("import a.{b /* x */ => c} // y\n", NEW),
            "import a.{b /* x */ as c} // y\n"
        );
    }

    #[test]
    fn only_what_the_dialect_allows() {
        let star_only = Dialect {
            as_for_import_rename: false,
            ..NEW
        };
        assert_eq!(
            rewritten("import a.{b => c, _}\n", star_only),
            "import a.{b => c, *}\n"
        );
        assert_eq!(changed("import a.{b => c, _}\n", OLD), None);
        let no_question_mark = Dialect {
            question_mark_type_wildcard: false,
            ..NEW
        };
        assert_eq!(changed("val a: List[_] = b\n", no_question_mark), None);
    }

    #[test]
    fn back_to_scala_2() {
        let scala2 = OLD;
        assert_eq!(
            rewritten("import a.{b as c, d as _, *}\n", scala2),
            "import a.{b => c, d => _, _}\n"
        );
        assert_eq!(rewritten("import a.*\n", scala2), "import a._\n");
        assert_eq!(
            rewritten("import a.b as c, d.e as _\n", scala2),
            "import a.{b => c}, d.{e => _}\n"
        );
        assert_eq!(changed("import a.{b => c, _}\n", scala2), None);
        // No Scala 2 form without a path.
        assert_eq!(changed("import foo as f\n", scala2), None);
    }

    #[test]
    fn type_wildcards() {
        for (source, expected) in [
            ("val a: List[_] = b\n", "val a: List[?] = b\n"),
            ("def f(m: Map[_, Int]) = m\n", "def f(m: Map[?, Int]) = m\n"),
            ("val a: List[_ <: A] = b\n", "val a: List[? <: A] = b\n"),
            (
                "val a: List[_ >: A <: B] = b\n",
                "val a: List[? >: A <: B] = b\n",
            ),
            ("val a: List[_<:A] = b\n", "val a: List[? <:A] = b\n"),
            (
                "x match { case _: List[_] => }\n",
                "x match { case _: List[?] => }\n",
            ),
            ("val a: F[List[_]] = b\n", "val a: F[List[?]] = b\n"),
        ] {
            assert_eq!(rewritten(source, NEW), expected, "{source}");
        }
    }

    #[test]
    fn not_type_wildcards() {
        for source in [
            // Type parameters.
            "def f[F[_]](x: F[Int]) = x\n",
            "type F[_] = Int\n",
            "class C[A[_ <: B]]\n",
            // Not a type.
            "val f = g(_)\n",
            "val a: List[?] = b\n",
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
        // Without the new syntax, `?` is a name: kind-projector's
        // placeholder here.
        assert_eq!(changed("val a: F[Either[A, ?]] = b\n", OLD), None);
    }

    #[test]
    fn vararg_splices() {
        for (old, new) in [
            ("f(xs: _*)\n", "f(xs*)\n"),
            ("f(a, g(b): _*)\n", "f(a, g(b)*)\n"),
            ("new A(xs: _*)\n", "new A(xs*)\n"),
            (
                "x match { case Seq(a, xs @ _*) => }\n",
                "x match { case Seq(a, xs*) => }\n",
            ),
        ] {
            assert_eq!(rewritten(old, NEW), new, "{old}");
            assert_eq!(rewritten(new, OLD), old, "{new}");
        }
        assert_eq!(rewritten("f(xs:_ *)\n", NEW), "f(xs*)\n");
        for same in [
            "x match { case Seq(_*) => }\n",
            // `*` would join the name or operator.
            "f(foo_ : _*)\n",
            "f(a.++ : _*)\n",
            // A comment in the way.
            "f(xs /* c */ : _*)\n",
        ] {
            assert_eq!(changed(same, NEW), None, "{same}");
        }
        assert_eq!(changed("x match { case Seq(_*) => }\n", OLD), None);
    }

    #[test]
    fn control_syntax() {
        for (old, new) in [
            ("if (c) a else b\n", "if c then a else b\n"),
            ("if (a) b else if (c) d\n", "if a then b else if c then d\n"),
            ("while (c) x\n", "while c do x\n"),
            ("for (x <- xs) f(x)\n", "for x <- xs do f(x)\n"),
            ("for (x <- xs) yield x\n", "for x <- xs yield x\n"),
            (
                "for (x <- xs; y <- ys) yield x\n",
                "for x <- xs; y <- ys yield x\n",
            ),
            (
                "def f =\n  if (c)\n    a\n  else\n    b\n",
                "def f =\n  if c then\n    a\n  else\n    b\n",
            ),
        ] {
            assert_eq!(rewritten(old, NEW), new, "{old}");
            assert_eq!(rewritten(new, OLD), old, "{new}");
        }
        // Back, a comment before `then` is in the way.
        assert_eq!(
            rewritten("if (a /* x */) b\n", NEW),
            "if a /* x */ then b\n"
        );
        assert_eq!(changed("if a /* x */ then b\n", OLD), None);
        // Braces go with the parentheses.
        assert_eq!(rewritten("if (c) {\n  a\n}\n", NEW), "if c then\n  a\n");
        // Written without spaces.
        assert_eq!(rewritten("if(c)a\n", NEW), "if c then a\n");
        assert_eq!(rewritten("while(c){x}\n", NEW), "while c do {x}\n");
        // Parentheses with `then` or `do` lose one or the other.
        assert_eq!(rewritten("if (c) then a\n", NEW), "if c then a\n");
        assert_eq!(rewritten("if (c) then a\n", OLD), "if (c) a\n");
        assert_eq!(
            rewritten("for (x <- xs) do f(x)\n", OLD),
            "for (x <- xs) f(x)\n"
        );
        // Braces go in Scala 3, and come back in Scala 2 for a `for` over
        // several lines.
        let braced = "for {\n  x <- xs\n  y <- ys\n} yield x\n";
        assert_eq!(
            rewritten(braced, NEW),
            "for\n  x <- xs\n  y <- ys\nyield x\n"
        );
        assert_eq!(
            rewritten("for\n  x <- xs\n  y <- ys\nyield x\n", OLD),
            braced
        );
        assert_eq!(
            rewritten("for\n  x <- xs\ndo\n  f(x)\n", OLD),
            "for {\n  x <- xs\n}\n  f(x)\n"
        );
    }

    #[test]
    fn control_syntax_left_as_written() {
        for source in [
            // A line break inside the parentheses.
            "if (\n  c\n) a\n",
            // A condition starting with `(` or `{`.
            "if ((a, b) == c) d\n",
            "while ({\n  a\n  b\n}) ()\n",
            "do x while (c)\n",
            // What tree-sitter-scala would read differently.
        ] {
            assert_eq!(changed(source, NEW), None, "{source}");
        }
        // The `match` loses its braces, but the `for` stays as written.
        let changed_for = changed("for (x <- xs) x match {\n  case _ =>\n}\n", NEW);
        assert!(changed_for.is_none_or(|c| c.starts_with("for (x <- xs) x match")));
        // Written with indentation, so in full Scala 3.
        assert_eq!(changed("def f =\n  if (c)\n    a\n  end if\n", NEW), None);
        // Neither `if`: the outer one would take the `else`.
        assert_eq!(changed("if (a) x = if (b) 1 else 2\n", NEW), None);
        let source3 = Dialect {
            quiet_syntax: false,
            ..NEW
        };
        assert_eq!(changed("if (c) a\n", source3), None);
        for source in [
            "def f =\n  if c then\n    a\n  end if\n",
            "inline if c then a\n",
        ] {
            assert_eq!(changed(source, OLD), None, "{source}");
        }
    }

    #[test]
    fn optional_braces() {
        for (old, new) in [
            (
                "def f = {\n  a\n  b\n}\nval x = 1\n",
                "def f =\n  a\n  b\nval x = 1\n",
            ),
            ("class A {\n  val x = 1\n}\n", "class A:\n  val x = 1\n"),
            (
                "class A extends B with C {\n  val x = 1\n}\n",
                "class A extends B, C:\n  val x = 1\n",
            ),
            ("enum E {\n  case A, B\n}\n", "enum E:\n  case A, B\n"),
            ("x match {\n  case a => 1\n}\n", "x match\n  case a => 1\n"),
            (
                "if c then {\n  a\n} else {\n  b\n}\n",
                "if c then\n  a\nelse\n  b\n",
            ),
            (
                "try {\n  a\n} catch {\n  case e => b\n} finally {\n  c\n}\n",
                "try\n  a\ncatch\n  case e => b\nfinally\n  c\n",
            ),
            (
                "for {\n  x <- xs\n  y <- ys\n} yield x\n",
                "for\n  x <- xs\n  y <- ys\nyield x\n",
            ),
            (
                "x match {\n  case a => {\n    b\n    c\n  }\n}\n",
                "x match\n  case a =>\n    b\n    c\n",
            ),
            (
                "object #:: {\n  val x = 1\n}\n",
                "object `#::`:\n  val x = 1\n",
            ),
            (
                "class A private {\n  val x = 1\n}\n",
                "class A private ():\n  val x = 1\n",
            ),
            (
                "class A protected {\n  val x = 1\n}\n",
                "class A protected ():\n  val x = 1\n",
            ),
            (
                "class A private[p] {\n  val x = 1\n}\n",
                "class A private[p]:\n  val x = 1\n",
            ),
            // A comment after `{` stays on its line.
            ("def f = { // c\n  a\n}\n", "def f = // c\n  a\n"),
            // Old control syntax becomes new too.
            ("if (c) {\n  a\n}\n", "if c then\n  a\n"),
        ] {
            assert_eq!(rewritten(old, NEW), new, "{old}");
            assert_eq!(
                crate::lossless_tree::check_equivalent(old, new),
                Ok(()),
                "{old}"
            );
        }
    }

    #[test]
    fn braces_left_as_written() {
        for source in [
            // `foo_+:` would be one name.
            "object foo_+ {\n  val x = 1\n}\n",
            // On one line.
            "def f = { a }\n",
            // Something after `}`.
            "def f = {\n  a\n}.map(g)\n",
            "val x = {\n  a\n} + 1\n",
            "def f = {\n  a\n} // c\n",
            // Something before `}` on its line, or after `{` on its.
            "def f = {\n  a }\n",
            "def f = { a\n}\n",
            // A line no deeper than the `}`.
            "def f = {\na\n}\n",
            // A deeper line after the `}`.
            "  def f = {\n    a\n  }\n    b\n",
            // Statements at two widths: without braces, the deeper one would
            // continue the one before it, or sit between two widths.
            "object A {\n  def a = 1\n     def b = 2\n}\n",
            "object A {\n  def a =\n    1\n\n   def b = 2\n}\n",
            // An `else` between its `if` line and the `then` branch.
            "def f = {\n  val x = if c then\n      a\n    else b\n  x\n}\n",
            // Empty.
            "class A {\n}\n",
            // A `for` without `yield` after its enumerators.
            "for {\n  x <- xs\n}\n{\n  f(x)\n}\n",
            // Not where Scala 3 allows indentation for braces.
            "new A {\n  val x = 1\n}\n",
            "val x = {\n  a\n}\n".replace("val x = ", "{\n").as_str(),
        ] {
            let changed = changed(source, NEW);
            assert!(
                changed
                    .as_deref()
                    .is_none_or(|c| c.contains('{') || !source.contains('{')),
                "{source:?} became {changed:?}"
            );
        }
        let source3 = Dialect::named("scala213source3").unwrap();
        assert_eq!(changed("class A {\n  val x = 1\n}\n", source3), None);
    }

    #[test]
    fn braces_back() {
        for (new, old) in [
            (
                "def f =\n  a\n  b\nval x = 1\n",
                "def f = {\n  a\n  b\n}\nval x = 1\n",
            ),
            ("class A:\n  val x = 1\n", "class A {\n  val x = 1\n}\n"),
            ("x match\n  case a => 1\n", "x match {\n  case a => 1\n}\n"),
            (
                "if c then\n  a\n  b\nelse\n  d\n  e\n",
                "if (c) {\n  a\n  b\n} else {\n  d\n  e\n}\n",
            ),
            // A comment after the last statement stays on its line.
            ("def f =\n  a\n  b // c\n", "def f = {\n  a\n  b // c\n}\n"),
            // Nested, each `}` at its own width.
            (
                "object A:\n  def f = x match\n    case a => 1\n",
                "object A {\n  def f = x match {\n    case a => 1\n  }\n}\n",
            ),
        ] {
            assert_eq!(rewritten(new, OLD), old, "{new}");
        }
        for same in [
            // One expression needs no braces.
            "def f =\n  a\n",
            "if (c)\n  a\nelse\n  b\n",
            // Scala 2 has no end markers.
            "def f =\n  a\n  b\nend f\n",
            // Braced typed lambda with tuple type is not a colon argument.
            "val x = xs.flatMap { a: (String, Json) =>\n  a._2\n}\n",
        ] {
            assert_eq!(changed(same, OLD), None, "{same}");
        }
    }

    #[test]
    fn fewer_braces() {
        let fewer = NEW;
        for (braced, colon) in [
            ("xs.map { x =>\n  a\n  b\n}\n", "xs.map: x =>\n  a\n  b\n"),
            ("xs.map { (x, y) =>\n  a\n}\n", "xs.map: (x, y) =>\n  a\n"),
            ("xs.foreach { _ =>\n  a\n}\n", "xs.foreach: _ =>\n  a\n"),
            ("xs.foreach {\n  a\n  b\n}\n", "xs.foreach:\n  a\n  b\n"),
            (
                "xs.collect {\n  case a => b\n}\n",
                "xs.collect:\n  case a => b\n",
            ),
            ("f(a) { x =>\n  b\n}\n", "f(a): x =>\n  b\n"),
            (
                "val y = xs\n  .map { x =>\n    a\n  }\n",
                "val y = xs\n  .map: x =>\n    a\n",
            ),
            // A selection or closing bracket after the `}`.
            (
                "xs.map { x =>\n  a\n}.filter(p)\n",
                "xs.map: x =>\n  a\n.filter(p)\n",
            ),
            ("f(xs.map { x =>\n  a\n})\n", "f(xs.map: x =>\n  a\n)\n"),
        ] {
            assert_eq!(rewritten(braced, fewer), colon, "{braced}");
            assert_eq!(rewritten(colon, OLD), braced, "{colon}");
        }
        // A typed parameter gets parentheses, which stay in braces.
        for (braced, colon) in [
            ("xs.map { x: Int =>\n  a\n}\n", "xs.map: (x: Int) =>\n  a\n"),
            (
                "xs.map { x: M.N =>\n  a\n  b\n}\n",
                "xs.map: (x: M.N) =>\n  a\n  b\n",
            ),
            ("xs.map { x: Int => a }\n", "xs.map: (x: Int) =>\n  a\n"),
        ] {
            assert_eq!(rewritten(braced, fewer), colon, "{braced}");
        }
        assert_eq!(
            rewritten("xs.map: (x: Int) =>\n  a\n", OLD),
            "xs.map { (x: Int) =>\n  a\n}\n"
        );
        // On one line, the body goes on the next.
        for (braced, colon) in [
            ("val a = xs.map { x => a }\n", "val a = xs.map: x =>\n  a\n"),
            ("xs.foreach { println(_) }\n", "xs.foreach:\n  println(_)\n"),
            (
                "xs.collect { case Some(x) => x }\n",
                "xs.collect:\n  case Some(x) => x\n",
            ),
            ("f(xs.map { x => a })\n", "f(xs.map: x =>\n  a)\n"),
            (
                "val b = xs.map { x => a }.filter(p)\n",
                "val b = xs.map: x =>\n  a\n.filter(p)\n",
            ),
            ("  foo(a)(b) { x => c }\n", "  foo(a)(b): x =>\n    c\n"),
        ] {
            assert_eq!(rewritten(braced, fewer), colon, "{braced}");
        }
        assert_eq!(
            rewritten("f(xs.map: x =>\n  a)\n", OLD),
            "f(xs.map { x =>\n  a\n})\n"
        );
        for same in [
            // Anything else after the `}`.
            "xs.map { x =>\n  a\n} + 1\n",
            "xs.map { x => a } + 1\n",
            // On one line, more than one expression, or a comment.
            "xs.foreach { a; b }\n",
            "xs.map { x => /* c */ a }\n",
            // The body starting on the `=>` line.
            "xs.map { x => a\n  b\n}\n",
            // An operator before the `{`.
            "xs ++ {\n  a\n}\n",
            // Not deeper than the `}`.
            "xs.map { x =>\na\n}\n",
            // A body ending in XML, with XML after the `}`: scalac would
            // read both as one literal.
            "val a = xs.map { x =>\n  <a/>\n}\n\n<b/>\n",
            "val a = xs.map { x => <a/> }\n<b/>\n",
            // A second braced argument: scalac rejects `f { a }: x =>`.
            "f { a } { x =>\n  b\n}\n",
            "f { a } {\n  case x => b\n}\n",
            // In a `for` generator.
            "for c <- xs.collectFirst {\n    case 1 => 1\n  }\ndo f(c)\n",
        ] {
            assert_eq!(changed(same, fewer), None, "{same}");
        }
    }

    #[test]
    fn format_off_is_left_alone() {
        let source = "// format: off\nimport a._\n// format: on\nimport b._\n";
        assert_eq!(
            rewritten(source, NEW),
            "// format: off\nimport a._\n// format: on\nimport b.*\n"
        );
    }

    #[test]
    fn format_off_keeps_braces() {
        // Each one's braces would go, or come back, outside `format: off`.
        let unchanged = [
            ("lazy val a = p.settings(f {\n  x\n})\n", NEW),
            ("xs.foreach {\n  x\n}\n", NEW),
            ("def f = {\n  a\n  b\n}\n", NEW),
            ("object A {\n  val a = 1\n}\n", NEW),
            ("xs.foreach:\n  x\n", OLD),
            ("def f =\n  a\n  b\n", OLD),
        ];
        for (code, dialect) in unchanged {
            assert_ne!(rewritten(code, dialect), code, "{code}");
            let source = format!("// format: off\n{code}// format: on\nval b = 1\n");
            assert_eq!(rewritten(&source, dialect), source, "{code}");
        }
    }

    #[test]
    fn braces_half_in_format_off_stay() {
        let source = "def f = {\n  a\n  // format: off\n  b\n}\n";
        assert_eq!(rewritten(source, NEW), source);
    }
}
