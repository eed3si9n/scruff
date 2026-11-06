//! Comments that turn rules off for a line:
//!
//! ```scala
//! val s: String = null // scruff: ignore(S004)
//!
//! // scruff: ignore(no_null, no_var)
//! var t: String = null
//! ```
//!
//! A comment after code covers its own line; a comment on a line of its
//! own covers the next line. Each entry is matched as in `[lint]`: a
//! rule's name, its code, a start of codes, or `ALL`. A block comment,
//! `/* scruff: ignore(S004) */`, works the same way.
//!
//! Two forms cover the whole file, wherever they are in it:
//! `// scruff: file_ignore(no_null)` turns the rules it names off, and
//! `// scruff: noqa` turns every rule off, as in Ruff.

use std::collections::HashMap;

use super::Rule;
use crate::lossless_tree::{SyntaxTrivia, Tree};

/// The rules turned off in a file: on each line, by 1-based line number,
/// and in the whole file.
pub(super) struct Suppressions {
    lines: HashMap<usize, Vec<Rule>>,
    file: Vec<Rule>,
}

impl Suppressions {
    pub(super) fn find(tree: &Tree) -> Suppressions {
        let mut lines: HashMap<usize, Vec<Rule>> = HashMap::new();
        let mut file = Vec::new();
        let source = tree.syntax().text();
        let mut add = |comment: SyntaxTrivia<'_>| {
            let rules = match parse(comment.text()) {
                Some(Directive::File(rules)) => return file.extend(rules),
                Some(Directive::Line(rules)) => rules,
                None => return,
            };
            let start = usize::from(comment.text_range().start());
            let (line, _) = tree.line_col(comment.text_range().start());
            let line_start = source[..start].rfind('\n').map_or(0, |i| i + 1);
            let alone = source[line_start..start].trim().is_empty();
            let target = if alone { line + 1 } else { line };
            lines.entry(target).or_default().extend(rules);
        };
        for token in tree.syntax().tokens() {
            token
                .leading_trivia()
                .chain(token.trailing_trivia())
                .filter(|t| t.is_comment())
                .for_each(&mut add);
        }
        tree.eof_trivia()
            .filter(|t| t.is_comment())
            .for_each(&mut add);
        Suppressions { lines, file }
    }

    /// Whether `rule` is turned off on `line`.
    pub(super) fn covers(&self, line: usize, rule: Rule) -> bool {
        self.file.contains(&rule)
            || self
                .lines
                .get(&line)
                .is_some_and(|rules| rules.contains(&rule))
    }
}

/// What a suppression comment turns off.
#[derive(Debug, PartialEq)]
enum Directive {
    /// `scruff: ignore(...)`: rules on one line.
    Line(Vec<Rule>),
    /// `scruff: file_ignore(...)` or `scruff: noqa`: rules in the whole file.
    File(Vec<Rule>),
}

/// What a suppression comment turns off, or `None` if it isn't one.
/// Entries no rule matches are skipped: the problem they meant to hide is
/// still reported, which shows the mistake.
fn parse(comment: &str) -> Option<Directive> {
    let body = match comment.strip_prefix("//") {
        Some(rest) => rest,
        None => comment.strip_prefix("/*")?.strip_suffix("*/")?,
    };
    let body = body.trim().strip_prefix("scruff:")?.trim_start();
    if body == "noqa" {
        return Some(Directive::File(Rule::ALL.to_vec()));
    }
    let (directive, entries): (fn(Vec<Rule>) -> Directive, _) =
        if let Some(entries) = body.strip_prefix("ignore(") {
            (Directive::Line, entries)
        } else {
            (Directive::File, body.strip_prefix("file_ignore(")?)
        };
    let (entries, rest) = entries.split_once(')')?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(directive(
        entries
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .flat_map(Rule::matching)
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::{check_tree, fixed, flagged};
    use crate::lossless_tree::parse as parse_tree;

    fn line(rules: &[Rule]) -> Option<Directive> {
        Some(Directive::Line(rules.to_vec()))
    }

    fn file(rules: &[Rule]) -> Option<Directive> {
        Some(Directive::File(rules.to_vec()))
    }

    #[test]
    fn parses_names_codes_and_prefixes() {
        assert_eq!(parse("// scruff: ignore(S004)"), line(&[Rule::NoNull]));
        assert_eq!(
            parse("//scruff:ignore( no_null , S002 )"),
            line(&[Rule::NoNull, Rule::NoVar])
        );
        assert_eq!(
            parse("/* scruff: ignore(no_while) */"),
            line(&[Rule::NoWhile])
        );
        assert_eq!(parse("// scruff: ignore(ALL)"), line(Rule::ALL));
        assert_eq!(
            parse("// scruff: ignore(C)"),
            line(&[Rule::UntypedImplicitVal])
        );
        assert_eq!(parse("// scruff: ignore(S999)"), line(&[]));
        assert_eq!(parse("// scruff: ignore S004"), None);
        assert_eq!(parse("// scruff: ignore(S004) because"), None);
        assert_eq!(parse("// ignore(S004)"), None);
    }

    #[test]
    fn parses_file_wide_comments() {
        assert_eq!(parse("// scruff: noqa"), file(Rule::ALL));
        assert_eq!(parse("/*scruff:noqa*/"), file(Rule::ALL));
        assert_eq!(
            parse("// scruff: file_ignore(no_null, S002)"),
            file(&[Rule::NoNull, Rule::NoVar])
        );
        assert_eq!(parse("// scruff: file_ignore(S999)"), file(&[]));
        assert_eq!(parse("// scruff: noqa(S004)"), None);
        assert_eq!(parse("// scruff: noqa because"), None);
        assert_eq!(parse("// scruff: file_ignore S004"), None);
        assert_eq!(parse("// noqa"), None);
    }

    #[test]
    fn a_trailing_comment_covers_its_line() {
        let source = "\
object A:
  val a: String = null // scruff: ignore(S004)
  val b: String = null
  val c: String = null /* scruff: ignore(no_null) */
";
        assert_eq!(flagged(Rule::NoNull, source), ["3:19"]);
    }

    #[test]
    fn a_comment_on_its_own_line_covers_the_next() {
        let source = "\
object A:
  // scruff: ignore(no_null)
  val a: String = null
  val b: String = null

  // scruff: ignore(no_null)

  val c: String = null
";
        assert_eq!(flagged(Rule::NoNull, source), ["4:19", "8:19"]);
    }

    #[test]
    fn only_the_rules_named_are_turned_off() {
        let source = "object A:\n  var a: String = null // scruff: ignore(S004)\n";
        let tree = parse_tree(source);
        let rules: Vec<_> = check_tree(&tree, Rule::ALL)
            .unwrap()
            .iter()
            .map(|d| d.rule)
            .collect();
        assert_eq!(rules, [Rule::NoVar]);
    }

    #[test]
    fn several_rules_and_all() {
        let source = "\
object A:
  var a: String = null // scruff: ignore(no_var, no_null)
  var b: String = null // scruff: ignore(ALL)
";
        let tree = parse_tree(source);
        assert_eq!(check_tree(&tree, Rule::ALL).unwrap(), []);
    }

    #[test]
    fn file_ignore_covers_the_whole_file() {
        let source = "\
object A:
  val a: String = null
  var b: String = null

// scruff: file_ignore(no_null)
object B:
  val c: String = null
";
        let tree = parse_tree(source);
        let rules: Vec<_> = check_tree(&tree, Rule::ALL)
            .unwrap()
            .iter()
            .map(|d| d.rule)
            .collect();
        assert_eq!(rules, [Rule::NoVar]);
    }

    #[test]
    fn noqa_covers_every_rule() {
        let source = "\
// scruff: noqa
object A:
  var a: String = null
  def f(x: Int = 1) { return }
";
        let tree = parse_tree(source);
        assert_eq!(check_tree(&tree, Rule::ALL).unwrap(), []);
    }

    #[test]
    fn file_wide_comments_are_not_fixed() {
        let source = "// scruff: noqa\nobject A:\n  def f() { }\n";
        assert_eq!(fixed(Rule::ProcedureSyntax, source), source);
    }
}
