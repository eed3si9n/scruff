//! `S008` `procedure_syntax`, on by default.
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
//! ## Fix
//!
//! `--fix` writes the result type, `: Unit`, after the last parameter
//! clause, with `=` before the body: `def run() { ... }` becomes
//! `def run(): Unit = { ... }`, and `def run()` becomes `def run(): Unit`.
//! It leaves a declaration followed by a block on the next line, as in
//! `def run()` then `{ ... }`, since Scala 2 may read the two as one
//! method.
//!
//! ## References
//!
//! - [Dropped: Procedure Syntax](https://docs.scala-lang.org/scala3/reference/dropped-features/procedure-syntax.html)

use super::{Edit, Finding};
use crate::lossless_tree::{NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`def f() { ... }` and `def f()` without a result type";

pub(super) const MESSAGE: &str =
    "procedure syntax is dropped in Scala 3; write the result type, `: Unit =`";

/// The method's name, if `element` is a method in procedure syntax, with
/// `: Unit` to put in.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<Finding> {
    let node = element.as_node()?;
    let fix: fn(&SyntaxNode<'_>) -> Option<Edit> = match node.kind() {
        // The `=` is a token of the definition itself; one inside the body
        // or a default value belongs to a child node.
        SyntaxKind::FunctionDefinition => {
            let eq = node
                .children_with_tokens()
                .filter_map(NodeOrToken::into_token)
                .any(|token| token.kind() == SyntaxKind::Eq);
            if eq {
                return None;
            }
            definition_fix
        }
        SyntaxKind::FunctionDeclaration => {
            if node.child_by_field_name("return_type").is_some() {
                return None;
            }
            declaration_fix
        }
        _ => return None,
    };
    let name = node.child_by_field_name("name")?;
    if name.text() == "this" {
        return None;
    }
    Some(Finding {
        range: name.text_range(),
        fix: fix(node),
    })
}

/// `: Unit =` after the last parameter clause of a definition without `=`.
fn definition_fix(node: &SyntaxNode<'_>) -> Option<Edit> {
    if node.child_by_field_name("return_type").is_some() {
        return None;
    }
    let body = node.child_by_field_name("body")?;
    let head = node
        .non_trivia_children()
        .take_while(|child| child.text_range() != body.text_range())
        .last()?;
    Some(Edit::insert(head.text_range().end(), ": Unit ="))
}

/// `: Unit` after the last parameter clause of a declaration without a
/// result type.
fn declaration_fix(node: &SyntaxNode<'_>) -> Option<Edit> {
    if followed_by_block(node) {
        return None;
    }
    Some(Edit::insert(node.text_range().end(), ": Unit"))
}

/// Whether the next thing after `node`, past whitespace and comments, is a
/// block.
fn followed_by_block(node: &SyntaxNode<'_>) -> bool {
    std::iter::successors(node.next_sibling_or_token(), |e| e.next_sibling_or_token())
        .find(|e| !e.is_trivia())
        .is_some_and(|e| e.kind() == SyntaxKind::Block)
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, fixed, flagged};

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
            flagged(Rule::ProcedureSyntax, source),
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
        assert_eq!(flagged(Rule::ProcedureSyntax, source), Vec::<String>::new());
    }

    #[test]
    fn fixes_with_the_unit_result_type() {
        let source = "\
abstract class A {
  def p() /* c */ { println(1) }
  def q { println(2) }
  def u[T](x: T)(implicit o: Ordering[T]) { () }
  def t()  // hi
  def s
  def this(x: Int) { this() }
}
";
        assert_eq!(
            fixed(Rule::ProcedureSyntax, source),
            "\
abstract class A {
  def p(): Unit = /* c */ { println(1) }
  def q: Unit = { println(2) }
  def u[T](x: T)(implicit o: Ordering[T]): Unit = { () }
  def t(): Unit  // hi
  def s: Unit
  def this(x: Int) { this() }
}
"
        );
    }

    #[test]
    fn leaves_a_declaration_followed_by_a_block() {
        let source = "class A {\n  def q()\n  { 2 }\n}\n";
        assert_eq!(fixed(Rule::ProcedureSyntax, source), source);
    }
}
