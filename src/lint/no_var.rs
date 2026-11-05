//! `S002` `no_var`, opt-in.
//!
//! ## What it does
//!
//! Flags `var`: a definition, an abstract declaration, or a class
//! parameter. It points at the keyword, after any modifiers.
//!
//! ## Why is this bad?
//!
//! State in a `val` can't change under the code that reads it, so the code
//! is easier to follow and safe to share between threads. It's opt-in
//! because a local `var` is common, and harmless, in performance-minded
//! code such as a loop's counter.
//!
//! ## Example
//!
//! ```scala
//! var total = 0
//! for x <- xs do total += x
//! ```
//!
//! Use instead:
//!
//! ```scala
//! val total = xs.sum
//! ```

use crate::lossless_tree::{SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`var` definitions, declarations and class parameters";

pub(super) const MESSAGE: &str = "`var` is not allowed; use a `val`";

/// The `var` keyword, if `element` is one: in a `var_definition`, a
/// `var_declaration` or a `class_parameter`.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    (element.kind() == SyntaxKind::VarKw).then(|| element.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_var_keyword() {
        let source = "\
trait T:
  var a: Int
class C(var b: Int, val c: Int):
  private var d, e = 2
  def f =
    var g = 1
    g
";
        assert_eq!(flagged(Rule::NoVar, source), ["2:3", "3:9", "4:11", "6:5"]);
    }

    #[test]
    fn passes_vals_and_names_with_var() {
        let source = "object A:\n  val variable = 1\n  def f(vars: Int*) = vars\n  // var\n";
        assert_eq!(flagged(Rule::NoVar, source), Vec::<String>::new());
    }
}
