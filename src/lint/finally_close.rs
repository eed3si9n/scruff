//! `S011` `finally_close`, off by default; see `doc/S011_finally_close.md`.
//!
//! Flags `try { ... } finally a.close()`, pointing at `try`. `--fix`
//! rewrites `val a = ...` and the `try` after it to
//! `Using.resource(...): a =>`, or `Using.resource(...) { a => ... }`
//! without fewer braces, and imports `scala.util.Using`.
//!
//! Credit to Kenji Yoshida (xuwei-k)'s [ScalaUtilUsingResource rule](https://github.com/xuwei-k/scalafix-rules/blob/main/rules/src/main/scala/fix/ScalaUtilUsingResource.scala).

use super::{Edit, Finding, Fix, imports, line_start, name, newline};
use crate::config::Dialect;
use crate::lossless_tree::{SyntaxElement, SyntaxKind, SyntaxNode, TextRange, TextSize};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "`try { ... } finally x.close()`";

pub(super) const MESSAGE: &str =
    "closing a resource in `finally`; use `scala.util.Using.resource` instead";

/// `try`, if `element` is a `try` without `catch` whose `finally` only
/// closes a name, with the rewrite to `Using.resource` in `dialect`.
pub(super) fn check(element: &SyntaxElement<'_>, dialect: Dialect) -> Option<Finding> {
    let node = element.as_node()?;
    if node.kind() != SyntaxKind::TryExpression {
        return None;
    }
    let children: Vec<SyntaxElement<'_>> = node.non_trivia_children().collect();
    if children.iter().any(|c| c.kind() == SyntaxKind::CatchClause) {
        return None;
    }
    let finally = children
        .iter()
        .find(|c| c.kind() == SyntaxKind::FinallyClause)?
        .as_node()?;
    let resource = closed(finally)?;
    let try_kw = children.first().filter(|c| c.kind() == SyntaxKind::TryKw)?;
    Some(Finding {
        range: try_kw.text_range(),
        fix: using(node, &resource, dialect),
    })
}

/// The name `finally` closes, if closing it, as `a.close()` or `a.close`,
/// is all it does.
fn closed(finally: &SyntaxNode<'_>) -> Option<String> {
    let mut expr = only(finally.non_trivia_children().skip(1))?;
    while matches!(expr.kind(), SyntaxKind::Block | SyntaxKind::IndentedBlock) {
        expr = only(expr.as_node()?.non_trivia_children().filter(|c| {
            !matches!(
                c.kind(),
                SyntaxKind::LCurly | SyntaxKind::RCurly | SyntaxKind::Semi
            )
        }))?;
    }
    let field = match expr.kind() {
        SyntaxKind::CallExpression => {
            let call = expr.as_node()?;
            let arguments = call.child_by_field_name("arguments")?;
            // `()`: the parentheses and nothing else.
            if arguments.as_node()?.non_trivia_children().count() != 2 {
                return None;
            }
            call.child_by_field_name("function")?
        }
        SyntaxKind::FieldExpression => expr,
        _ => return None,
    };
    let field = field
        .as_node()
        .filter(|f| f.kind() == SyntaxKind::FieldExpression)?;
    let value = field
        .child_by_field_name("value")
        .filter(|v| v.kind() == SyntaxKind::Identifier)?;
    let method = field.child_by_field_name("field")?;
    (method.text() == "close").then(|| name(value.text()))
}

/// The one element of `elements`, if there's exactly one.
fn only<'t>(mut elements: impl Iterator<Item = SyntaxElement<'t>>) -> Option<SyntaxElement<'t>> {
    let first = elements.next()?;
    elements.next().is_none().then_some(first)
}

/// The edits from `val a = e` and the `try` after it to
/// `Using.resource(e) { a => ... }`, or `Using.resource(e): a =>` where
/// `dialect` has fewer braces, with the import.
fn using(try_expr: &SyntaxNode<'_>, resource: &str, dialect: Dialect) -> Option<Fix> {
    let source = try_expr.ancestors().last()?.text();
    let parent = try_expr.parent()?;
    if !matches!(parent.kind(), SyntaxKind::Block | SyntaxKind::IndentedBlock) {
        return None;
    }
    // The `val` right before, with only whitespace between.
    let mut before = try_expr.prev_sibling_or_token();
    while let Some(e) = before.filter(|e| e.kind() == SyntaxKind::Whitespace) {
        before = e.prev_sibling_or_token();
    }
    let val = *before?
        .as_node()
        .filter(|v| v.kind() == SyntaxKind::ValDefinition)?;
    let first = val.non_trivia_children().next()?;
    let pattern = val.child_by_field_name("pattern")?;
    if first.kind() != SyntaxKind::ValKw
        || pattern.kind() != SyntaxKind::Identifier
        || name(pattern.text()) != resource
        || has_comments(&val)
    {
        return None;
    }
    let value = val.child_by_field_name("value")?;
    // After the `try`, the name would be gone.
    let used_after = std::iter::successors(try_expr.next_sibling_or_token(), |e| {
        e.next_sibling_or_token()
    })
    .filter_map(|e| e.into_node())
    .flat_map(|n| n.tokens().collect::<Vec<_>>())
    .any(|t| t.kind() == SyntaxKind::Identifier && name(t.text()) == resource);
    if used_after {
        return None;
    }
    let param = match val.child_by_field_name("type") {
        Some(ty) => format!("({}: {})", pattern.text(), ty.text()),
        None => pattern.text().to_string(),
    };
    let body = try_expr.child_by_field_name("body")?;
    // Comments outside the body would be lost.
    let outside_body = try_expr
        .children_with_tokens()
        .filter(|c| c.text_range() != body.text_range());
    if outside_body.into_iter().any(|c| match &c {
        _ if c.is_trivia() => c.kind() != SyntaxKind::Whitespace,
        SyntaxElement::Node(n) => has_comments(n),
        SyntaxElement::Token(_) => false,
    }) {
        return None;
    }
    let resource_expr = trimmed(source, &value)?;
    // Comments after the body's last token would be lost.
    let body_end = content_end(&body)?;
    if body
        .as_node()
        .into_iter()
        .flat_map(|b| b.descendants_with_tokens())
        .any(|e| {
            e.is_trivia()
                && e.kind() != SyntaxKind::Whitespace
                && e.text_range().start() >= body_end
        })
    {
        return None;
    }
    let start = usize::from(val.text_range().start());
    let indent = &source[line_start(source, start)..start];
    let indent = indent.trim().is_empty().then_some(indent);
    let try_kw = try_expr.non_trivia_children().next()?;
    // The body's text after `try` or `{`, up to its last token.
    let statements = match body.kind() {
        SyntaxKind::Block => {
            let lcurly = body.as_node()?.non_trivia_children().next()?;
            let rcurly = body.as_node()?.non_trivia_children().last()?;
            let inner = TextRange::new(lcurly.text_range().end(), rcurly.text_range().start());
            let last = body
                .as_node()?
                .tokens()
                .filter(|t| !t.is_trivia() && inner.contains_range(t.text_range()))
                .last();
            last.map(|t| &source[usize::from(inner.start())..usize::from(t.text_range().end())])
        }
        SyntaxKind::IndentedBlock => {
            Some(&source[usize::from(try_kw.text_range().end())..usize::from(body_end)])
        }
        _ => None,
    };
    let newline = newline(source);
    let replacement = match (statements, indent) {
        (Some(statements), Some(indent))
            if dialect.fewer_braces && indented_below(statements, indent) =>
        {
            format!("Using.resource({resource_expr}): {param} =>{statements}")
        }
        _ => match body.kind() {
            SyntaxKind::Block => {
                let text = body.text();
                let inner = text.strip_prefix('{')?.strip_suffix('}')?;
                format!("Using.resource({resource_expr}) {{ {param} =>{inner}}}")
            }
            SyntaxKind::IndentedBlock => format!(
                "Using.resource({resource_expr}) {{ {param} =>{}{newline}{}}}",
                statements?, indent?
            ),
            _ => format!(
                "Using.resource({resource_expr})({param} => {})",
                trimmed(source, &body)?
            ),
        },
    };
    // An indented block's range takes in the whitespace after it.
    let end = content_end(&SyntaxElement::Node(*try_expr))?;
    let rewrite = Edit {
        range: TextRange::new(val.text_range().start(), end),
        replacement,
    };
    let mut edits = vec![rewrite];
    edits.extend(imports::import(try_expr, &["scala", "util", "Using"])?);
    Some(Fix { edits })
}

/// Whether `statements` start on a line of their own, after the rest of
/// the first line, and every line of them is indented deeper than `indent`,
/// as the body of a colon argument must be.
fn indented_below(statements: &str, indent: &str) -> bool {
    let mut lines = statements.lines();
    let first = lines.next().unwrap_or("").trim();
    let rest: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    (first.is_empty() || first.starts_with("//"))
        && !rest.is_empty()
        && rest.iter().all(|line| {
            let depth = line.len() - line.trim_start().len();
            depth > indent.len() && line.starts_with(indent)
        })
}

/// Where `element`'s last token other than whitespace and comments ends:
/// a node's range can take in the whitespace after it.
fn content_end(element: &SyntaxElement<'_>) -> Option<TextSize> {
    match element {
        SyntaxElement::Node(n) => n.tokens().filter(|t| !t.is_trivia()).last(),
        SyntaxElement::Token(t) => Some(*t),
    }
    .map(|t| t.text_range().end())
}

/// `element`'s text without the whitespace and comments after it.
fn trimmed<'t>(source: &'t str, element: &SyntaxElement<'t>) -> Option<&'t str> {
    let start = usize::from(element.text_range().start());
    source.get(start..usize::from(content_end(element)?))
}

/// Whether there's a comment in `node`.
fn has_comments(node: &SyntaxNode<'_>) -> bool {
    node.descendants_with_tokens()
        .any(|e| e.is_trivia() && e.kind() != SyntaxKind::Whitespace)
}

#[cfg(test)]
mod tests {
    use crate::config::Dialect;
    use crate::lint::{Rule, fixed, fixed_in, flagged};

    fn scala213() -> Dialect {
        Dialect::named("scala213").unwrap()
    }

    #[test]
    fn rewrites_to_using_and_adds_the_import() {
        let source = "\
import java.io.FileInputStream

object A:
  def f1: Int =
    val a = new FileInputStream(\"a\")
    try
      val b = a.read()
      val c = a.read()
      b + c
    finally a.close()
";
        assert_eq!(flagged(Rule::FinallyClose, source), ["6:5"]);
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
import java.io.FileInputStream
import scala.util.Using

object A:
  def f1: Int =
    Using.resource(new FileInputStream(\"a\")): a =>
      val b = a.read()
      val c = a.read()
      b + c
"
        );
    }

    #[test]
    fn flags_only_a_finally_that_only_closes() {
        let source = "\
object A {
  def f(in: java.io.InputStream) = try in.read() finally in.close()
  def g = try a.read() finally { a.close; }
  def h = try a.read() catch { case _: E => 0 } finally a.close()
  def i = try a.read() finally { a.close(); log() }
  def j = try a.read() finally a.close(1)
  def k = try a.read() finally a.b.close()
  def l = try a.read() finally a.flush()
}
";
        assert_eq!(flagged(Rule::FinallyClose, source), ["2:36", "3:11"]);
    }

    #[test]
    fn rewrites_expressions_and_indented_blocks() {
        let source = "\
import scala.util.Using

object A:
  def f =
    val a = new F
    try a.read() finally a.close()
  def g =
    val `b`: Res = open()
    try
      b.read()
      b.read()
    finally
      b.close()
";
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
import scala.util.Using

object A:
  def f =
    Using.resource(new F)(a => a.read())
  def g =
    Using.resource(open()): (`b`: Res) =>
      b.read()
      b.read()
"
        );
    }

    #[test]
    fn adds_the_import_once_in_sorted_order() {
        let source = "\
package p

import java.io.File
// for Z
import z.Z

object A {
  def f = {
    val a = new F
    try a.read() finally a.close()
  }
  def g = {
    val a = new F
    try a.read() finally a.close()
  }
}
";
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
package p

import java.io.File
import scala.util.Using
// for Z
import z.Z

object A {
  def f = {
    Using.resource(new F)(a => a.read())
  }
  def g = {
    Using.resource(new F)(a => a.read())
  }
}
"
        );
    }

    #[test]
    fn adds_the_import_after_the_package_or_at_the_start() {
        let body = "\
object A {
  def f = {
    val a = new F
    try a.read() finally a.close()
  }
}
";
        let fixed_body = "\
object A {
  def f = {
    Using.resource(new F)(a => a.read())
  }
}
";
        assert_eq!(
            fixed(
                Rule::FinallyClose,
                &format!("package p\npackage q\n\n{body}")
            ),
            format!("package p\npackage q\n\nimport scala.util.Using\n\n{fixed_body}")
        );
        assert_eq!(
            fixed(Rule::FinallyClose, &format!("// header\n{body}")),
            format!("// header\nimport scala.util.Using\n\n{fixed_body}")
        );
    }

    #[test]
    fn imports_in_scope_bring_using_in() {
        for import in [
            "import scala.util.Using",
            "import scala.util.{Try, Using}",
            "import scala.util.*",
            "import scala.util._",
        ] {
            let source = format!(
                "object A {{\n  {import}\n  def f = {{\n    val a = new F\n    try a.read() finally a.close()\n  }}\n}}\n"
            );
            assert_eq!(
                fixed(Rule::FinallyClose, &source),
                format!(
                    "object A {{\n  {import}\n  def f = {{\n    Using.resource(new F)(a => a.read())\n  }}\n}}\n"
                ),
                "{import}"
            );
        }
    }

    #[test]
    fn leaves_what_it_cannot_rewrite() {
        for source in [
            // Another `Using`.
            "import my.Using\nobject A { def f = { val a = new F\n try a.read() finally a.close() } }\n",
            "import x.{Y => Using}\nobject A { def f = { val a = new F\n try a.read() finally a.close() } }\n",
            // `a` is used after.
            "object A { def f = { val a = new F\n try a.read() finally a.close()\n a.ok } }\n",
            // No `val` right before.
            "object A { def f = { val a = new F\n g()\n try a.read() finally a.close() } }\n",
            "object A { def f = { var a = new F\n try a.read() finally a.close() } }\n",
            "object A { def f = { lazy val a = new F\n try a.read() finally a.close() } }\n",
            "object A { def f = { val b = new F\n try a.read() finally a.close() } }\n",
            "object A { val a = new F\n try a.read() finally a.close() }\n",
            // Comments would be lost.
            "object A { def f = { val a = new F\n // c\n try a.read() finally a.close() } }\n",
            "object A { def f = { val a = new F\n try a.read() /* c */ finally a.close() } }\n",
        ] {
            assert_eq!(fixed(Rule::FinallyClose, source), source);
            assert_eq!(flagged(Rule::FinallyClose, source).len(), 1, "{source}");
        }
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let source = "object A:\r\n  def f =\r\n    val a = new F\r\n    try\r\n      a.read()\r\n    finally a.close()\r\n";
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "import scala.util.Using\r\n\r\nobject A:\r\n  def f =\r\n    Using.resource(new F): a =>\r\n      a.read()\r\n"
        );
        assert_eq!(
            fixed_in(Rule::FinallyClose, source, scala213()),
            "import scala.util.Using\r\n\r\nobject A:\r\n  def f =\r\n    Using.resource(new F) { a =>\r\n      a.read()\r\n    }\r\n"
        );
    }

    #[test]
    fn rewrites_nested_trys_and_multi_line_values() {
        let source = "\
import scala.util.Using

object A:
  def f =
    val a =
      F(
        1
      )

    try
      val b = a.open()
      try
        b.read()
      finally b.close()
    finally a.close()
";
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
import scala.util.Using

object A:
  def f =
    Using.resource(F(
        1
      )): a =>
      Using.resource(a.open()): b =>
        b.read()
"
        );
    }

    #[test]
    fn uses_braces_without_fewer_braces() {
        let source = "\
object A {
  def f1: Int = {
    val a = new F
    try {
      val b = a.read() // b
      b
    } finally {
      a.close()
    }
  }
}
";
        assert_eq!(
            fixed_in(Rule::FinallyClose, source, scala213()),
            "\
import scala.util.Using

object A {
  def f1: Int = {
    Using.resource(new F) { a =>
      val b = a.read() // b
      b
    }
  }
}
"
        );
        // Scala 3 drops the braces of the `try`'s block too.
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
import scala.util.Using

object A {
  def f1: Int = {
    Using.resource(new F): a =>
      val b = a.read() // b
      b
  }
}
"
        );
    }

    #[test]
    fn uses_braces_where_the_body_is_not_indented_below() {
        let source = "\
object A {
  def f = {
    val a = new F
    try { a.read()
    } finally a.close()
  }
  def g = {
    val a = new F
    try {
      foo(
    )
    } finally a.close()
  }
}
";
        assert_eq!(
            fixed(Rule::FinallyClose, source),
            "\
import scala.util.Using

object A {
  def f = {
    Using.resource(new F) { a => a.read()
    }
  }
  def g = {
    Using.resource(new F) { a =>
      foo(
    )
    }
  }
}
"
        );
    }
}
