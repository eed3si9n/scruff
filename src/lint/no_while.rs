//! `S005` `no_while`, opt-in.
//!
//! ## What it does
//!
//! Flags `while` loops, and Scala 2's `do … while`. It points at the
//! loop's first keyword.
//!
//! ## Why is this bad?
//!
//! A `while` loop does its work through side effects, usually on a `var`,
//! so what it computes is spread over the loop instead of being a value.
//! A collection method or a tail-recursive function says the same as an
//! expression. It's opt-in because `while` is the fastest loop on the JVM,
//! and common, with `var`, in performance-minded code.
//!
//! ## Example
//!
//! ```scala
//! var i = 0
//! var total = 0
//! while i < xs.length do
//!   total += xs(i)
//!   i += 1
//! ```
//!
//! Use instead:
//!
//! ```scala
//! val total = xs.sum
//! ```

use crate::lossless_tree::{SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`while` and `do … while` loops";

pub(super) const MESSAGE: &str =
    "`while` is not allowed; use a collection method or a recursive function";

/// The loop's first keyword, if `element` is a `while` or `do … while`.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let node = element.as_node()?;
    if !matches!(
        node.kind(),
        SyntaxKind::WhileExpression | SyntaxKind::DoWhileExpression
    ) {
        return None;
    }
    let keyword = node.non_trivia_children().next()?;
    Some(keyword.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_while_loop() {
        let source = "\
object A:
  def f(n: Int) =
    while n > 0 do println(n)
    while (n > 0) { println(n) }
    while
      n > 0
    do
      println(n)
";
        assert_eq!(flagged(Rule::NoWhile, source), ["3:5", "4:5", "5:5"]);
    }

    #[test]
    fn flags_do_while_at_do() {
        let source = "object B {\n  def g(n: Int) = do { println(n) } while (n > 0)\n}\n";
        assert_eq!(flagged(Rule::NoWhile, source), ["2:19"]);
    }

    #[test]
    fn passes_names_and_strings_with_while() {
        let source = "object A:\n  val whileLoop = \"while\"\n  // while\n";
        assert_eq!(flagged(Rule::NoWhile, source), Vec::<String>::new());
    }
}
