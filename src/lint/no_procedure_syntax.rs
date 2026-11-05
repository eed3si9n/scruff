//! `S008` `no_procedure_syntax`, on by default.
//!
//! ## What it does
//!
//! Flags procedure syntax: a method whose body follows its parameters
//! without `=`, as in `def f() { ... }`, and an abstract method without a
//! result type, as in `def f()`. It points at the method's name.
//! Auxiliary constructors, `def this(...) { ... }`, aren't flagged.
//!
//! ## Why is this bad?
//!
//! The Scala 3 reference lists it under dropped features: "Procedure
//! syntax `def f() { ... }` has been dropped." Scala 3 accepts it only
//! under `-source:3.0-migration`.
//!
//! ## Example
//!
//! ```scala
//! def run() { println("running") }
//! ```
//!
//! Use instead:
//!
//! ```scala
//! def run(): Unit = { println("running") }
//! ```
//!
//! ## References
//!
//! - [Dropped: Procedure Syntax](https://docs.scala-lang.org/scala3/reference/dropped-features/procedure-syntax.html)

use crate::lossless_tree::{NodeOrToken, SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`def f() { ... }` and `def f()` without a result type";

pub(super) const MESSAGE: &str =
    "procedure syntax is dropped in Scala 3; write the result type, `: Unit =`";

/// The method's name, if `element` is a method in procedure syntax.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let node = element.as_node()?;
    let procedure = match node.kind() {
        // The `=` is a token of the definition itself; one inside the body
        // or a default value belongs to a child node.
        SyntaxKind::FunctionDefinition => !node
            .children_with_tokens()
            .filter_map(NodeOrToken::into_token)
            .any(|token| token.kind() == SyntaxKind::Eq),
        SyntaxKind::FunctionDeclaration => node.child_by_field_name("return_type").is_none(),
        _ => false,
    };
    let name = node.child_by_field_name("name")?;
    (procedure && name.text() != "this").then(|| name.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_definitions_without_eq_and_declarations_without_a_type() {
        let source = "\
abstract class A {
  def p() { println(1) }
  def q { println(2) }
  def u[T](x: T = null)(implicit o: Ordering[T]) { () }
  def t()
}
";
        assert_eq!(
            flagged(Rule::NoProcedureSyntax, source),
            ["2:7", "3:7", "4:7", "5:7"]
        );
    }

    #[test]
    fn passes_methods_with_eq_or_a_type_and_constructors() {
        let source = "\
abstract class A {
  def r(): Unit = { println(3) }
  def s(): Unit
  def v = 1
  def w(x: Int = 1) = x
  def this(x: Int) { this() }
}
";
        assert_eq!(
            flagged(Rule::NoProcedureSyntax, source),
            Vec::<String>::new()
        );
    }
}
