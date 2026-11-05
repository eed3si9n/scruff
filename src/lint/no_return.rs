//! `S001` `no_return`, on by default.
//!
//! ## What it does
//!
//! Flags `return`.
//!
//! ## Why is this bad?
//!
//! In Scala a method's result is its last expression, so `return` is never
//! needed to give one. Inside a lambda, `return` leaves the enclosing method
//! by throwing an exception, which a `try` or another thread can catch on
//! the way; Scala 3 deprecates it there.
//!
//! ## Example
//!
//! ```scala
//! def sign(x: Int): Int =
//!   if x < 0 then return -1
//!   1
//! ```
//!
//! Use instead:
//!
//! ```scala
//! def sign(x: Int): Int =
//!   if x < 0 then -1 else 1
//! ```

use crate::lossless_tree::{SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`return` statements";

pub(super) const MESSAGE: &str = "`return` is not allowed; make the result the last expression";

/// The whole `return` expression, if `element` is one.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    (element.kind() == SyntaxKind::ReturnExpression).then(|| element.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_return() {
        let source = "\
object A:
  def f(x: Int): Int =
    if x > 0 then return 1
    return
  def g(xs: List[Int]): Int =
    xs.foreach { x => return x }
    0
";
        assert_eq!(flagged(Rule::NoReturn, source), ["3:19", "4:5", "6:23"]);
    }

    #[test]
    fn passes_code_without_one() {
        let source = "object A:\n  def f(x: Int): Int = x + 1\n  val returns = 1\n  // return\n";
        assert_eq!(flagged(Rule::NoReturn, source), Vec::<String>::new());
    }
}
