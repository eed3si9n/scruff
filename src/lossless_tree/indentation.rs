//! Scala 3 indentation regions: blocks whose structure comes from
//! indentation instead of braces.
//!
//! Scala's rules inside a region:
//!
//! - Each statement starts its own line, unless it follows a `;`.
//! - All statements share the same indentation, which extends the
//!   enclosing region's indentation. Scala compares indentation as text, so
//!   a tab and spaces are never equal. `match` and `catch` cases may also sit
//!   at the enclosing indentation.
//! - An `end` marker usually sits at the indentation of the definition it
//!   closes, but Scala also accepts it deeper. Don't move one.
//!
//! tree-sitter-scala misparses some valid code inside regions. A line that
//! continues the one before it (`f` / `(a)`, or `return` / `if c then 1`)
//! starts a "statement" of its own, and an `end` marker inside braces can be
//! attached to the wrong construct. The
//! tree is then wrong about the program, and `check_equivalent` can't catch
//! a re-indent that changes its meaning. So a formatter should only
//! re-indent regions where [`SyntaxNode::region_problem`] finds nothing,
//! and copy the rest, including what's nested inside, unchanged.
//!
//! Shifting a well-formed region left or right is safe as long as it stays
//! deeper than the enclosing region. Joining a one-line region onto the line
//! that opens it (`def f =` / `1` to `def f = 1`) is valid Scala but changes
//! the tree, so `check_equivalent` reports it.

use super::{NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TriviaKind};
use crate::T;

/// Why a region isn't safe to re-indent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionProblem {
    /// A statement shares a line with code before it, without a `;`.
    NotOnOwnLine,
    /// Statements start at different indentations. Also how an infix
    /// operator at the end of a line shows up: tree-sitter-scala makes the
    /// next line a separate, deeper statement.
    MixedIndentation,
    /// The region isn't indented deeper than the region around it.
    NotDeeperThanEnclosing,
    /// An `end` marker in this region is attached to a construct other than
    /// the one it names.
    MisattachedEndMarker,
}

impl<'t> SyntaxNode<'t> {
    /// Whether this node's statements are laid out by indentation: an
    /// indented block or match cases, a class, object, enum or given body
    /// written with `:` or `with` instead of braces, or a multi-line
    /// `extension`.
    pub fn is_indentation_region(&self) -> bool {
        match self.kind() {
            // tree-sitter-scala also uses indented_block for a lambda body
            // directly inside braces, where indentation doesn't matter.
            SyntaxKind::IndentedBlock => !self.parent().is_some_and(|lambda| {
                lambda.kind() == SyntaxKind::LambdaExpression
                    && lambda
                        .parent()
                        .is_some_and(|block| block.kind() == SyntaxKind::Block)
            }),
            SyntaxKind::IndentedCases => true,
            SyntaxKind::TemplateBody | SyntaxKind::EnumBody | SyntaxKind::WithTemplateBody => !self
                .non_trivia_children()
                .any(|child| child.kind() == T!["{"]),
            // An extension's methods are its direct children; there is no
            // block node around them.
            SyntaxKind::ExtensionDefinition => self
                .extension_bodies()
                .next()
                .and_then(|body| first_code_token(&body))
                .is_some_and(|token| token.is_first_on_line()),
            _ => false,
        }
    }

    /// The statements of an indentation region, in order. Empty if this node
    /// isn't one.
    pub fn region_statements(&self) -> impl Iterator<Item = SyntaxElement<'t>> + use<'t> {
        let node = *self;
        let is_region = self.is_indentation_region();
        self.non_trivia_children()
            .filter(move |child| is_region && is_statement(&node, child))
    }

    /// The indentation of the region's statements: that of the first one
    /// that starts a line.
    pub fn region_indentation(&self) -> Option<&'t str> {
        self.region_statements()
            .filter_map(|statement| first_code_token(&statement))
            .find(SyntaxToken::is_first_on_line)
            .map(|token| token.indentation())
    }

    /// The nearest ancestor that is an indentation region, not counting
    /// this node.
    pub fn enclosing_region(&self) -> Option<SyntaxNode<'t>> {
        self.ancestors()
            .skip(1)
            .find(SyntaxNode::is_indentation_region)
    }

    /// Why this region isn't safe to re-indent, or `None` if it is (or isn't
    /// a region).
    pub fn region_problem(&self) -> Option<RegionProblem> {
        let indent = self.region_indentation()?;
        let firsts: Vec<_> = self
            .region_statements()
            .filter_map(|statement| first_code_token(&statement))
            .collect();
        for first in &firsts {
            if first.is_first_on_line() {
                if first.indentation() != indent {
                    return Some(RegionProblem::MixedIndentation);
                }
            } else if !follows_semicolon(first) {
                return Some(RegionProblem::NotOnOwnLine);
            }
        }
        if let Some(outer) = self.enclosing_region().and_then(|r| r.region_indentation()) {
            let deeper = indent.len() > outer.len() && indent.starts_with(outer);
            let cases_at_same_level = self.kind() == SyntaxKind::IndentedCases && indent == outer;
            if !deeper && !cases_at_same_level {
                return Some(RegionProblem::NotDeeperThanEnclosing);
            }
        }
        let misattached = self.descendants().any(|node| {
            node.kind() == SyntaxKind::EndMarker
                && !node.end_marker_matches_parent()
                && node.enclosing_region() == Some(*self)
        });
        misattached.then_some(RegionProblem::MisattachedEndMarker)
    }

    /// For an `end` marker: whether the construct it names (`end match`,
    /// `end if`, `end MyClass`) is the node tree-sitter attached it to.
    /// `true` for other nodes.
    pub fn end_marker_matches_parent(&self) -> bool {
        if self.kind() != SyntaxKind::EndMarker {
            return true;
        }
        let Some(parent) = self.parent() else {
            return false;
        };
        let name = self.text().trim().strip_prefix("end").unwrap_or("").trim();
        let keyword_kind = match name {
            "if" => Some(SyntaxKind::IfExpression),
            "while" => Some(SyntaxKind::WhileExpression),
            "for" => Some(SyntaxKind::ForExpression),
            "match" => Some(SyntaxKind::MatchExpression),
            "try" => Some(SyntaxKind::TryExpression),
            "new" => Some(SyntaxKind::InstanceExpression),
            "given" => Some(SyntaxKind::GivenDefinition),
            "extension" => Some(SyntaxKind::ExtensionDefinition),
            "val" => Some(SyntaxKind::ValDefinition),
            "this" => Some(SyntaxKind::FunctionDefinition),
            _ => None,
        };
        if keyword_kind == Some(parent.kind()) {
            return true;
        }
        // `end Name` closes a definition named Name, which may also be a
        // keyword-like name such as a method called `extension`. A val's or
        // var's name is in its `pattern` field. Packages may be dotted:
        // `package a.b` / `end b`.
        let own = parent
            .child_by_field_name("name")
            .or_else(|| parent.child_by_field_name("pattern"));
        own.is_some_and(|own| {
            let own = own.text();
            own == name || own.ends_with(&format!(".{name}"))
        })
    }

    fn extension_bodies(&self) -> impl Iterator<Item = SyntaxElement<'t>> + use<'t> {
        self.non_trivia_children()
            .filter(|child| child.field_name() == Some("body"))
    }
}

fn follows_semicolon(token: &SyntaxToken<'_>) -> bool {
    std::iter::successors(token.prev_token(), SyntaxToken::prev_token)
        .find(SyntaxToken::owns_trivia)
        .is_some_and(|previous| previous.kind() == T![";"])
}

fn is_statement(region: &SyntaxNode<'_>, child: &SyntaxElement<'_>) -> bool {
    if region.kind() == SyntaxKind::ExtensionDefinition {
        return child.field_name() == Some("body");
    }
    // Leaves out `:`, `with`, `;` and the like. Named tokens such as a bare
    // identifier can be statements.
    child.kind().is_named() && child.kind() != SyntaxKind::EndMarker
}

/// The first token of an element that isn't trivia or missing.
pub(super) fn first_code_token<'t>(element: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match element {
        NodeOrToken::Token(token) => Some(*token).filter(SyntaxToken::owns_trivia),
        NodeOrToken::Node(node) => node.tokens().find(SyntaxToken::owns_trivia),
    }
}

impl<'t> SyntaxToken<'t> {
    /// Whether no code comes before this token on its line. Whitespace and
    /// comments may.
    pub fn is_first_on_line(&self) -> bool {
        let mut previous = self.prev_token();
        while let Some(token) = previous {
            if token.owns_trivia() {
                // Trailing trivia never contains a newline, so the line break,
                // if any, is in this token's leading trivia.
                return self
                    .leading_trivia()
                    .any(|t| t.kind() == TriviaKind::Newline);
            }
            previous = token.prev_token();
        }
        true
    }

    /// The spaces and tabs at the start of this token's line.
    pub fn indentation(&self) -> &'t str {
        let text = self.tree.text.as_str();
        let start = usize::from(self.text_range().start());
        let line_start = text[..start].rfind(['\n', '\r']).map_or(0, |i| i + 1);
        let line = &text[line_start..];
        &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    fn regions(source: &str) -> Vec<(SyntaxKind, Vec<String>)> {
        let tree = parse(source);
        tree.syntax()
            .descendants()
            .filter(SyntaxNode::is_indentation_region)
            .map(|region| {
                let statements = region
                    .region_statements()
                    .map(|s| s.text().trim().to_string())
                    .collect();
                (region.kind(), statements)
            })
            .collect()
    }

    #[test]
    fn indented_blocks_and_cases() {
        let found =
            regions("def f(x: Int) =\n  val y = x\n  y match\n    case 1 => 2\n    case _ => 3\n");
        assert_eq!(
            found,
            [
                (
                    SyntaxKind::IndentedBlock,
                    vec![
                        "val y = x".to_string(),
                        "y match\n    case 1 => 2\n    case _ => 3".to_string()
                    ]
                ),
                (
                    SyntaxKind::IndentedCases,
                    vec!["case 1 => 2".to_string(), "case _ => 3".to_string()]
                ),
            ]
        );
    }

    #[test]
    fn colon_bodies_are_regions_and_braces_are_not() {
        let found = regions("object A:\n  val a = 1\nclass B { val b = 2 }\nenum C:\n  case X\n");
        let kinds: Vec<_> = found.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, [SyntaxKind::TemplateBody, SyntaxKind::EnumBody]);
        assert_eq!(found[0].1, ["val a = 1"]);
    }

    #[test]
    fn given_with_body() {
        let indented = regions("given Ord[Int] with\n  def compare(a: Int, b: Int) = a - b\n");
        assert_eq!(indented.len(), 1);
        assert_eq!(indented[0].0, SyntaxKind::WithTemplateBody);
        assert!(regions("given Ord[Int] with { def c = 1 }").is_empty());
    }

    #[test]
    fn multi_line_extension_is_a_region() {
        let found = regions("extension (s: String)\n  def a = 1\n  def b = 2\n");
        assert_eq!(
            found,
            [(
                SyntaxKind::ExtensionDefinition,
                vec!["def a = 1".to_string(), "def b = 2".to_string()]
            )]
        );
        assert!(regions("extension (s: String) def a = 1").is_empty());
    }

    #[test]
    fn end_markers_are_not_statements() {
        let found = regions("object A:\n  def h =\n    1\n  end h\nend A\n");
        assert_eq!(
            found[0],
            (
                SyntaxKind::TemplateBody,
                vec!["def h =\n    1\n  end h".to_string()]
            )
        );
        assert_eq!(found[1], (SyntaxKind::IndentedBlock, vec!["1".to_string()]));
    }

    #[test]
    fn enclosing_region_skips_brace_blocks() {
        let tree = parse("object A:\n  def f = {\n    val x = 1\n    x\n  }\n");
        let val = tree
            .syntax()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ValDefinition)
            .unwrap();
        assert_eq!(
            val.enclosing_region().map(|r| r.kind()),
            Some(SyntaxKind::TemplateBody)
        );
    }

    fn problems(source: &str) -> Vec<(SyntaxKind, Option<RegionProblem>)> {
        let tree = parse(source);
        tree.syntax()
            .descendants()
            .filter(SyntaxNode::is_indentation_region)
            .map(|region| (region.kind(), region.region_problem()))
            .collect()
    }

    #[test]
    fn well_formed_regions_have_no_problem() {
        let found = problems("object A:\n  def f =\n    val x = 1; val y = 2\n    x + y\n");
        assert_eq!(
            found,
            [
                (SyntaxKind::TemplateBody, None),
                (SyntaxKind::IndentedBlock, None)
            ]
        );
    }

    #[test]
    fn cases_may_sit_at_the_match_indentation() {
        let found = problems("def f(x: Int) =\n  x match\n  case 1 => 2\n  case _ => 3\n");
        assert_eq!(
            found,
            [
                (SyntaxKind::IndentedBlock, None),
                (SyntaxKind::IndentedCases, None)
            ]
        );
    }

    #[test]
    fn misparsed_continuation_lines_are_flagged() {
        // Scala reads `f(a)` and `return if c then 1 else 2`; tree-sitter-scala
        // 0.26.4 starts a statement on each deeper line.
        let call = problems("object O:\n  def g =\n    f\n      (a)\n");
        assert_eq!(
            call[1],
            (
                SyntaxKind::IndentedBlock,
                Some(RegionProblem::MixedIndentation)
            )
        );
        let ret = problems("object O:\n  def h: Int =\n    return\n      if c then 1 else 2\n");
        assert_eq!(
            ret[1],
            (
                SyntaxKind::IndentedBlock,
                Some(RegionProblem::MixedIndentation)
            )
        );
    }

    #[test]
    fn infix_operators_over_lines_are_read_whole() {
        // tree-sitter-scala 0.26.3 split these; 0.26.4 reads each as one
        // expression, as Scala does.
        let trailing = problems("object O:\n  def g =\n    a getOrElse\n      b\n");
        assert_eq!(trailing[1], (SyntaxKind::IndentedBlock, None));
        let leading = problems("object O:\n  def s =\n    a == 0\n    ||\n      b\n");
        assert_eq!(leading[1], (SyntaxKind::IndentedBlock, None));
    }

    #[test]
    fn operator_named_statements_are_fine() {
        let found = problems("object O:\n  def f =\n    log()\n    ???\n");
        assert_eq!(
            found,
            [
                (SyntaxKind::TemplateBody, None),
                (SyntaxKind::IndentedBlock, None)
            ]
        );
    }

    #[test]
    fn end_markers_match_what_they_close() {
        let tree = parse(concat!(
            "object A:\n  def h =\n    x match\n      case _ => 1\n    end match\n  end h\n",
            "  val v =\n    1\n  end v\n  var w =\n    2\n  end w\n",
            "  def extension =\n    3\n  end extension\nend A\n",
        ));
        let markers: Vec<_> = tree
            .syntax()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::EndMarker)
            .collect();
        assert_eq!(markers.len(), 6);
        assert!(markers.iter().all(SyntaxNode::end_marker_matches_parent));
    }

    #[test]
    fn misattached_end_marker_flags_its_region() {
        // `end g` names a different definition from the one it closes.
        let source = "object O:\n  def f =\n    1\n  end g\n";
        let tree = parse(source);
        let marker = tree
            .syntax()
            .descendants()
            .find(|n| n.kind() == SyntaxKind::EndMarker)
            .unwrap();
        assert!(
            !marker.end_marker_matches_parent(),
            "tree-sitter changed; {:#?}",
            tree.syntax()
        );
        let region = marker.enclosing_region().unwrap();
        assert_eq!(
            region.region_problem(),
            Some(RegionProblem::MisattachedEndMarker)
        );
    }

    #[test]
    fn lambda_body_in_braces_is_not_a_region() {
        let found = problems("object O:\n  def r = m.map { x =>\n     val a = 1\n      a\n  }\n");
        assert_eq!(found, [(SyntaxKind::TemplateBody, None)]);
    }

    #[test]
    fn first_on_line_and_indentation() {
        let tree = parse("object A:\n\t/* c */ val x = 1; val y = 2\n");
        let token = |text: &str| tree.syntax().tokens().find(|t| t.text() == text).unwrap();
        assert!(token("object").is_first_on_line());
        assert!(
            token("val").is_first_on_line(),
            "comments before it don't count"
        );
        assert!(!token("x").is_first_on_line());
        assert!(
            !tree
                .syntax()
                .tokens()
                .filter(|t| t.text() == "val")
                .nth(1)
                .unwrap()
                .is_first_on_line()
        );
        assert_eq!(token("x").indentation(), "\t");
        assert_eq!(token("object").indentation(), "");
    }
}
