//! `C001` `untyped_implicit_val`, on by default.
//!
//! ## What it does
//!
//! Flags an `implicit val`, `implicit lazy val` or `implicit var` without a
//! type, as a member or at the top level. It points at the name. Not
//! flagged: one in a local block, as in a method's body, one marked
//! `override`, and one in an anonymous class, `new M { ... }`.
//!
//! ## Why is this bad?
//!
//! Scala 3 rejects it, with "type of implicit definition needs to be given
//! explicitly". The Scala 3 reference says: "Types of implicit values and
//! result types of implicit methods must be explicitly declared. Excepted
//! are only values in local blocks where the type may still be inferred."
//!
//! Scala 3.3 and 3.8 also accept one that overrides a member, which takes
//! the member's type. Syntax shows that only when it says `override`. An
//! anonymous class's implicit members almost always implement its parent's,
//! as in `new Monoid[A] { implicit val F = F0 }`, so they're left alone,
//! though a new one there would go unflagged. Elsewhere, one that
//! implements an abstract member without `override` is still flagged; its
//! type can be written all the same.
//!
//! ## Example
//!
//! ```scala
//! implicit val ordering = Ordering.by[User, String](_.name)
//! ```
//!
//! Use instead:
//!
//! ```scala
//! implicit val ordering: Ordering[User] = Ordering.by[User, String](_.name)
//! ```
//!
//! ## References
//!
//! - [Changes in Implicit Resolution](https://docs.scala-lang.org/scala3/reference/changed-features/implicit-resolution.html)

use crate::lossless_tree::{SyntaxElement, SyntaxKind, SyntaxNode, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`implicit val` members without a type";

pub(super) const MESSAGE: &str = "an implicit value needs an explicit type; Scala 3 requires one unless it's local or overrides a member";

/// The name, if `element` is an implicit value without a type that's
/// not local, not marked `override`, and not in an anonymous class.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let node = element.as_node()?;
    let value = matches!(
        node.kind(),
        SyntaxKind::ValDefinition | SyntaxKind::VarDefinition
    );
    if !value
        || !has_modifier(node, SyntaxKind::ImplicitKw)
        || has_modifier(node, SyntaxKind::OverrideKw)
        || node.child_by_field_name("type").is_some()
        || is_local(node)
        || in_anonymous_class(node)
    {
        return None;
    }
    Some(node.child_by_field_name("pattern")?.text_range())
}

fn has_modifier(node: &SyntaxNode<'_>, keyword: SyntaxKind) -> bool {
    node.children()
        .filter(|child| child.kind() == SyntaxKind::Modifiers)
        .flat_map(|modifiers| modifiers.tokens())
        .any(|token| token.kind() == keyword)
}

/// Whether `node` is a member of `new M { ... }`.
fn in_anonymous_class(node: &SyntaxNode<'_>) -> bool {
    node.parent()
        .filter(|body| body.kind() == SyntaxKind::TemplateBody)
        .and_then(|body| body.parent())
        .is_some_and(|parent| parent.kind() == SyntaxKind::InstanceExpression)
}

/// Whether `node` is in a block or a `case`, rather than a member of a
/// class, object, trait, enum, given or package, or at the top level.
fn is_local(node: &SyntaxNode<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        !matches!(
            parent.kind(),
            SyntaxKind::CompilationUnit
                | SyntaxKind::TemplateBody
                | SyntaxKind::WithTemplateBody
                | SyntaxKind::EnumBody
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_members_and_top_level_values_without_a_type() {
        let source = "\
implicit val top = 1
object A {
  implicit val a = Ordering.Int
  implicit lazy val c = 1
  private implicit val d = 2
  implicit var e = 3
}
trait T:
  implicit val f = 4
package p {
  implicit val g = 5
}
object B {
  given G with { implicit val h = 6 }
  object D extends M { implicit val k = F0 }
}
";
        assert_eq!(
            flagged(Rule::UntypedImplicitVal, source),
            [
                "1:14", "3:16", "4:21", "5:24", "6:16", "9:16", "11:16", "14:31", "15:37"
            ]
        );
    }

    #[test]
    fn passes_typed_local_and_non_implicit_values() {
        let source = "\
object A {
  implicit val b: Ordering[Int] = Ordering.Int
  val f = 4
  implicit def g: Int = 5
  def h = { implicit val i = 6; i }
  def k =
    implicit val j = 7
    j
  def m(x: Int) = x match { case 1 => implicit val n = 2; n }
}
class C(implicit val x: Int)
trait M { implicit def F: Ordering[Int] }
object N extends M { override implicit val F = Ordering.Int }
object P {
  val a = new M { implicit val F = Ordering.Int }
  val b = new M with N {
    implicit val F = F0
  }
  val c = new M:
    implicit val F = F0
}
";
        assert_eq!(
            flagged(Rule::UntypedImplicitVal, source),
            Vec::<String>::new()
        );
    }
}
