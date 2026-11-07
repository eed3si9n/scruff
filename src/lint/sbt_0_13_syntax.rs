//! `S009` `sbt_0_13_syntax`, opt-in.
//!
//! ## What it does
//!
//! Flags sbt 0.13's scoping with `in`, as in `scalacOptions in Compile` or
//! `key.in(Compile)`. It points at `in`.
//!
//! `in` is flagged only when its scope is a name, a tuple, a selection or a
//! call, as `Compile`, `(Compile, console)`, `Configurations.Test` or
//! `LocalProject("core")`. Not flagged: `project in file("core")` and
//! `project.in(...)`, which set a project's directory, and `in` with a
//! block, a lambda or a literal, or on a string, as in ScalaTest's
//! `"works" in { ... }`.
//!
//! ## Why is this bad?
//!
//! sbt 1.5 deprecated `in` for the slash syntax, and sbt 2 removes it.
//!
//! ## Example
//!
//! ```scala
//! scalacOptions in (Compile, console) := Nil
//! ```
//!
//! Use instead:
//!
//! ```scala
//! Compile / console / scalacOptions := Nil
//! ```
//!
//! ## Fix
//!
//! `--fix` writes the slash syntax, with a tuple's parts in its order:
//! `key in (Compile, console)` becomes `Compile / console / key`. It
//! leaves a chain such as `key in Compile in core`, where syntax can't
//! tell the project from the configuration, a key that isn't a name or a
//! selection, and code with a comment inside.
//!
//! ## References
//!
//! - [Migrating from sbt 0.13.x](https://www.scala-sbt.org/1.x/docs/Migrating-from-sbt-013x.html)
//! - [Migrating from sbt 1.x](https://www.scala-sbt.org/2.x/docs/en/changes/migrating-from-sbt-1.x.html)

use super::{Edit, Finding, Fix};
use crate::lossless_tree::{SyntaxElement, SyntaxKind, SyntaxNode, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "sbt 0.13's `key in Scope`";

pub(super) const MESSAGE: &str =
    "`in` is sbt 0.13 syntax, which sbt 2 drops; use the slash syntax, `Compile / key`";

/// `in`, if `element` scopes a key with it, with the slash syntax to put
/// in its place when [`slash_syntax`] can write it.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<Finding> {
    let node = element.as_node()?;
    let scoping = Scoping::of(element)?;
    if !is_scoping(&scoping.key, &scoping.scope) {
        return None;
    }
    Some(Finding {
        range: scoping.in_range,
        fix: slash_syntax(node, scoping).map(Fix::from),
    })
}

/// The slash syntax in place of `key in scope`, with the scope's parts in
/// their order: `key in (Compile, console)` is `Compile / console / key`.
/// sbt orders a tuple's parts as the slash syntax does, project,
/// configuration, then task.
///
/// Only a key that's a name or a selection, `Keys.fork`, is fixed, with
/// parts that are names, selections or calls; the others could hold an
/// operator that binds looser than `/`. A chain such as `key in Compile in
/// core` is left, since syntax can't tell which part is the project. So
/// is one with a comment inside.
fn slash_syntax(node: &SyntaxNode<'_>, scoping: Scoping<'_>) -> Option<Edit> {
    let parts = match scoping.scope.kind() {
        SyntaxKind::TupleExpression => operands(&scoping.scope.into_node()?).collect(),
        _ => vec![scoping.scope],
    };
    let commented = node
        .descendants_with_tokens()
        .any(|e| matches!(e.kind(), SyntaxKind::Comment | SyntaxKind::BlockComment));
    if !is_path(&scoping.key) || !(1..=3).contains(&parts.len()) || commented {
        return None;
    }
    let mut texts = Vec::new();
    for part in &parts {
        let call = part.kind() == SyntaxKind::CallExpression
            && field(part, "function").is_some_and(|f| is_path(&f));
        if !call && !is_path(part) {
            return None;
        }
        texts.push(part.text());
    }
    texts.push(scoping.key.text());
    let slash = texts.join(" / ");
    // `key.in(Compile)` binds tighter than `/`, as in `key.in(Compile).value`.
    let replacement = if scoping.method && needs_parentheses(node) {
        format!("({slash})")
    } else {
        slash
    };
    Some(Edit {
        range: node.text_range(),
        replacement,
    })
}

/// `key in scope` or `key.in(scope)`, taken apart.
struct Scoping<'t> {
    key: SyntaxElement<'t>,
    scope: SyntaxElement<'t>,
    /// Where `in` is.
    in_range: TextRange,
    /// Whether it's `key.in(scope)`.
    method: bool,
}

impl<'t> Scoping<'t> {
    fn of(element: &SyntaxElement<'t>) -> Option<Scoping<'t>> {
        let node = element.as_node()?;
        match node.kind() {
            SyntaxKind::InfixExpression => {
                let operator = node.child_by_field_name("operator")?;
                (operator.text() == "in").then_some(())?;
                Some(Scoping {
                    key: node.child_by_field_name("left")?,
                    scope: node.child_by_field_name("right")?,
                    in_range: operator.text_range(),
                    method: false,
                })
            }
            SyntaxKind::CallExpression => {
                let function = node.child_by_field_name("function")?;
                let name = field(&function, "field")?;
                if function.kind() != SyntaxKind::FieldExpression || name.text() != "in" {
                    return None;
                }
                let arguments = node.child_by_field_name("arguments")?.into_node()?;
                let mut arguments = operands(&arguments);
                let scope = arguments.next()?;
                arguments.next().is_none().then_some(())?;
                Some(Scoping {
                    key: field(&function, "value")?,
                    scope,
                    in_range: name.text_range(),
                    method: true,
                })
            }
            _ => None,
        }
    }
}

/// The expressions in an argument list or a tuple.
fn operands<'t>(node: &SyntaxNode<'t>) -> impl Iterator<Item = SyntaxElement<'t>> {
    node.children_with_tokens().filter(|e| {
        e.kind().is_named()
            && !matches!(
                e.kind(),
                SyntaxKind::Whitespace | SyntaxKind::Comment | SyntaxKind::BlockComment
            )
    })
}

/// Whether `element` is a name or a selection of one, as `Keys.fork`.
fn is_path(element: &SyntaxElement<'_>) -> bool {
    match element.kind() {
        SyntaxKind::Identifier => true,
        SyntaxKind::FieldExpression => field(element, "value").is_some_and(|v| is_path(&v)),
        _ => false,
    }
}

/// Whether `a / b` in place of `node` needs parentheses to keep `node`'s
/// grouping: anywhere but where an operator looser than `/` takes it, such
/// as `:=` or `in`, or where it stands alone.
fn needs_parentheses(node: &SyntaxNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SyntaxKind::InfixExpression => !parent
            .child_by_field_name("operator")
            .is_some_and(|operator| binds_looser_than_slash(operator.text())),
        SyntaxKind::Arguments
        | SyntaxKind::ParenthesizedExpression
        | SyntaxKind::TupleExpression
        | SyntaxKind::Block
        | SyntaxKind::IndentedBlock
        | SyntaxKind::TemplateBody
        | SyntaxKind::CompilationUnit
        | SyntaxKind::ValDefinition => false,
        _ => true,
    }
}

/// Whether Scala gives `operator` a lower precedence than `/`: a letter
/// operator, as `in`, or an assignment operator, as `:=` or `++=`.
fn binds_looser_than_slash(operator: &str) -> bool {
    let letter = operator
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '`');
    let assignment = operator.ends_with('=')
        && !operator.starts_with('=')
        && !matches!(operator, "<=" | ">=" | "!=");
    letter || assignment
}

fn field<'t>(element: &SyntaxElement<'t>, name: &str) -> Option<SyntaxElement<'t>> {
    element.as_node()?.child_by_field_name(name)
}

/// Whether `receiver in scope` scopes a key, rather than set a project's
/// directory or be some other `in`, such as ScalaTest's.
fn is_scoping(receiver: &SyntaxElement<'_>, scope: &SyntaxElement<'_>) -> bool {
    let scope_like = match scope.kind() {
        SyntaxKind::Identifier | SyntaxKind::TupleExpression | SyntaxKind::FieldExpression => true,
        SyntaxKind::CallExpression => {
            field(scope, "function").is_none_or(|function| function.text() != "file")
        }
        _ => false,
    };
    scope_like && receiver.kind() != SyntaxKind::String && !is_project(receiver)
}

/// Whether `element` is `project`, `Project(...)`, or a call or selection
/// on one, as `project.settings(...)`.
fn is_project(element: &SyntaxElement<'_>) -> bool {
    match element.kind() {
        SyntaxKind::Identifier => matches!(element.text(), "project" | "Project"),
        SyntaxKind::CallExpression | SyntaxKind::GenericFunction => {
            field(element, "function").is_some_and(|f| is_project(&f))
        }
        SyntaxKind::FieldExpression => field(element, "value").is_some_and(|v| is_project(&v)),
        SyntaxKind::ParenthesizedExpression => element
            .as_node()
            .and_then(|n| n.children().next())
            .is_some_and(|inner| is_project(&SyntaxElement::Node(inner))),
        SyntaxKind::InfixExpression => field(element, "left").is_some_and(|l| is_project(&l)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, fixed, flagged};

    #[test]
    fn flags_in_scoping() {
        let source = "\
scalacOptions in (Compile, console) := Nil
fork in run := true
val cp = (fullClasspath in Test).value
x.in(Compile) := 1
y in Configurations.Test := 2
z in LocalProject(\"core\") := 3
";
        assert_eq!(
            flagged(Rule::Sbt013Syntax, source),
            ["1:15", "2:6", "3:25", "4:3", "5:3", "6:3"]
        );
    }

    #[test]
    fn passes_slash_syntax_project_directories_and_other_ins() {
        let source = "\
Compile / console / scalacOptions := Nil
a := b.value
a <<= b
lazy val core = project in file(\"core\")
lazy val util = project.in(file(\"util\"))
lazy val app = (project in file(\"app\")).settings(name := \"app\")
lazy val web = Project(\"web\", file(\"web\")) in base
lazy val api = project.settings(name := \"api\").in(base)
\"works\" in { assert(true) }
x in 1
";
        assert_eq!(flagged(Rule::Sbt013Syntax, source), Vec::<String>::new());
    }

    #[test]
    fn fixes_to_the_slash_syntax_in_the_tuple_order() {
        let source = "\
scalacOptions in (Compile, console) := Nil
fork in run := true
Keys.fork in Test := true
z in LocalProject(\"core\") := 3
y in Configurations.Test += 2
val cp = (fullClasspath in Test).value
x.in(Compile) := 1
val v = x.in(Compile).value
f(x.in(Compile))
";
        assert_eq!(
            fixed(Rule::Sbt013Syntax, source),
            "\
Compile / console / scalacOptions := Nil
run / fork := true
Test / Keys.fork := true
LocalProject(\"core\") / z := 3
Configurations.Test / y += 2
val cp = (Test / fullClasspath).value
Compile / x := 1
val v = (Compile / x).value
f(Compile / x)
"
        );
    }

    #[test]
    fn leaves_what_it_cannot_order_or_group() {
        let source = "\
a in Compile in core := 1
(b ++ c) in Compile := 2
d in (Compile /* main */) := 3
";
        assert_eq!(
            fixed(Rule::Sbt013Syntax, source),
            "\
Compile / a in core := 1
(b ++ c) in Compile := 2
d in (Compile /* main */) := 3
"
        );
    }
}
