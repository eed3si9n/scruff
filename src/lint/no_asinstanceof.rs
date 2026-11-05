//! `S006` `no_asinstanceof`, opt-in.
//!
//! ## What it does
//!
//! Flags `asInstanceOf`, as in `x.asInstanceOf[T]`, `asInstanceOf[T]` on
//! `this`, or `` x `asInstanceOf` y ``. It points at the name.
//!
//! ## Why is this bad?
//!
//! A cast tells the compiler to trust that a value has a type, without
//! checking it: a wrong one fails at run time with a
//! `ClassCastException`, or, for a generic type whose argument is erased,
//! not at all until the value is used, far from the cast. A pattern match
//! checks the type and says what to do when it's something else. It's
//! opt-in because libraries cast on purpose, where the types prove what
//! the compiler can't.
//!
//! ## Example
//!
//! ```scala
//! def length(x: Any): Int = x.asInstanceOf[String].length
//! ```
//!
//! Use instead:
//!
//! ```scala
//! def length(x: Any): Int = x match
//!   case s: String => s.length
//!   case _         => 0
//! ```

use crate::lossless_tree::{SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "casts with `asInstanceOf`";

pub(super) const MESSAGE: &str = "`asInstanceOf` is not allowed; use a pattern match";

/// The name, if `element` is `asInstanceOf`, backticked or not. `Any`
/// defines it as final, so code can't define one of its own: every use is
/// the cast.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let is_cast = element.kind() == SyntaxKind::Identifier
        && matches!(element.text(), "asInstanceOf" | "`asInstanceOf`");
    is_cast.then(|| element.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_cast() {
        let source = "\
class A:
  def f(x: Any) = x.asInstanceOf[String]
  def g = asInstanceOf[A]
  def h(x: Any) = x
    .asInstanceOf[List[Int]]
    .head
  def m(x: Any) = x `asInstanceOf` 1
";
        assert_eq!(
            flagged(Rule::NoAsInstanceOf, source),
            ["2:21", "3:11", "5:6", "7:21"]
        );
    }

    #[test]
    fn passes_type_tests_and_names_with_it() {
        let source = "\
class A:
  def k(x: Any) = x.isInstanceOf[String]
  val asInstanceOfCount = \"asInstanceOf\"
  // asInstanceOf
";
        assert_eq!(flagged(Rule::NoAsInstanceOf, source), Vec::<String>::new());
    }
}
