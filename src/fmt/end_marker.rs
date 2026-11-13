//! Which statements get a Scala 3 end marker when they're long.
//!
//! As scalafmt's `rewrite.scala3.endMarker.insert`: a statement gets one
//! if it owns a region of significant indentation, as `def f =` with
//! statements indented below or `class A:`, and is a statement of a
//! template, a package, the file, or a block of more than one statement.
//! Adding a marker to a block's only statement would make it a block of
//! two. How long is long is the printer's to say, as it depends on where
//! lines break: see [`crate::doc::Doc::EndMarker`]. The marker closes the
//! statement's last block as a `}` would, so the line breaks count from
//! the line that block opens on, as with scalafmt's `spanHas =
//! lastBlockOnly`: the `else` of an `if`, not its `then`.
//!
//! A marker already there is left to the printer too, so a short
//! statement loses it and the result doesn't depend on whether the source
//! had one. As in scalafmt, not where a comment is before the marker or on
//! its line, as the comment would move out of the region, nor where the
//! marker doesn't name its statement: those stay as written.

use std::borrow::Cow;

use crate::lossless_tree::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextSize,
};

/// A marker a statement may get.
pub(super) struct EndMarker<'t> {
    /// As `f` for `end f`.
    pub name: Cow<'t, str>,
    /// Where its last block's first token starts.
    pub last_block: Option<TextSize>,
    /// The marker the source has, which the printer stands in for.
    pub existing: Option<SyntaxNode<'t>>,
}

/// The statement's marker, if it can take one, or has one the printer may
/// drop. Decided from the source alone.
pub(super) fn end_marker<'t>(node: SyntaxNode<'t>) -> Option<EndMarker<'t>> {
    let name = own_region_name(node)?;
    if !is_statement(node) {
        return None;
    }
    let existing = match trailing_marker(node) {
        Some(marker) => {
            let named = marker.text().strip_prefix("end").map(str::trim) == Some(&*name)
                && marker.end_marker_matches_parent();
            let commented = marker.tokens().any(|t| {
                t.leading_trivia()
                    .chain(t.trailing_trivia())
                    .any(|c| c.is_comment())
            });
            (named && !commented).then_some(marker)?;
            Some(marker)
        }
        None => None,
    };
    Some(EndMarker {
        name,
        last_block: last_block(node)
            .as_ref()
            .and_then(block_start)
            .map(|t| t.text_range().start()),
        existing,
    })
}

/// The block the marker closes: a definition's body, an `if`'s `else`
/// branch or else its `then` one, a `try`'s `finally` or else its `catch`
/// cases or else its body.
fn last_block<'t>(node: SyntaxNode<'t>) -> Option<SyntaxElement<'t>> {
    let field = |name| node.child_by_field_name(name);
    match node.kind() {
        SyntaxKind::IfExpression => field("alternative").or_else(|| field("consequence")),
        SyntaxKind::ValDefinition | SyntaxKind::VarDefinition => field("value"),
        SyntaxKind::InstanceExpression => node
            .children()
            .find(|c| c.kind() == SyntaxKind::TemplateBody)
            .map(NodeOrToken::Node),
        SyntaxKind::ExtensionDefinition => node
            .children_with_tokens()
            .find(|c| c.field_name() == Some("body")),
        SyntaxKind::TryExpression => {
            let clause = |kind| node.children().find(|c| c.kind() == kind);
            if let Some(finally) = clause(SyntaxKind::FinallyClause) {
                finally
                    .non_trivia_children()
                    .find(|c| c.as_token().is_none_or(|t| t.text() != "finally"))
            } else if let Some(catch) = clause(SyntaxKind::CatchClause) {
                catch
                    .children()
                    .find(|c| c.kind() == SyntaxKind::IndentedCases)
                    .map(NodeOrToken::Node)
            } else {
                field("body")
            }
        }
        _ => field("body"),
    }
}

/// The first token of a block's first statement, or of the expression
/// that's there instead.
fn block_start<'t>(block: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match block {
        NodeOrToken::Node(b)
            if matches!(
                b.kind(),
                SyntaxKind::TemplateBody
                    | SyntaxKind::EnumBody
                    | SyntaxKind::WithTemplateBody
                    | SyntaxKind::IndentedBlock
                    | SyntaxKind::IndentedCases
            ) =>
        {
            statements(*b).next().and_then(|s| first_token(&s))
        }
        other => first_token(other),
    }
}

/// The end marker the node ends in. tree-sitter-scala may attach
/// one to something inside, as the `end if` of an `if` with `else if`s to
/// the last `else if`.
fn trailing_marker(node: SyntaxNode<'_>) -> Option<SyntaxNode<'_>> {
    let marker = node
        .last_code_token()?
        .parent()
        .filter(|p| p.kind() == SyntaxKind::EndMarker)?;
    // Its own, not a statement's inside it, as the last class's in a
    // class body: on the node, or on its `else if`s.
    let mut owner = marker.parent()?;
    while owner != node {
        let is_else_if =
            owner.kind() == SyntaxKind::IfExpression && owner.field_name() == Some("alternative");
        if !is_else_if {
            return None;
        }
        owner = owner.parent()?;
    }
    Some(marker)
}

/// The name, if the node is a construct that owns a region of significant
/// indentation.
fn own_region_name<'t>(node: SyntaxNode<'t>) -> Option<Cow<'t, str>> {
    let field = |name| node.child_by_field_name(name);
    let named = || field("name").map(|n| Cow::Borrowed(n.text()));
    match node.kind() {
        SyntaxKind::ClassDefinition
        | SyntaxKind::ObjectDefinition
        | SyntaxKind::TraitDefinition
        | SyntaxKind::EnumDefinition
        | SyntaxKind::PackageObject => {
            unbraced_template(field("body")?).then_some(())?;
            named()
        }
        SyntaxKind::InstanceExpression => {
            node.children()
                .find(|c| c.kind() == SyntaxKind::TemplateBody)
                .filter(|&b| unbraced_template(NodeOrToken::Node(b)))?;
            Some(Cow::Borrowed("new"))
        }
        SyntaxKind::GivenDefinition => {
            let body = field("body")?;
            let owns = if body.kind() == SyntaxKind::WithTemplateBody {
                unbraced_template(body)
            } else {
                is_indented_statements(&body)
            };
            owns.then_some(())?;
            Some(named().unwrap_or(Cow::Borrowed("given")))
        }
        SyntaxKind::FunctionDefinition => {
            is_indented_statements(&field("body")?).then_some(())?;
            named()
        }
        SyntaxKind::ValDefinition | SyntaxKind::VarDefinition => {
            is_indented_statements(&field("value")?).then_some(())?;
            let pattern = field("pattern")?;
            if pattern.kind() == SyntaxKind::Identifier {
                Some(Cow::Borrowed(pattern.text()))
            } else if node.kind() == SyntaxKind::ValDefinition {
                Some(Cow::Borrowed("val"))
            } else {
                // `end var` isn't Scala.
                None
            }
        }
        SyntaxKind::ExtensionDefinition => {
            // Its methods not in braces.
            node.children_with_tokens()
                .find(|c| c.field_name() == Some("body"))?;
            let braced = node
                .children()
                .any(|c| c.kind() == SyntaxKind::TemplateBody)
                || has_token(node, "{");
            (!braced).then_some(Cow::Borrowed("extension"))
        }
        // Control syntax: a body after `then`, `do`, `try` and the like is
        // a region, even of one expression, unless it's in braces.
        SyntaxKind::IfExpression => if_owns_region(node).then_some(Cow::Borrowed("if")),
        SyntaxKind::WhileExpression => {
            let owns = field("body").is_some_and(|b| !is_braced(&b))
                || field("condition").is_some_and(|c| is_indented_statements(&c));
            owns.then_some(Cow::Borrowed("while"))
        }
        SyntaxKind::ForExpression => {
            let body = field("body");
            let enumerators = field("enumerators");
            let yields = has_token(node, "yield");
            let owns = if yields {
                body.is_some_and(|b| is_indented_statements(&b))
            } else {
                body.is_some_and(|b| !is_braced(&b))
                    && (has_token(node, "do")
                        || enumerators
                            .as_ref()
                            .and_then(first_token)
                            .is_some_and(|t| t.text() == "("))
            };
            // Enumerators on lines of their own, without parentheses.
            let unbraced_enumerators = enumerators.is_some_and(|e| {
                e.as_node().is_some_and(|e| {
                    e.children()
                        .filter(|c| c.kind() == SyntaxKind::Enumerator)
                        .nth(1)
                        .is_some()
                }) && first_token(&e).is_some_and(|t| !matches!(t.text(), "(" | "{"))
            });
            (owns || unbraced_enumerators).then_some(Cow::Borrowed("for"))
        }
        SyntaxKind::MatchExpression => {
            // tree-sitter-scala reads `x = y match` as `(x = y) match`;
            // Scala reads it as `x = (y match)`, an assignment, which
            // takes no marker.
            let assigned = field("value")?.kind() == SyntaxKind::AssignmentExpression;
            (!assigned && field("body")?.kind() == SyntaxKind::IndentedCases)
                .then_some(Cow::Borrowed("match"))
        }
        SyntaxKind::TryExpression => {
            let body = field("body").is_some_and(|b| !is_braced(&b));
            let clauses = node.children().any(|c| match c.kind() {
                SyntaxKind::CatchClause => {
                    c.children().any(|x| x.kind() == SyntaxKind::IndentedCases)
                }
                SyntaxKind::FinallyClause => c
                    .children_with_tokens()
                    .filter(|x| {
                        !x.is_trivia() && x.as_token().is_none_or(|t| t.text() != "finally")
                    })
                    .any(|x| !is_braced(&x)),
                _ => false,
            });
            (body || clauses).then_some(Cow::Borrowed("try"))
        }
        _ => None,
    }
}

/// An `if` with `then`, as `then` starts a region; or in the old syntax,
/// one with a branch of indented statements, or an `else if` that has one.
fn if_owns_region(node: SyntaxNode<'_>) -> bool {
    let consequence = node.child_by_field_name("consequence");
    if has_token(node, "then") {
        return consequence.is_some_and(|c| !is_braced(&c));
    }
    consequence.is_some_and(|c| is_indented_statements(&c))
        || node
            .child_by_field_name("alternative")
            .is_some_and(|a| match a {
                NodeOrToken::Node(a) if a.kind() == SyntaxKind::IfExpression => if_owns_region(a),
                a => is_indented_statements(&a),
            })
}

/// Whether the node has the token among its own children, as `then` in an
/// `if`. tree-sitter-scala puts some under a field, as `condition`.
fn has_token(node: SyntaxNode<'_>, text: &str) -> bool {
    node.children_with_tokens()
        .any(|c| c.as_token().is_some_and(|t| t.text() == text))
}

/// A block or cases in braces.
fn is_braced(element: &SyntaxElement<'_>) -> bool {
    matches!(element.kind(), SyntaxKind::Block | SyntaxKind::CaseBlock)
        && first_token(element).is_some_and(|t| t.text() == "{")
}

/// A class body, an enum's, or a given's `with` body, opened by `:` or
/// `with` and indented, not in braces.
fn unbraced_template(body: SyntaxElement<'_>) -> bool {
    let NodeOrToken::Node(body) = body else {
        return false;
    };
    matches!(
        body.kind(),
        SyntaxKind::TemplateBody | SyntaxKind::EnumBody | SyntaxKind::WithTemplateBody
    ) && !body
        .children_with_tokens()
        .any(|c| c.as_token().is_some_and(|t| t.text() == "{"))
}

/// An indented block of more than one statement, or of one that isn't an
/// expression, such as a definition: what Scala 3 reads as a region
/// standing for braces. One expression indented below `=` is just that
/// expression.
fn is_indented_statements(element: &SyntaxElement<'_>) -> bool {
    let NodeOrToken::Node(block) = element else {
        return false;
    };
    if block.kind() != SyntaxKind::IndentedBlock {
        return false;
    }
    let mut statements = statements(*block);
    match (statements.next(), statements.next()) {
        (Some(_), Some(_)) => true,
        (Some(only), None) => !is_expression(&only),
        _ => false,
    }
}

fn is_expression(element: &SyntaxElement<'_>) -> bool {
    let name = element.kind().name();
    !(name.ends_with("_definition")
        || name.ends_with("_declaration")
        || matches!(
            element.kind(),
            SyntaxKind::ImportDeclaration | SyntaxKind::ExportDeclaration
        ))
}

/// Whether the node is a statement where a marker may follow it: in a
/// template, a package, the file, or a block of more than one statement.
fn is_statement(node: SyntaxNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SyntaxKind::CompilationUnit
        | SyntaxKind::TemplateBody
        | SyntaxKind::EnumBody
        | SyntaxKind::WithTemplateBody => true,
        SyntaxKind::Block | SyntaxKind::IndentedBlock => statements(parent).nth(1).is_some(),
        // A `case`'s statements, after its `=>`, and an extension's methods.
        SyntaxKind::CaseClause | SyntaxKind::ExtensionDefinition => {
            node.field_name() == Some("body")
                && parent
                    .children_with_tokens()
                    .filter(|c| c.field_name() == Some("body"))
                    .nth(1)
                    .is_some()
        }
        _ => false,
    }
}

/// A block's statements: what's named in it, but not an end marker.
fn statements(block: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxElement<'_>> {
    block
        .non_trivia_children()
        .filter(|c| c.kind().is_named() && c.kind() != SyntaxKind::EndMarker)
}

fn first_token<'t>(element: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match element {
        NodeOrToken::Token(t) => Some(*t),
        NodeOrToken::Node(n) => n.tokens().find(|t| !t.is_trivia()),
    }
}
