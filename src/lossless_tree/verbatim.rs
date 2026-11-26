//! Parts of the source a formatter must copy exactly as written.
//!
//! A formatter should emit each verbatim range unchanged, and must not
//! re-indent a line that starts inside one: shifting the second line of a
//! multi-line string changes the string.

use std::sync::OnceLock;

use super::{
    NodeOrToken, RegionProblem, SyntaxKind, SyntaxNode, SyntaxToken, SyntaxTrivia, TextRange,
    TextSize, Tree, TriviaKind,
};

/// Why a range must be copied unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerbatimReason {
    /// An `ERROR` node, or a node tree-sitter completed with a missing token.
    SyntaxError,
    /// A string literal, including interpolated strings and the code inside
    /// their `${...}`.
    StringLiteral,
    /// An XML literal, where whitespace is part of the value.
    XmlLiteral,
    /// A block comment spanning several lines.
    Comment,
    /// An indentation region that isn't safe to re-indent.
    UnsafeIndentation(RegionProblem),
    /// Between `// format: off` and `// format: on` (or the end of the
    /// file), as in scalafmt.
    FormatOff,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct VerbatimRange {
    pub range: TextRange,
    pub reason: VerbatimReason,
}

impl Tree {
    /// Every verbatim range, sorted and non-overlapping. Nested ranges are
    /// merged into the outermost one, which keeps its reason.
    pub fn verbatim_ranges(&self) -> &[VerbatimRange] {
        self.verbatim.get_or_init(|| compute(self))
    }

    /// The verbatim range containing `offset`, if any.
    pub fn verbatim_at(&self, offset: TextSize) -> Option<&VerbatimRange> {
        let ranges = self.verbatim_ranges();
        let i = ranges.partition_point(|r| r.range.end() <= offset);
        ranges.get(i).filter(|r| r.range.start() <= offset)
    }
}

impl SyntaxNode<'_> {
    /// The verbatim range this whole node falls inside, if any.
    pub fn verbatim_range(&self) -> Option<&VerbatimRange> {
        let range = self.text_range();
        self.tree
            .verbatim_at(range.start())
            .filter(|r| r.range.contains_range(range))
    }
}

pub(super) type Cache = OnceLock<Vec<VerbatimRange>>;

fn compute(parse: &Tree) -> Vec<VerbatimRange> {
    let root = parse.syntax();
    let mut found = Vec::new();
    for element in root.descendants_with_tokens() {
        if element.text_range().is_empty() {
            continue;
        }
        let reason = match element {
            NodeOrToken::Node(node) => node_reason(&node),
            // A string without interpolation is a single token.
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::String => {
                Some(VerbatimReason::StringLiteral)
            }
            NodeOrToken::Token(_) => None,
        };
        if let Some(reason) = reason {
            found.push(VerbatimRange {
                range: element.text_range(),
                reason,
            });
        }
    }

    let comments: Vec<_> = root
        .tokens()
        .flat_map(|t| t.leading_trivia().chain(t.trailing_trivia()))
        .chain(parse.eof_trivia())
        .filter(SyntaxTrivia::is_comment)
        .collect();
    for comment in &comments {
        if comment.kind() == TriviaKind::BlockComment && comment.text().contains(['\n', '\r']) {
            found.push(VerbatimRange {
                range: comment.text_range(),
                reason: VerbatimReason::Comment,
            });
        }
    }
    found.extend(format_off_ranges(&comments, root.text_range().end()));

    // Outer ranges first, then fold nested and overlapping ones into them.
    found.sort_by_key(|r| (r.range.start(), std::cmp::Reverse(r.range.end())));
    let mut merged: Vec<VerbatimRange> = Vec::new();
    for next in found {
        match merged.last_mut() {
            Some(last) if next.range.start() < last.range.end() => {
                last.range = last.range.cover(next.range);
            }
            _ => merged.push(next),
        }
    }
    merged
}

fn node_reason(node: &SyntaxNode<'_>) -> Option<VerbatimReason> {
    let parent_kind = node.parent().map(|p| p.kind());
    match node.kind() {
        SyntaxKind::Error => return Some(VerbatimReason::SyntaxError),
        SyntaxKind::String | SyntaxKind::InterpolatedStringExpression => {
            return Some(VerbatimReason::StringLiteral);
        }
        SyntaxKind::InterpolatedString
            if parent_kind != Some(SyntaxKind::InterpolatedStringExpression) =>
        {
            return Some(VerbatimReason::StringLiteral);
        }
        kind if is_xml(kind) && !parent_kind.is_some_and(is_xml) => {
            return Some(VerbatimReason::XmlLiteral);
        }
        _ => {}
    }
    let has_missing_token = node
        .children_with_tokens()
        .any(|child| child.as_token().is_some_and(SyntaxToken::is_missing));
    if has_missing_token {
        return Some(VerbatimReason::SyntaxError);
    }
    node.region_problem().map(VerbatimReason::UnsafeIndentation)
}

fn is_xml(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::XmlElement
            | SyntaxKind::XmlExpression
            | SyntaxKind::XmlPattern
            | SyntaxKind::XmlCdata
            | SyntaxKind::XmlComment
            | SyntaxKind::XmlProcessingInstruction
            | SyntaxKind::XmlString
            | SyntaxKind::XmlText
            | SyntaxKind::XmlAttribute
            | SyntaxKind::XmlName
    )
}

/// The setting a `// format: on` or `/* format: off */` comment makes, if
/// it is one.
fn format_switch(comment: &SyntaxTrivia<'_>) -> Option<bool> {
    let text = comment.text();
    let body = text
        .strip_prefix("//")
        .or_else(|| text.strip_prefix("/*").and_then(|t| t.strip_suffix("*/")))?;
    match body.trim() {
        "format: on" => Some(true),
        "format: off" => Some(false),
        _ => None,
    }
}

fn format_off_ranges(comments: &[SyntaxTrivia<'_>], end_of_file: TextSize) -> Vec<VerbatimRange> {
    let mut ranges = Vec::new();
    let mut off_since = None;
    for comment in comments {
        match (format_switch(comment), off_since) {
            (Some(false), None) => off_since = Some(comment.text_range().start()),
            (Some(true), Some(start)) => {
                ranges.push(TextRange::new(start, comment.text_range().end()));
                off_since = None;
            }
            _ => {}
        }
    }
    if let Some(start) = off_since {
        ranges.push(TextRange::new(start, end_of_file));
    }
    ranges
        .into_iter()
        .map(|range| VerbatimRange {
            range,
            reason: VerbatimReason::FormatOff,
        })
        .collect()
}

/// Checks the ranges are sorted, disjoint, non-empty, inside the file, and
/// never cut through a token.
#[cfg(test)]
pub(super) fn assert_well_formed(parse: &Tree) {
    let ranges = parse.verbatim_ranges();
    for pair in ranges.windows(2) {
        assert!(
            pair[0].range.end() <= pair[1].range.start(),
            "overlap: {pair:?}"
        );
    }
    let end = parse.syntax().text_range().end();
    for r in ranges {
        assert!(!r.range.is_empty() && r.range.end() <= end, "{r:?}");
    }
    // Every code token is wholly inside or wholly outside each range.
    // Comment tokens can be split: trivia trims spaces tree-sitter
    // sometimes includes in them.
    for token in parse.syntax().tokens().filter(|t| !t.is_trivia()) {
        let range = token.text_range();
        if let Some(v) = parse.verbatim_at(range.start()) {
            assert!(v.range.contains_range(range), "{token:?} straddles {v:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn reasons(source: &str) -> Vec<(&str, VerbatimReason)> {
        // Leaked so the returned texts can borrow from it; fine in tests.
        let tree = Box::leak(Box::new(parse(source)));
        assert_well_formed(tree);
        tree.verbatim_ranges()
            .iter()
            .map(|r| (&tree.syntax().text()[r.range], r.reason))
            .collect()
    }

    #[test]
    fn plain_code_has_no_verbatim_ranges() {
        assert!(reasons("object A {\n  def f(x: Int) = x + 1 // note\n}\n").is_empty());
    }

    #[test]
    fn strings_are_verbatim() {
        let found = reasons(
            "val a = \"x  y\"\nval b = s\"${f(1,  2)} z\"\nval c = \"\"\"one\n  two\"\"\"\n",
        );
        assert_eq!(
            found,
            [
                ("\"x  y\"", VerbatimReason::StringLiteral),
                ("s\"${f(1,  2)} z\"", VerbatimReason::StringLiteral),
                ("\"\"\"one\n  two\"\"\"", VerbatimReason::StringLiteral),
            ]
        );
    }

    #[test]
    fn a_line_starting_inside_a_string_is_verbatim() {
        let source = "object A:\n  val c = \"\"\"one\n    two\"\"\"\n  val d = 1\n";
        let tree: Tree = parse(source);
        let line_start = |n: usize| -> TextSize {
            let offset: usize = source.split_inclusive('\n').take(n - 1).map(str::len).sum();
            TextSize::try_from(offset).unwrap()
        };
        assert!(tree.verbatim_at(line_start(2)).is_none());
        assert_eq!(
            tree.verbatim_at(line_start(3)).unwrap().reason,
            VerbatimReason::StringLiteral
        );
        assert!(tree.verbatim_at(line_start(4)).is_none());
    }

    #[test]
    fn multi_line_block_comments_are_verbatim() {
        let found = reasons("/** Docs.\n  * More.\n  */\nobject A /* short */\n");
        assert_eq!(
            found,
            [("/** Docs.\n  * More.\n  */", VerbatimReason::Comment)]
        );
    }

    #[test]
    fn syntax_errors_are_verbatim() {
        let found = reasons("object A {\n  def f( = 1\n");
        assert!(
            found.iter().all(|(_, r)| *r == VerbatimReason::SyntaxError),
            "{found:?}"
        );
        assert!(!found.is_empty());
    }

    #[test]
    fn unsafe_regions_are_verbatim_and_swallow_nested_ranges() {
        let source = "object O:\n  def h: Int =\n    return\n      if c then \"s\" else \"t\"\n";
        let found = reasons(source);
        assert_eq!(
            found,
            [(
                "return\n      if c then \"s\" else \"t\"\n",
                VerbatimReason::UnsafeIndentation(RegionProblem::MixedIndentation)
            )]
        );
    }

    #[test]
    fn format_off_and_on() {
        let source = "val a = 1\n// format: off\nval b  =  2\n// format: on\nval c = 3\n/* format: off */\nval d = 4\n";
        let found = reasons(source);
        assert_eq!(
            found,
            [
                (
                    "// format: off\nval b  =  2\n// format: on",
                    VerbatimReason::FormatOff
                ),
                ("/* format: off */\nval d = 4\n", VerbatimReason::FormatOff),
            ]
        );
    }

    #[test]
    fn xml_literals_are_verbatim() {
        let found = reasons("val x = <a href=\"y\">  text {1 + 2}</a>\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0],
            (
                "<a href=\"y\">  text {1 + 2}</a>",
                VerbatimReason::XmlLiteral
            )
        );
    }

    #[test]
    fn node_inside_a_range_reports_it() {
        let tree = parse("val b = s\"${f(1)}\"\n");
        let call = tree
            .syntax()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::CallExpression)
            .unwrap();
        assert_eq!(
            call.verbatim_range().unwrap().reason,
            VerbatimReason::StringLiteral
        );
        let val = tree.syntax().children().next().unwrap();
        assert!(val.verbatim_range().is_none());
    }
}
