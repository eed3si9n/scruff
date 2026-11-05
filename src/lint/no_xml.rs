//! `S007` `no_xml`, on by default.
//!
//! ## What it does
//!
//! Flags XML literals, as values, `<p>{text}</p>`, or as patterns,
//! `case <a>{t}</a> =>`. A literal inside another one's `{ … }` is part of
//! it, so only the outermost is flagged.
//!
//! ## Why is this bad?
//!
//! The Scala 3 reference lists them under dropped features: "XML Literals
//! are still supported, but will be dropped in the near future, to be
//! replaced with XML string interpolation."
//!
//! ## Example
//!
//! ```scala
//! val link = <a href={url}>{title}</a>
//! ```
//!
//! Use instead the XML string interpolator the reference points to,
//! `lampepfl/xml-interpolator`:
//!
//! ```scala
//! import dotty.xml.interpolator.*
//!
//! val link = xml"""<a href=${url}>${title}</a>"""
//! ```
//!
//! ## References
//!
//! - [Dropped: XML Literals](https://docs.scala-lang.org/scala3/reference/dropped-features/xml.html)

use crate::lossless_tree::{SyntaxElement, SyntaxKind, SyntaxNode, TextRange};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "XML literals and patterns";

pub(super) const MESSAGE: &str =
    "XML literals are not allowed; Scala 3 will drop them, use XML string interpolation";

/// The whole literal, if `element` is one and isn't inside another.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<TextRange> {
    let node = element.as_node()?;
    let outermost = is_xml(node) && !node.parent()?.ancestors().any(|a| is_xml(&a));
    outermost.then(|| node.text_range())
}

fn is_xml(node: &SyntaxNode<'_>) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::XmlExpression | SyntaxKind::XmlPattern
    )
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, flagged};

    #[test]
    fn flags_each_outermost_literal() {
        let source = "\
object A {
  val a = <p>hi</p>
  val b = <ul>{ xs.map(x => <li>{x}</li>) }</ul>
  val c = <br/>
  def f(n: scala.xml.Node) = n match { case <a>{t}</a> => t }
}
";
        assert_eq!(
            flagged(Rule::NoXml, source),
            ["2:11", "3:11", "4:11", "5:45"]
        );
    }

    #[test]
    fn passes_less_than_and_strings() {
        let source = "\
object A {
  val xmlString = \"<p>\"
  val lt = 1 < 2
  def f[T <: AnyRef](xs: List[T]) = xs
}
";
        assert_eq!(flagged(Rule::NoXml, source), Vec::<String>::new());
    }
}
