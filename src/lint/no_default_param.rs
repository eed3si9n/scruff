//! `S003` `no_default_param`, opt-in.
//!
//! ## What it does
//!
//! Flags a parameter's default value, in a method, a constructor, a class,
//! an enum, an extension, a given, or a `using` or `implicit` clause. It
//! points at the `=`.
//!
//! ## Why is this bad?
//!
//! A default value is compiled into a method of its own, which callers call
//! from their own code. Adding, removing or reordering parameters with
//! defaults breaks binary compatibility, so a library can't evolve them
//! without breaking code compiled against it. Only one overload of a method
//! may have defaults, and an override can't change them. It's opt-in
//! because they're common, and harmless, in application code.
//!
//! ## Example
//!
//! ```scala
//! def connect(host: String, port: Int = 80): Connection = ???
//! ```
//!
//! Use instead:
//!
//! ```scala
//! def connect(host: String): Connection = connect(host, 80)
//! def connect(host: String, port: Int): Connection = ???
//! ```

use crate::lossless_tree::{NodeOrToken, SyntaxElement, SyntaxKind, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "default values of parameters";

pub(super) const MESSAGE: &str =
    "default parameter values are not allowed; add an overload instead";

/// From the `=` to the end of the default value, if `element` is a
/// parameter with one.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let node = element.as_node()?;
    if !matches!(
        node.kind(),
        SyntaxKind::Parameter | SyntaxKind::ClassParameter
    ) {
        return None;
    }
    let value = node.child_by_field_name("default_value")?;
    // The parameter's only `=` token; a type's would be inside a child node.
    let eq = node
        .children_with_tokens()
        .filter_map(NodeOrToken::into_token)
        .find(|token| token.kind() == SyntaxKind::Eq)?;
    Some(TextRange::new(
        eq.text_range().start(),
        value.text_range().end(),
    ))
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_default_value() {
        let source = "\
class C(val a: Int = 1, b: Int)(using c: Int = 2):
  def f(x: Int, y: String = \"y\")(implicit z: Int = 3) = x
  def this(s: String = \"\") = this(1, 2)
enum E(v: Int = 0):
  case A
extension (q: Int)
  def g(r: Int = 4) = q
given G(using t: Int = 5): AnyRef with {}
";
        assert_eq!(
            flagged(Rule::NoDefaultParam, source),
            [
                "1:20", "1:46", "2:27", "2:50", "3:22", "4:15", "7:16", "8:22"
            ]
        );
    }

    #[test]
    fn passes_parameters_without_defaults() {
        let source = "\
class C(a: Int, f: Int => Int):
  def g(x: Int, y: Option[Int])(using Ordering[Int]): Int = x
  val h = (p: Int) => p
  def k[T >: Null](t: T) = t
";
        assert_eq!(flagged(Rule::NoDefaultParam, source), Vec::<String>::new());
    }
}
