//! `S004` `no_null`, on by default.
//!
//! ## What it does
//!
//! Flags `null`: as a value, in a comparison, or as a pattern. The `Null`
//! type, as in `T | Null` or `T >: Null`, isn't flagged.
//!
//! ## Why is this bad?
//!
//! Any reference can hold `null` without its type saying so, so the error
//! shows up as a `NullPointerException` far from where the `null` came
//! from. An `Option` says in the type that a value may be missing, and the
//! compiler makes callers deal with it. A `null` from Java can be wrapped
//! where it comes in, with `Option(x)`.
//!
//! ## Example
//!
//! ```scala
//! def find(name: String): User =
//!   if users.contains(name) then users(name) else null
//! ```
//!
//! Use instead:
//!
//! ```scala
//! def find(name: String): Option[User] =
//!   users.get(name)
//! ```

use crate::lossless_tree::{SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`null`";

pub(super) const MESSAGE: &str = "`null` is not allowed; use an `Option`";

/// The `null`, if `element` is one. It's a leaf, so the tree holds it as a
/// token.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    (element.kind() == SyntaxKind::NullLiteral).then(|| element.text_range())
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_null() {
        let source = "\
object A:
  val s: String = null
  def f(x: String) = if x == null then 0 else 1
  def g(x: Any) = x match
    case null => 0
    case _    => 1
  f(null, null)
";
        assert_eq!(
            flagged(Rule::NoNull, source),
            ["2:19", "3:30", "5:10", "7:5", "7:11"]
        );
    }

    #[test]
    fn passes_the_null_type_and_names_with_null() {
        let source = "\
object A:
  def k[T >: Null](t: T | Null) = t
  val nullable = \"null\"
  // null
";
        assert_eq!(flagged(Rule::NoNull, source), Vec::<String>::new());
    }
}
