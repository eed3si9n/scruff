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

use std::collections::HashMap;

use super::Rule;
use crate::lossless_tree::{SyntaxTrivia, Tree};

/// The rules turned off on each line, by 1-based line number.
pub(super) struct Suppressions(HashMap<usize, Vec<Rule>>);

impl Suppressions {
    pub(super) fn find(tree: &Tree) -> Suppressions {
        let mut lines: HashMap<usize, Vec<Rule>> = HashMap::new();
        let source = tree.syntax().text();
        let mut add = |comment: SyntaxTrivia<'_>| {
            let Some(rules) = parse(comment.text()) else {
                return;
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
        Suppressions(lines)
    }

    /// Whether `rule` is turned off on `line`.
    pub(super) fn covers(&self, line: usize, rule: Rule) -> bool {
        self.0.get(&line).is_some_and(|rules| rules.contains(&rule))
    }
}

/// The rules a `scruff: ignore(...)` comment names, or `None` if it isn't
/// one. Entries no rule matches are skipped: the problem they meant to
/// hide is still reported, which shows the mistake.
fn parse(comment: &str) -> Option<Vec<Rule>> {
    let body = match comment.strip_prefix("//") {
        Some(rest) => rest,
        None => comment.strip_prefix("/*")?.strip_suffix("*/")?,
    };
    let entries = body
        .trim()
        .strip_prefix("scruff:")?
        .trim_start()
        .strip_prefix("ignore(")?;
    let (entries, rest) = entries.split_once(')')?;
    if !rest.trim().is_empty() {
        return None;
    }
    Some(
        entries
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .flat_map(Rule::matching)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lint::{check_tree, flagged};
    use crate::lossless_tree::parse as parse_tree;

    #[test]
    fn parses_names_codes_and_prefixes() {
        assert_eq!(parse("// scruff: ignore(S004)"), Some(vec![Rule::NoNull]));
        assert_eq!(
            parse("//scruff:ignore( no_null , S002 )"),
            Some(vec![Rule::NoNull, Rule::NoVar])
        );
        assert_eq!(
            parse("/* scruff: ignore(no_while) */"),
            Some(vec![Rule::NoWhile])
        );
        assert_eq!(
            parse("// scruff: ignore(ALL)").map(|r| r.len()),
            Some(Rule::ALL.len())
        );
        assert_eq!(
            parse("// scruff: ignore(C)"),
            Some(vec![Rule::UntypedImplicitVal])
        );
        assert_eq!(parse("// scruff: ignore(S999)"), Some(vec![]));
        assert_eq!(parse("// scruff: ignore S004"), None);
        assert_eq!(parse("// scruff: ignore(S004) because"), None);
        assert_eq!(parse("// ignore(S004)"), None);
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
}
