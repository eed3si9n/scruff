//! `S010` `organize_imports`, off by default; see
//! `doc/S010_organize_imports.md`.
//!
//! Flags imports out of sorted order, pointing at the first import out
//! of place; `--fix` sorts the lines and drops blank ones between them.
//! Imports up to another statement sort together. They compare chunk by
//! chunk between the dots, with capitals after lowercase letters, braces
//! left out, and wildcards first. An import using a name an import above
//! it brings in, as `a.foo` after `c.b.a`, keeps its place after it, and
//! the other way round. An import with `scruff: ignore(organize_imports)`
//! goes after the rest, since what it uses may come from any of them.

use std::cmp::Ordering;

use super::{Edit, Finding, Rule, suppression};
use crate::lossless_tree::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxTrivia, TextRange, TextSize,
    TriviaKind,
};

/// What the rule flags, for `scruff check --help`.
pub(super) const SUMMARY: &str = "imports out of order";

pub(super) const MESSAGE: &str = "imports are not sorted alphabetically";

/// The first import out of place, if `element` is the first import of a
/// group and the group isn't sorted, with the group sorted to put in.
pub(super) fn check(element: &SyntaxElement<'_>) -> Option<Finding> {
    let node = element.as_node()?;
    if node.kind() != SyntaxKind::ImportDeclaration || in_group(node, Direction::Back).is_some() {
        return None;
    }
    let group: Vec<SyntaxNode<'_>> =
        std::iter::successors(Some(*node), |n| in_group(n, Direction::Forward)).collect();
    let imports: Vec<Import> = group.iter().map(Import::new).collect();
    let order = sorted_order(&imports);
    // Not on an ignored import: that would hide the problem and its fix.
    let (at, _) = order
        .iter()
        .enumerate()
        .find(|&(at, &index)| at != index && !imports[at].ignored)?;
    Some(Finding {
        range: group[at].text_range(),
        fix: sort_lines(&group, &order),
    })
}

/// The group's lines put in `order`, if each import starts a line. Each
/// import takes along the rest of its line, as a comment there, and the
/// comment lines right above it.
fn sort_lines(group: &[SyntaxNode<'_>], order: &[usize]) -> Option<Edit> {
    let source = group[0].ancestors().last()?.text();
    let line_start = |offset: usize| source[..offset].rfind('\n').map_or(0, |i| i + 1);
    // Where the line ends, before its `\r\n` or `\n`.
    let line_end = |offset: usize| {
        let end = source[offset..]
            .find('\n')
            .map_or(source.len(), |i| offset + i);
        end - usize::from(source[..end].ends_with('\r'))
    };
    let is_comment = |text: &str| {
        let text = text.trim();
        text.starts_with("//") || (text.starts_with("/*") && text.ends_with("*/"))
    };
    // An ignore comment above the first import goes along with it.
    let first = ignore_comment_above(&group[0]).unwrap_or(group[0].text_range().start());
    let start = line_start(usize::from(first));
    let end = line_end(usize::from(group[group.len() - 1].text_range().end()));
    let newline = if source[start..].contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut items = Vec::with_capacity(group.len());
    // Where the text after the last import's line starts.
    let mut next = None;
    for import in group {
        let (from, to) = (
            usize::from(import.text_range().start()),
            usize::from(import.text_range().end()),
        );
        let (begin, finish) = (line_start(from), line_end(to));
        let rest = &source[to..finish];
        if !source[begin..from].trim().is_empty() || !(rest.trim().is_empty() || is_comment(rest)) {
            return None;
        }
        let begin = if next.is_none() { start } else { begin };
        // Comment lines since the last import go with this one; blank
        // lines are dropped.
        let mut lines = Vec::new();
        if let Some(after) = next {
            for line in source[after..begin].lines() {
                if is_comment(line) {
                    lines.push(line);
                } else if !line.trim().is_empty() {
                    return None;
                }
            }
        }
        lines.push(&source[begin..finish]);
        items.push(lines.join(newline));
        next = Some(
            source[finish..]
                .find('\n')
                .map_or(source.len(), |i| finish + i + 1),
        );
    }
    let replacement = order
        .iter()
        .map(|&i| items[i].as_str())
        .collect::<Vec<_>>()
        .join(newline);
    let offset = |o: usize| TextSize::try_from(o).ok();
    Some(Edit {
        range: TextRange::new(offset(start)?, offset(end)?),
        replacement,
    })
}

#[derive(Clone, Copy)]
enum Direction {
    Back,
    Forward,
}

/// The import next to `node` in `direction`, if it's in the same group:
/// only whitespace without a blank line, comments and `;` between them.
fn in_group<'t>(node: &SyntaxNode<'t>, direction: Direction) -> Option<SyntaxNode<'t>> {
    let step = |e: &SyntaxElement<'t>| match direction {
        Direction::Back => e.prev_sibling_or_token(),
        Direction::Forward => e.next_sibling_or_token(),
    };
    let mut element = step(&NodeOrToken::Node(*node));
    while let Some(e) = element {
        match &e {
            NodeOrToken::Node(n) if n.kind() == SyntaxKind::ImportDeclaration => return Some(*n),
            NodeOrToken::Token(t) if t.kind() == SyntaxKind::Semi => {}
            _ if e.is_trivia() => {}
            _ => return None,
        }
        element = step(&e);
    }
    None
}

/// What sorting needs to know about one import.
struct Import {
    /// What it's sorted by: the chunks of its text between dots, without
    /// whitespace, comments or backticks, each ranked character by
    /// character.
    key: Vec<Vec<Rank>>,
    /// The first name of each path, as `a` in `a.foo`.
    roots: Vec<String>,
    /// The names it brings in, as `a` from `c.b.a`.
    names: Vec<String>,
    /// Whether a `scruff: ignore(organize_imports)` covers it, which puts
    /// it after the rest of the group: what it uses, say from a wildcard,
    /// may come from any import above it.
    ignored: bool,
}

impl Import {
    fn new(node: &SyntaxNode<'_>) -> Import {
        let mut key = vec![Vec::new()];
        for token in node
            .tokens()
            .filter(|t| !t.is_trivia() && t.kind() != SyntaxKind::ImportKw)
        {
            match token.kind() {
                SyntaxKind::Dot => key.push(Vec::new()),
                // `a.{b, c}` sorts as `a.b,c`.
                SyntaxKind::LCurly | SyntaxKind::RCurly => {}
                _ if token
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::NamespaceWildcard) =>
                {
                    key.last_mut().expect("starts with a chunk").push(WILDCARD);
                }
                _ => key
                    .last_mut()
                    .expect("starts with a chunk")
                    .extend(name(token.text()).chars().map(rank)),
            }
        }
        let parts: Vec<SyntaxElement<'_>> = node
            .non_trivia_children()
            .filter(|child| child.kind() != SyntaxKind::ImportKw)
            .collect();
        let mut roots = Vec::new();
        let mut names = Vec::new();
        // `import a.b, c.{d, e}` holds one path per comma.
        for path in parts.split(|part| part.kind() == SyntaxKind::Comma) {
            if let Some(first) = path.first().filter(|p| p.kind() == SyntaxKind::Identifier) {
                roots.push(name(first.text()));
            }
            names.extend(path.last().and_then(imported).into_iter().flatten());
        }
        let ignores = |t: &SyntaxTrivia<'_>| {
            t.is_comment() && suppression::ignores(t.text(), Rule::OrganizeImports)
        };
        let ignored =
            node.trailing_trivia().any(|t| ignores(&t)) || ignore_comment_above(node).is_some();
        Import {
            key,
            roots,
            names,
            ignored,
        }
    }

    /// Whether `self` refers to a name `other` brings in, or the other way
    /// round, so the two must keep their order.
    fn related(&self, other: &Import) -> bool {
        let uses = |a: &Import, b: &Import| a.roots.iter().any(|r| b.names.contains(r));
        uses(self, other) || uses(other, self)
    }
}

/// Where the `scruff: ignore(organize_imports)` on the line right above
/// `import` starts, if there is one.
fn ignore_comment_above(import: &SyntaxNode<'_>) -> Option<TextSize> {
    let mut before = import
        .leading_trivia()
        .rev()
        .filter(|t| t.kind() != TriviaKind::Whitespace);
    // The newline ending the comment's line, then the comment.
    before.next().filter(|t| t.kind() == TriviaKind::Newline)?;
    before
        .next()
        .filter(|t| t.is_comment() && suppression::ignores(t.text(), Rule::OrganizeImports))
        .map(|t| t.text_range().start())
}

/// The names the last part of a path brings in, or `None` for a wildcard.
fn imported(last: &SyntaxElement<'_>) -> Option<Vec<String>> {
    match last.kind() {
        SyntaxKind::Identifier => Some(vec![name(last.text())]),
        SyntaxKind::AsRenamedIdentifier | SyntaxKind::ArrowRenamedIdentifier => {
            Some(alias(last.as_node()?).into_iter().collect())
        }
        SyntaxKind::NamespaceSelectors => {
            let selectors = last.as_node()?;
            let mut names = Vec::new();
            for selector in selectors.non_trivia_children() {
                match selector.kind() {
                    SyntaxKind::Identifier => names.push(name(selector.text())),
                    SyntaxKind::AsRenamedIdentifier | SyntaxKind::ArrowRenamedIdentifier => {
                        names.extend(selector.as_node().and_then(alias));
                    }
                    SyntaxKind::NamespaceWildcard => return None,
                    _ => {}
                }
            }
            Some(names)
        }
        _ => None,
    }
}

/// The new name of `b as c` or `b => c`; none for `b => _`.
fn alias(renamed: &SyntaxNode<'_>) -> Option<String> {
    renamed
        .child_by_field_name("alias")
        .filter(|a| a.kind() == SyntaxKind::Identifier)
        .map(|a| name(a.text()))
}

/// `` `a` `` and `a` are the same name.
fn name(text: &str) -> String {
    text.trim_matches('`').to_string()
}

type Rank = (char, Option<char>);

/// Where `*`, `_` and `given` sort as wildcards: before any character.
const WILDCARD: Rank = ('\0', None);

/// Where `c` sorts: as in ASCII, but a capital right after `z`, before
/// `{`.
fn rank(c: char) -> Rank {
    if c.is_ascii_uppercase() {
        ('z', Some(c))
    } else {
        (c, None)
    }
}

/// The indexes of `imports` in sorted order: each time, the smallest
/// import that no import before it must follow.
fn sorted_order(imports: &[Import]) -> Vec<usize> {
    let mut placed = vec![false; imports.len()];
    let mut order = Vec::with_capacity(imports.len());
    while order.len() < imports.len() {
        let next = (0..imports.len())
            .filter(|&j| {
                !placed[j] && (0..j).all(|i| placed[i] || !imports[i].related(&imports[j]))
            })
            .min_by(|&a, &b| {
                let (x, y) = (&imports[a], &imports[b]);
                // Ignored imports keep their order, after the rest.
                x.ignored
                    .cmp(&y.ignored)
                    .then_with(|| {
                        if x.ignored {
                            Ordering::Equal
                        } else {
                            x.key.cmp(&y.key)
                        }
                    })
                    .then(a.cmp(&b))
            })
            .expect("imports keep their source order when related, so one is always free");
        placed[next] = true;
        order.push(next);
    }
    order
}

#[cfg(test)]
mod tests {
    use crate::lint::{Rule, fixed, flagged};

    #[test]
    fn flags_the_first_import_out_of_place() {
        let source = "\
import scala.util.Try
import java.io.File
import cats.syntax.all.*
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        let source = "\
import a.b
import a.c
import b.x
import a.d
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["3:1"]);
    }

    #[test]
    fn passes_sorted_imports() {
        let source = "\
import a.b
import a.bc
import a.{c, d}
import a.z
import a.Z
import b.{given, *}
import c.b.a; import c.d
import A.x
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
    }

    #[test]
    fn sorts_chunk_by_chunk() {
        // As one string, `$` would sort before `.`.
        let source = "\
import a.b$c
import a.b.d
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        let source = "\
import a.b
import a.b.d
import a.b.d.e
import a.b$c
import a.`b.c`
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
    }

    #[test]
    fn braces_are_left_out() {
        let source = "\
import scala.{specialized => sp}
import scala.annotation.tailrec
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        let source = "\
import scala.annotation.tailrec
import scala.{specialized => sp}
import scala.util.Try
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
    }

    #[test]
    fn wildcards_come_first() {
        let source = "\
import b.a
import b.{given, *}
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        let source = "\
import a.z
import b.*
import b.given
import b.{given, *}
import b.{given, Ord}
import b.a
import b.given_
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
    }

    #[test]
    fn ignored_imports_go_after_the_rest() {
        // `duration` comes from `scala.concurrent.*`, so it must stay below.
        let source = "\
import scala.concurrent.*
// scruff: ignore(organize_imports)
import duration.*
import cats.Eq
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "\
import cats.Eq
import scala.concurrent.*
// scruff: ignore(organize_imports)
import duration.*
"
        );
        // The comment above the first import goes along with it, and the
        // problem is reported on an import that isn't ignored.
        let source = "\
// scruff: ignore(organize_imports)
import b.x
import a.y
import c.z
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["3:1"]);
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "\
import a.y
import c.z
// scruff: ignore(organize_imports)
import b.x
"
        );
        // On the import's own line, by code; ignored imports keep their
        // order.
        let source = "\
import z.w // scruff: ignore(S010)
import b.x
import y.v // scruff: ignore(organize_imports)
import a.q
";
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "\
import a.q
import b.x
import z.w // scruff: ignore(S010)
import y.v // scruff: ignore(organize_imports)
"
        );
    }

    #[test]
    fn passes_ignored_imports_after_the_rest() {
        let source = "\
import a.x
import c.y
// scruff: ignore(organize_imports)
import duration.*
// scruff: ignore(organize_imports)
import b.z
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
        // A comment above a blank line, or for another rule, doesn't count.
        let source = "\
// scruff: ignore(organize_imports)

import b.x
import a.y // scruff: ignore(no_null)
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["3:1"]);
    }

    #[test]
    fn file_wide_comments_opt_out_of_every_group() {
        let unsorted = "\
import b.x
import a.y

object A {
  import d.x
  import c.y
}
";
        assert_eq!(flagged(Rule::OrganizeImports, unsorted), ["1:1", "5:3"]);
        for comment in [
            "// scruff: file_ignore(organize_imports)",
            "// scruff: file_ignore(S010)",
            "// scruff: noqa",
        ] {
            let source = format!("{comment}\n{unsorted}");
            assert_eq!(
                flagged(Rule::OrganizeImports, &source),
                Vec::<String>::new(),
                "{comment}"
            );
        }
        // Another rule's name leaves this one on.
        let source = format!("// scruff: file_ignore(no_null)\n{unsorted}");
        assert_eq!(flagged(Rule::OrganizeImports, &source), ["2:1", "6:3"]);
    }

    #[test]
    fn fixes_by_sorting_the_lines_and_dropping_blank_ones() {
        let source = "\
package p

import scala.util.Try
// the file API
import java.io.File // for paths
import cats.syntax.all.*

import b.x
import a.{
  y,
  z
}

object A {
  import d.x
  import c.y
  val v = 1
}
";
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "\
package p

import a.{
  y,
  z
}
import b.x
import cats.syntax.all.*
// the file API
import java.io.File // for paths
import scala.util.Try

object A {
  import c.y
  import d.x
  val v = 1
}
"
        );
    }

    #[test]
    fn fixes_keep_imports_of_an_imported_name_after_it() {
        let source = "import c.b.a\nimport b.x\nimport a.foo\nimport a.bar\n";
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "import b.x\nimport c.b.a\nimport a.bar\nimport a.foo\n"
        );
    }

    #[test]
    fn fixes_keep_crlf_line_endings() {
        let source = "import b.x\r\nimport a.y // note\r\nobject A\r\n";
        assert_eq!(
            fixed(Rule::OrganizeImports, source),
            "import a.y // note\r\nimport b.x\r\nobject A\r\n"
        );
    }

    #[test]
    fn leaves_imports_sharing_a_line() {
        for source in [
            "import b.x; import a.y\n",
            "object A { import b.x\n  import a.y\n}\n",
            "import b.x\n/* a\n   b */\nimport a.y\n",
        ] {
            assert_eq!(fixed(Rule::OrganizeImports, source), source);
            assert_eq!(flagged(Rule::OrganizeImports, source).len(), 1, "{source}");
        }
    }

    #[test]
    fn capitals_come_after_lowercase_letters() {
        let source = "\
import cats.Eq
import cats.data.Ior
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
        let source = "\
import MyApp.given
import cats.Eq
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
    }

    #[test]
    fn imports_of_an_imported_name_come_after_it() {
        let source = "\
import c.b.a
import a.foo
import d.e
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
        let renamed = "\
import c.{b => a, d as e}
import e.x
import a.foo
";
        assert_eq!(flagged(Rule::OrganizeImports, renamed), ["2:1"]);
        let many = "\
import c.b.a, d.f
import f.x
import a.foo
";
        assert_eq!(flagged(Rule::OrganizeImports, many), ["2:1"]);
        // `b.x` doesn't need `a`, so it still sorts before `c.b.a`.
        let unrelated = "\
import c.b.a
import a.foo
import b.x
";
        assert_eq!(flagged(Rule::OrganizeImports, unrelated), ["1:1"]);
    }

    #[test]
    fn an_import_above_one_that_shadows_it_stays_above() {
        let source = "\
import a.foo
import Z.a
import a.bar
";
        assert_eq!(flagged(Rule::OrganizeImports, source), Vec::<String>::new());
    }

    #[test]
    fn names_from_a_wildcard_sort_as_any_other() {
        let source = "\
import scala.concurrent.*
import duration.*
";
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1"]);
    }

    #[test]
    fn only_statements_start_new_groups() {
        let source = "\
import java.io.File
import java.util.List

import scala.util.Try
// a comment
import scala.collection.mutable

import cats.Eq

object A {
  import z.y
  val x = 1
  import b.a
  def f = {
    import x.z
    import x.y
    1
  }
}
";
        // The blank lines don't split the imports at the top.
        assert_eq!(flagged(Rule::OrganizeImports, source), ["1:1", "15:5"]);
        let sorted = "import a.b\n\nimport c.d\n\nobject A\n";
        assert_eq!(flagged(Rule::OrganizeImports, sorted), Vec::<String>::new());
    }
}
