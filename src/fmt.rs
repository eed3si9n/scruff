use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;

use crate::T;

mod end_marker;
mod indent;
mod rewrite;
mod spacing;

use crate::config::Dialect;
use crate::doc::{AlignPart, Doc, FirstLine, PrintOptions, Shift, print};
use crate::lossless_tree::{
    Difference, NodeOrToken, SyntaxElement, SyntaxError, SyntaxKind, SyntaxNode, SyntaxToken,
    SyntaxTrivia, TextRange, TextSize, Tree, TriviaKind, VerbatimRange, VerbatimReason,
    check_equivalent_trees, parse, scaladoc_lines,
};

/// Formatting is deliberately not configurable beyond this. Indentation is
/// always two spaces, and lines end in `\n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatOptions {
    /// Defaults to 100, sbt's scalafmt `maxColumn`.
    pub max_width: usize,
    /// The code's Scala dialect, whose new syntax scruff rewrites to.
    /// Defaults to Scala 3.
    pub dialect: Dialect,
}

impl Default for FormatOptions {
    fn default() -> Self {
        FormatOptions {
            max_width: 100,
            dialect: Dialect::SCALA3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The input doesn't parse cleanly.
    SyntaxErrors(Vec<SyntaxError>),
    /// The formatter produced code that differs from the input. A bug in a
    /// formatting rule; the input is left alone.
    NotEquivalent(Difference),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::SyntaxErrors(errors) => {
                write!(f, "not formatting code with syntax errors")?;
                if let Some(first) = errors.first() {
                    write!(f, ", the first: {first}")?;
                }
                Ok(())
            }
            FormatError::NotEquivalent(difference) => {
                write!(f, "formatting would change the code: {difference}")
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// Formats Scala source.
///
/// A file whose lines all end in `\r\n`, as Git checks files out on Windows
/// by default, is formatted as its `\n` twin and keeps its `\r\n`. The round
/// trip is exact, so a string's line endings stay as written. Any other
/// file's line breaks become `\n`.
pub fn format(source: &str, options: &FormatOptions) -> Result<String, FormatError> {
    if !is_crlf(source) {
        return format_with(source, options, Rules::Structural, STYLE);
    }
    let lf = source.replace("\r\n", "\n");
    let formatted = format_with(&lf, options, Rules::Structural, STYLE)?;
    Ok(formatted.replace('\n', "\r\n"))
}

/// Whether `source` has line breaks, all of them `\r\n`.
fn is_crlf(source: &str) -> bool {
    let newlines = source.matches('\n').count();
    newlines > 0 && source.matches("\r\n").count() == newlines
}

/// Which rules build the document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rules {
    /// A node's own rule where it has one, otherwise the fallback.
    Structural,
    /// Only the fallback: children in order, spacing from [`spacing`].
    /// Tests compare it with [`Rules::Structural`].
    #[cfg_attr(not(test), allow(dead_code))]
    Fallback,
}

/// Choices scruff makes for everyone, gathered in one place so they're easy
/// to revisit. They aren't options: `format` always uses [`STYLE`], and
/// tests may try others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    /// Blank lines in a row, at most, between definitions in a template:
    /// the body of a class, object, trait or enum, an extension's methods,
    /// a package, or the file.
    max_blank_lines_in_templates: usize,
    /// Blank lines in a row, at most, anywhere else: in a block or a method
    /// body, including between nested functions.
    max_blank_lines_in_blocks: usize,
    /// Line breaks, at least, in a statement that gets a Scala 3 end
    /// marker, as `end f` after a long `def f =`. See [`end_marker`].
    end_marker_min_breaks: usize,
    /// Columns past its line's indentation, at most, where a definition's
    /// `match` starts on the `=`'s line; past them it starts the next line.
    /// Any other control structure always does. See
    /// [`Formatter::definition_body`].
    max_body_start: usize,
    /// Line breaks, at least, in a statement the printer counts as long:
    /// a val, var or def over several lines gets a blank line above it.
    long_statement_min_breaks: usize,
}

/// After Black, the Python formatter, which allows 2 blank lines between
/// module-level definitions and 1 inside functions. Scala's templates play
/// the part of Python's modules. sbt's code agrees: its runs of 2 in blocks
/// are all in comments or `format: off` blocks, which are copied as
/// written.
///
/// End markers follow sbt's `rewrite.scala3.endMarker.insert.breaks.min =
/// 20`. sbt doesn't remove them from short statements, and neither does
/// scruff.
const STYLE: Style = Style {
    max_blank_lines_in_templates: 2,
    max_blank_lines_in_blocks: 1,
    end_marker_min_breaks: 20,
    max_body_start: 30,
    long_statement_min_breaks: 1,
};

fn format_with(
    source: &str,
    options: &FormatOptions,
    rules: Rules,
    style: Style,
) -> Result<String, FormatError> {
    let original = parse(source);
    let (once, once_tree) = format_once(&original, options, rules, style)?;
    if once == source {
        return Ok(once);
    }
    let once_tree = once_tree.as_ref().unwrap_or(&original);
    // Breaking a line can leave something to rewrite, as braces around a
    // block that no longer fits on one: a second pass rewrites it, so that
    // formatting the result changes nothing.
    if rewrite::changes(once_tree, options.dialect)
        && let Ok((twice, twice_tree)) = format_once(once_tree, options, rules, style)
        && twice_tree.is_none_or(|tree| check_equivalent_trees(&original, &tree).is_ok())
    {
        return Ok(twice);
    }
    Ok(once)
}

/// One formatting pass over `original`, already parsed: the text, checked
/// against the original, and its tree, or `None` where the text is
/// `original`'s own.
///
/// A text is parsed only when it's new. Where formatting gives back the
/// text a tree was parsed from, the original or the rewrite, that tree is
/// the text's, and has been checked already.
fn format_once(
    original: &Tree,
    options: &FormatOptions,
    rules: Rules,
    style: Style,
) -> Result<(String, Option<Tree>), FormatError> {
    if !original.errors().is_empty() {
        return Err(FormatError::SyntaxErrors(original.errors().to_vec()));
    }
    // Rewritten to the dialect's syntax first, where tree-sitter-scala reads
    // the result as the same code.
    let rewritten = rewrite::rewrite(original, options.dialect).map(|r| r.tree);
    let tree = rewritten.as_ref().unwrap_or(original);
    let print_options = PrintOptions {
        max_width: options.max_width,
        indent: "  ".to_string(),
        line_ending: "\n".to_string(),
    };
    let end_markers = options.dialect.significant_indentation;
    let leading_infix = options.dialect.leading_infix_operators;
    let mut difference = None;
    // Boolean chains written over several lines are laid out first, and
    // then, if nothing else works, kept as written: tree-sitter-scala's
    // reading of the lines after one can depend on its line breaks.
    let mut any_broken_chains = false;
    for broken_chains in [true, false] {
        if !broken_chains && !any_broken_chains {
            break;
        }
        let relaid = relaid(tree, rules, style, broken_chains);
        any_broken_chains |= relaid.broken_chains;
        let mut layout = Some(indent::Layout::new(tree, &relaid));
        let reindented = layout.as_ref().is_some_and(|l| !l.is_empty());
        // What to try, in order, until the result means the same. Where
        // tree-sitter-scala reads the layout differently from scalac,
        // moving lines can change the tree, and so can an end marker it
        // attaches to something else. Then leave out end markers, or keep
        // the indentation as written, keeping the rest of the formatting.
        let tries = [(true, true), (true, false), (false, true), (false, false)]
            .into_iter()
            .filter(|&(reindent, markers)| (reindent || reindented) && (!markers || end_markers));
        for (reindent, markers) in tries {
            let layout = if reindent {
                layout
                    .take()
                    .unwrap_or_else(|| indent::Layout::new(tree, &relaid))
            } else {
                indent::Layout::default()
            };
            let options = BuildOptions {
                end_markers: markers,
                leading_infix,
                broken_chains,
            };
            let doc = build_doc(tree, rules, style, layout, options);
            let formatted = print(&doc, &print_options);
            if formatted == tree.syntax().text() {
                return Ok((formatted, rewritten));
            }
            let formatted_tree = parse(&formatted);
            match check_equivalent_trees(original, &formatted_tree) {
                Ok(()) => return Ok((formatted, Some(formatted_tree))),
                Err(d) => difference = Some(d),
            }
        }
    }
    let difference = difference.expect("one try at least");
    Err(FormatError::NotEquivalent(difference))
}

/// The document for a parsed file, before printing.
pub fn to_doc<'t>(tree: &'t Tree) -> Doc<'t> {
    let relaid = relaid(tree, Rules::Structural, STYLE, true);
    build_doc(
        tree,
        Rules::Structural,
        STYLE,
        indent::Layout::new(tree, &relaid),
        BuildOptions::default(),
    )
}

/// What the formatter lays out itself, for [`indent::Layout`]: the lists
/// the list rule lays out, as `(` and `)` offsets, and the lines boolean
/// chains join or break, with their parentheses' where those are the
/// chain's own.
fn relaid(tree: &Tree, rules: Rules, style: Style, broken_chains: bool) -> indent::Relaid {
    let mut relaid = indent::Relaid::default();
    if rules != Rules::Structural {
        return relaid;
    }
    let options = BuildOptions {
        broken_chains,
        ..BuildOptions::default()
    };
    let formatter = Formatter::new(tree, rules, style, indent::Layout::default(), options);
    let line_first =
        |token: SyntaxToken<'_>| token.is_first_on_line().then(|| token.text_range().start());
    for node in tree.syntax().descendants() {
        if let Some(parts) = formatter.list_parts(node) {
            relaid.lists.push((
                parts.open.text_range().start(),
                parts.close.text_range().start(),
            ));
        }
        if let Some(parts) = formatter.infix_parts(node) {
            relaid.broken_chains |= formatter.has_line_break(node.text_range());
            for (op, operand) in parts.operators.iter().zip(&parts.operands[1..]) {
                relaid.joined.extend(line_first(*op));
                relaid
                    .joined
                    .extend(first_code_token(operand).and_then(line_first));
            }
        }
        if let Some((chain, close)) = formatter.chain_parens_parts(node) {
            relaid
                .joined
                .extend(first_code_token(&NodeOrToken::Node(chain)).and_then(line_first));
            relaid.joined.extend(line_first(close));
        }
        if let Some(parts) = formatter.braced_parts(node) {
            let body = parts
                .lambda
                .as_ref()
                .map_or(&parts.inner, |(_, _, body)| body);
            relaid
                .joined
                .extend(first_code_token(body).and_then(line_first));
            relaid.joined.extend(line_first(parts.close));
        }
    }
    relaid.joined.sort();
    relaid.joined.dedup();
    relaid
}

/// What a [`Formatter`] does beyond its rules.
#[derive(Clone, Copy)]
struct BuildOptions {
    /// Whether long statements get an end marker: only where the dialect
    /// has significant indentation.
    end_markers: bool,
    /// Whether a line may start with an infix operator.
    leading_infix: bool,
    /// Whether boolean chains written over several lines are laid out, or
    /// kept as written.
    broken_chains: bool,
}

impl Default for BuildOptions {
    fn default() -> BuildOptions {
        BuildOptions {
            end_markers: true,
            leading_infix: true,
            broken_chains: true,
        }
    }
}

fn build_doc<'t>(
    tree: &'t Tree,
    rules: Rules,
    style: Style,
    layout: indent::Layout,
    options: BuildOptions,
) -> Doc<'t> {
    let mut formatter = Formatter::new(tree, rules, style, layout, options);
    let body = formatter.node(tree.syntax());
    // The file ends with its last code or comment and one line break, as in
    // scalafmt, unless what's at the end is copied verbatim.
    let mut eof: Vec<_> = tree.eof_trivia().collect();
    while eof.last().is_some_and(|t| {
        matches!(t.kind(), TriviaKind::Newline | TriviaKind::Whitespace)
            && formatter.is_free(t.text_range().start())
    }) {
        eof.pop();
    }
    // Popping stopped at verbatim whitespace: its line breaks are kept.
    let ends_verbatim = eof
        .last()
        .is_some_and(|t| matches!(t.kind(), TriviaKind::Newline | TriviaKind::Whitespace));
    let mut docs = Vec::new();
    formatter.trivia_within_lines(&mut docs, &eof, Gap::LineStart, None);
    let end = if formatter.at_file_start || ends_verbatim {
        Doc::Nil
    } else {
        Doc::hardline()
    };
    body + Doc::concat(docs) + end
}

/// Builds a [`Doc`] from the tree, copying verbatim ranges as they're
/// reached.
struct Formatter<'t> {
    tree: &'t Tree,
    rules: Rules,
    text: &'t str,
    verbatim: &'t [VerbatimRange],
    /// The first verbatim range that hasn't ended before the current
    /// position.
    next_verbatim: usize,
    /// End of the last verbatim range copied; source before it is done.
    copied_until: TextSize,
    /// Where the token starts whose leading trivia a rule already printed,
    /// ahead of a group that must not contain its line break.
    leading_done: Option<TextSize>,
    /// Statements whose leading trivia opened a frame for blank lines, which
    /// the printer closes after the statement, innermost last.
    blank_open: Vec<(SyntaxNode<'t>, u32)>,
    /// Whether the leading trivia being built is a statement's, which the
    /// printer may add blank lines above.
    blank_anchor_wanted: bool,
    /// Where the token starts whose leading line break may be a space: a
    /// body joining its keyword's line.
    joining: Option<TextSize>,
    /// A node whose frame its rule places instead: a lambda's colon
    /// argument in a chain, nested deeper than its own line.
    unframed: Option<SyntaxNode<'t>>,
    /// Where the tokens start whose trailing gap a rule prints itself, such
    /// as the line break before a `}`. Nested rules can each add one.
    trailing_skipped: Vec<TextSize>,
    /// Where lines go: frames, nested containers, and each line's offset.
    layout: indent::Layout,
    /// Whether no code or comment has been printed yet: line breaks before
    /// the first one are dropped.
    at_file_start: bool,
    /// Line breaks since the last code or comment.
    line_breaks: usize,
    style: Style,
    /// Where each comment and each line break starts, in order, to ask
    /// whether a range has one without scanning it.
    comments: Vec<TextSize>,
    breaks: Vec<TextSize>,
    /// What [`Formatter::prints_on_one_line`] found for each node, since
    /// rules ask it again for the nodes inside them.
    one_line: RefCell<HashMap<SyntaxNode<'t>, bool>>,
    /// Whether long statements get an end marker: only where the dialect
    /// has significant indentation.
    end_markers: bool,
    /// Where the last blocks start of the statements being marked,
    /// innermost last, until `element` reaches them.
    count_from: Vec<TextSize>,
    /// End markers in the source that the printer stands in for, innermost
    /// last, left out when reached.
    dropped_markers: Vec<SyntaxNode<'t>>,
    /// Whether a line may start with an infix operator: only where the
    /// dialect allows it. Elsewhere a broken boolean chain is wrapped in
    /// parentheses.
    leading_infix: bool,
    /// Whether boolean chains written over several lines are laid out.
    broken_chains: bool,
    /// Definition bodies on their `=`'s line that may move to the next,
    /// innermost last, until reached, with where their line goes and how
    /// much deeper the line they were on is than the definition's: see
    /// [`DefinitionBody::SameLine`].
    moving: Vec<(SyntaxNode<'t>, isize, isize)>,
    /// Definition bodies written on the line after their `=` that may join
    /// it, innermost last, until reached: the one statement of the block,
    /// where there is one, and the levels its lines come back by.
    joined: Vec<(SyntaxNode<'t>, isize)>,
}

/// What comes just before a piece of trivia on its line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gap {
    /// Nothing: the start of a line, or of the file.
    LineStart,
    /// Code, with no space yet.
    AfterCode,
    /// A comment, with no space yet.
    AfterComment,
    /// Whitespace kept as written.
    Space,
}

/// What a stretch of source is, so it can be printed the right way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Piece {
    Code,
    Whitespace,
    Newline,
    Comment,
}

impl<'t> Formatter<'t> {
    fn new(
        tree: &'t Tree,
        rules: Rules,
        style: Style,
        layout: indent::Layout,
        options: BuildOptions,
    ) -> Formatter<'t> {
        Formatter {
            tree,
            rules,
            text: tree.syntax().text(),
            verbatim: tree.verbatim_ranges(),
            next_verbatim: 0,
            copied_until: TextSize::from(0),
            leading_done: None,
            blank_open: Vec::new(),
            blank_anchor_wanted: false,
            joining: None,
            unframed: None,
            trailing_skipped: Vec::new(),
            layout,
            at_file_start: true,
            line_breaks: 0,
            style,
            comments: comment_starts(tree),
            breaks: tree
                .syntax()
                .text()
                .match_indices(['\n', '\r'])
                .filter_map(|(i, _)| TextSize::try_from(i).ok())
                .collect(),
            one_line: RefCell::default(),
            end_markers: options.end_markers,
            count_from: Vec::new(),
            dropped_markers: Vec::new(),
            leading_infix: options.leading_infix,
            broken_chains: options.broken_chains,
            moving: Vec::new(),
            joined: Vec::new(),
        }
    }

    /// Whether a comment starts in `range`.
    fn has_comment(&self, range: TextRange) -> bool {
        has_offset_in(&self.comments, range)
    }

    /// Whether a line break is in `range`.
    fn has_line_break(&self, range: TextRange) -> bool {
        has_offset_in(&self.breaks, range)
    }

    fn node(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        let doc = self.node_with_markers(node);
        // The frame for blank lines above the statement, if its leading
        // trivia opened one, closes with it; frames inside that never did
        // go with it.
        match self.blank_open.iter().rposition(|(n, _)| *n == node) {
            Some(i) => {
                let id = self.blank_open[i].1;
                self.blank_open.truncate(i);
                doc + Doc::BlankEnd { id }
            }
            None => doc,
        }
    }

    fn node_with_markers(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        // A definition's body that starts the line after its `=`.
        // If it breaks, the lines inside it were placed from the line the
        // body was on, so they move as far as its first line does.
        if let Some(&(body, definition, deeper)) = self.moving.last()
            && body == node
        {
            self.moving.pop();
            self.skip_leading(&NodeOrToken::Node(node));
            let (limit, first_line) = self.body_start(node);
            let body = self.node(node);
            let broken = Shift {
                levels: 1,
                columns: -deeper,
            };
            return Doc::starts_past(
                limit,
                (Doc::hardline().offset(definition + deeper), broken),
                (Doc::text(" "), Shift::default()),
                body,
                first_line,
            );
        }
        // A body written on the next line: joining the `=`'s line, the
        // lines inside it, nested a level deeper than the definition in its
        // block, come back a level.
        if let Some(&(body, levels)) = self.joined.last()
            && body == node
        {
            self.joined.pop();
            let (limit, first_line) = self.body_start(node);
            let leading = self.leading_of(node);
            let body = self.node(node);
            let back = Shift { levels, columns: 0 };
            return Doc::starts_past(
                limit,
                (leading, Shift::default()),
                (Doc::text(" "), back),
                body,
                first_line,
            );
        }
        if self.dropped_markers.last() == Some(&node) {
            self.dropped_markers.pop();
            return Doc::Nil;
        }
        // A statement that ends in a long indented region gets an end
        // marker, if the printer finds it long. The leading trivia goes
        // first, so the marker is counted and placed from the statement's
        // own first line.
        if let Some(marker) = self.end_marker(node) {
            let leading = self.leading_of(node);
            let min_breaks = self.style.end_marker_min_breaks;
            // Counted from the last block, which `element` marks.
            let (pending, dropping) = (self.count_from.len(), self.dropped_markers.len());
            if let Some(at) = marker.last_block {
                self.count_from.push(at);
            }
            // The printer prints it, if the statement is long.
            if let Some(existing) = marker.existing {
                self.dropped_markers.push(existing);
            }
            let body = self.node_unmarked(node);
            self.count_from.truncate(pending);
            self.dropped_markers.truncate(dropping);
            return leading + body.end_marker(format!("end {}", marker.name), min_breaks);
        }
        self.node_unmarked(node)
    }

    /// The statement's end marker, if it may get one: see
    /// [`end_marker`]. Not where it or the line break after it is copied
    /// verbatim, nor with code after it on its line.
    fn end_marker(&self, node: SyntaxNode<'t>) -> Option<end_marker::EndMarker<'t>> {
        if !self.end_markers {
            return None;
        }
        let marker = end_marker::end_marker(node)?;
        let end = node.last_code_token()?.text_range().end();
        let next = std::iter::successors(node.last_token()?.next_token(), SyntaxToken::next_token)
            .find(SyntaxToken::owns_trivia);
        let i = self.breaks.partition_point(|&o| o < end);
        let line_break = self.breaks.get(i).copied();
        let free_after = match (next, line_break) {
            (Some(next), Some(at)) if at < next.text_range().start() => self.is_free(at),
            // At the end of the file, which ends in a line break anyway.
            (None, at) => at.is_none_or(|at| self.is_free(at)) && self.is_free(end),
            _ => false,
        };
        // A marker the printer stands in for is free, and where the
        // printer would put it: on a line of its own, at the indentation
        // of the statement's first line.
        let existing_free = marker.existing.is_none_or(|m| {
            let at = m.text_range().start();
            let first = node.tokens().find(SyntaxToken::owns_trivia);
            // Its line break and indentation too, which go with it.
            let previous = m.first_token().and_then(|t| t.prev_token());
            let before = std::iter::successors(previous, SyntaxToken::prev_token)
                .find(SyntaxToken::owns_trivia)
                .map_or(at, |t| t.text_range().end());
            self.gap_is_free(TextRange::new(before, at))
                && self.is_free(at)
                && first.is_some_and(|first| {
                    let before = &self.text[self.line_start(at)..usize::from(at)];
                    before.trim().is_empty()
                        && before == self.indentation_at(first.text_range().start())
                })
        });
        (free_after && existing_free && self.is_free(node.text_range().start())).then_some(marker)
    }

    /// Where the line with `offset` starts.
    fn line_start(&self, offset: TextSize) -> usize {
        self.text[..usize::from(offset)]
            .rfind(['\n', '\r'])
            .map_or(0, |i| i + 1)
    }

    /// The spaces and tabs that start the line with `offset`.
    fn indentation_at(&self, offset: TextSize) -> &'t str {
        let line = &self.text[self.line_start(offset)..];
        &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
    }

    fn node_unmarked(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        // A body that may join its keyword's line: laid out as ever if it
        // breaks, with the line break before it a space if it fits.
        if let Some(first) = self.joinable_body(node) {
            self.joining = Some(first);
            return self.node_framed(node).group();
        }
        self.node_framed(node)
    }

    fn node_framed(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        // A frame places the lines inside it from the line it starts on.
        if self.layout.frames_after_leading(node) && self.unframed != Some(node) {
            let leading = self.leading_of(node);
            leading + self.node_body(node).from_line_indent()
        } else {
            self.node_body(node)
        }
    }

    /// Where it starts, for a body of one expression after `then`, `else`,
    /// an `if`'s condition, `try` or `finally`, or a `catch`'s one `case`:
    /// on the keyword's line if it fits, as `if c then a`, `else b`,
    /// `try a`, `catch case e => b` and `finally c`, or else where it would
    /// be anyway. Not with comments around it, a body over several lines,
    /// or one with an `if` inside, which could take an `else` after it; nor
    /// after an `if` condition over several lines, or inside parentheses.
    fn joinable_body(&self, node: SyntaxNode<'t>) -> Option<TextSize> {
        let owner = node.parent()?;
        let keyword =
            std::iter::successors(node.first_token()?.prev_token(), SyntaxToken::prev_token)
                .find(SyntaxToken::owns_trivia)?;
        let expected = match (node.kind(), owner.kind(), node.field_name()) {
            (
                SyntaxKind::IndentedBlock,
                SyntaxKind::IfExpression,
                Some("consequence" | "alternative"),
            ) => {
                // After the condition's `)`, or `then` or `else`.
                matches!(keyword.text(), "then" | "else")
                    || keyword.text() == ")"
                        && keyword.parent().is_some_and(|p| {
                            p.field_name() == Some("condition") && p.parent() == Some(owner)
                        })
            }
            (SyntaxKind::IndentedBlock, SyntaxKind::TryExpression, Some("body")) => {
                keyword.text() == "try"
            }
            (SyntaxKind::IndentedBlock, SyntaxKind::FinallyClause, _) => {
                keyword.text() == "finally"
            }
            (SyntaxKind::IndentedCases, SyntaxKind::CatchClause, _) => keyword.text() == "catch",
            _ => false,
        };
        if !expected || !self.free_at(node.text_range().start()) {
            return None;
        }
        let mut statements = node.non_trivia_children();
        let (only, None) = (statements.next()?, statements.next()) else {
            return None;
        };
        let name = only.kind().name();
        let first = first_code_token(&only)?;
        // An `if`'s condition on one line, as the `if`'s layout is decided
        // with the body on its own; and not in parentheses, whose list is.
        // Asked as the condition will print, not as it's written, so that a
        // chain that breaks to fit decides as it would on one line.
        let up_to_keyword: Vec<_> = owner
            .non_trivia_children()
            .take_while(|c| c.text_range().start() <= keyword.text_range().start())
            .collect();
        let split_condition = owner.kind() == SyntaxKind::IfExpression
            && keyword.text() != "else"
            && !self.items_on_one_line(&up_to_keyword);
        let in_parentheses = owner.ancestors().any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::Arguments
                    | SyntaxKind::ParenthesizedExpression
                    | SyntaxKind::TupleExpression
            )
        });
        // An `if` inside could take an `else` after it.
        let has_if = only.as_node().is_some_and(|n| {
            n.descendants()
                .any(|d| d.kind() == SyntaxKind::IfExpression)
        });
        // A `case` with nothing after its arrow would take what follows,
        // and tree-sitter-scala misreads one that has a `;`.
        let empty_case = only.kind() == SyntaxKind::CaseClause
            && only.as_node().is_none_or(|c| {
                c.child_by_field_name("body").is_none() || c.tokens().any(|t| t.text() == ";")
            });
        if split_condition
            || in_parentheses
            || empty_case
            || has_if
            || name.ends_with("_definition")
            || name.ends_with("_declaration")
            || only.kind() == SyntaxKind::ImportDeclaration
            || !only.kind().is_named()
            || keyword.trailing_trivia().any(|t| t.is_comment())
            || first.leading_trivia().any(|t| t.is_comment())
            || last_code_token(&only).is_none_or(|t| t.trailing_trivia().any(|c| c.is_comment()))
            || !self.prints_on_one_line(&only)
        {
            return None;
        }
        Some(first.text_range().start())
    }

    fn node_body(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        if self.rules == Rules::Structural {
            match self.definition_body(node) {
                Some(DefinitionBody::SameLine {
                    equals,
                    body,
                    definition,
                    deeper,
                }) => {
                    self.trailing_skipped.push(equals.text_range().start());
                    self.moving.push((body, definition, deeper));
                }
                Some(DefinitionBody::NextLine(statement, back)) => {
                    self.joined.push((statement, back));
                }
                None => {}
            }
        }
        // A block the braced rule lays out is still a container, for the
        // lines inside it that the layout places.
        if self.rules == Rules::Structural
            && node.kind() == SyntaxKind::Block
            && let Some(doc) = self.braced(node)
        {
            return doc;
        }
        if let Some(nested) = self.layout.container(node) {
            return self.container(node, nested);
        }
        // Alignment is layout, like indentation, so the fallback has it too.
        if is_case_list(node)
            && let Some(doc) = self.aligned(&node.non_trivia_children().collect::<Vec<_>>())
        {
            return doc;
        }
        if self.rules == Rules::Structural
            && let Some(doc) = self.structural(node)
        {
            return doc;
        }
        self.fallback(node)
    }

    /// Where a definition's body goes, if it's a call, as `f(x)` or
    /// `Def.task { ... }`, or a control structure, as a `match`, `if`,
    /// `try`, `for`, `while` or `do`-`while`, with statements, enumerators
    /// or `case` clauses on lines of their own: on the `=`'s line, unless it
    /// would start too far past the line's indentation, or what must fit
    /// there wouldn't, or it's a control structure other than a `match`,
    /// and then on the next line, a level deeper. See
    /// [`Formatter::body_start`]. The printer decides, whichever line the
    /// body is written on. Not a body with a comment before it or a `;`
    /// after it, which tree-sitter-scala would take into the body on its
    /// own line, nor where the layout doesn't place its lines.
    fn definition_body(&self, node: SyntaxNode<'t>) -> Option<DefinitionBody<'t>> {
        let field = match node.kind() {
            SyntaxKind::FunctionDefinition | SyntaxKind::GivenDefinition => "body",
            SyntaxKind::ValDefinition | SyntaxKind::VarDefinition => "value",
            _ => return None,
        };
        let body = node.child_by_field_name(field)?.into_node()?;
        // Written on the next line, the body is mostly a block of one
        // statement, a level deeper; a `do`-`while` isn't.
        let wrapped = body.kind() == SyntaxKind::IndentedBlock;
        let control = if wrapped {
            let mut statements = body
                .non_trivia_children()
                .filter(|c| c.kind() != SyntaxKind::EndMarker);
            match (statements.next(), statements.next()) {
                (Some(NodeOrToken::Node(only)), None) => only,
                _ => return None,
            }
        } else {
            body
        };
        if !matches!(
            control.kind(),
            SyntaxKind::MatchExpression
                | SyntaxKind::IfExpression
                | SyntaxKind::TryExpression
                | SyntaxKind::ForExpression
                | SyntaxKind::WhileExpression
                | SyntaxKind::DoWhileExpression
                | SyntaxKind::CallExpression
        ) {
            return None;
        }
        let first = first_code_token(&NodeOrToken::Node(control))?;
        let next_line = first.is_first_on_line();
        let equals = std::iter::successors(first.prev_token(), SyntaxToken::prev_token)
            .find(|t| !t.is_trivia())?;
        let gap = TextRange::new(equals.text_range().end(), first.text_range().start());
        // The definition's first line, which places it.
        let start = first_code_token(&NodeOrToken::Node(node))?;
        if equals.text() != "="
            || equals.is_first_on_line()
            || wrapped && !next_line
            || !start.is_first_on_line()
            || self.has_comment(gap)
            || followed_by_semicolon(control)
            || self.rule_between(&equals, &first) != Some(true)
            || control.kind() != SyntaxKind::CallExpression && !self.has_statement_lines(control)
        {
            return None;
        }
        if next_line {
            // Its lines are placed from the definition's, wherever it starts.
            self.layout.offset(first.text_range().start())?;
            // Joined, its lines at its own width, as `else` or `catch`, would
            // end up at the definition's.
            if !self.lines_deeper(control) {
                return None;
            }
            let back = if wrapped { -1 } else { 0 };
            return Some(DefinitionBody::NextLine(control, back));
        }
        // Where the body's line goes, past where the definition's lines are
        // placed from: inside the definition's own frame, its first line.
        let definition = if self.layout.frames_after_leading(node) {
            0
        } else {
            self.layout.offset(start.text_range().start())?
        };
        // How much deeper than the definition's first line the line the
        // body starts on is: the lines inside the body keep their place
        // from it.
        let line = self.placing_line(first)?;
        self.layout.offset(line)?;
        let deeper = self.indentation_at(line).len() as isize
            - self.indentation_at(start.text_range().start()).len() as isize;
        // From a line deeper than the definition's, lines inside the body
        // written shallower than it would move past the definition.
        if deeper != 0 && !self.lines_deeper(body) {
            return None;
        }
        // tree-sitter-scala takes a statement after a comment on its line,
        // as `/* final */ def g`, into a body on a line of its own.
        if self.next_line_starts_with_comment(node) {
            return None;
        }
        Some(DefinitionBody::SameLine {
            equals,
            body,
            definition,
            deeper,
        })
    }

    /// Columns past its line's indentation, at most, where a definition's
    /// body starts on the `=`'s line, and what of it must fit there: for a
    /// `match`, its first line; for a call, its head, as `f(`, with no
    /// limit for one on one line, but for one of plain arguments, all of
    /// it, on that line or the next, before its head. Other control
    /// structures always start the next line.
    fn body_start(&self, body: SyntaxNode<'t>) -> (usize, FirstLine) {
        match body.kind() {
            SyntaxKind::MatchExpression => (self.style.max_body_start, FirstLine::Flat),
            SyntaxKind::CallExpression if self.has_statement_lines(body) => {
                (self.style.max_body_start, FirstLine::ToBreak)
            }
            SyntaxKind::CallExpression if takes_lambda(body) => (usize::MAX, FirstLine::ToBreak),
            SyntaxKind::CallExpression => (usize::MAX, FirstLine::Whole),
            _ => (0, FirstLine::Flat),
        }
    }

    /// Whether each line of `node` after its first starts deeper than the
    /// first, or with a `}`, or is placed by a rule: the list rule, which
    /// puts a list's lines from its `(`'s line and its `)` at that line's
    /// width, or the rules for select and boolean chains.
    fn lines_deeper(&self, node: SyntaxNode<'t>) -> bool {
        let width = self.indentation_at(node.text_range().start()).len();
        node.tokens()
            .filter(|t| !t.is_trivia() && !t.is_missing() && t.is_first_on_line())
            .skip(1)
            .all(|t| {
                let at = t.text_range().start();
                t.text() == "}"
                    || self.indentation_at(at).len() > width
                    || t.parent_ancestors()
                        .filter_map(|list| self.list_parts(list))
                        .any(|parts| parts.open.text_range().start() < at)
                    || t.parent_ancestors().any(|chain| {
                        chain.text_range().start() < at
                            && (self.chain_parts(chain).is_some()
                                || self.infix_parts(chain).is_some())
                    })
            })
    }

    /// Whether the code after `node` is on a later line, after a comment
    /// on that line, as `/* final */ def g`.
    fn next_line_starts_with_comment(&self, node: SyntaxNode<'t>) -> bool {
        let Some(last) = node.last_code_token() else {
            return false;
        };
        let next = std::iter::successors(last.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia() && !t.is_missing());
        next.is_some_and(|next| {
            let (end, at) = (last.text_range().end(), next.text_range().start());
            let before = &self.text[self.line_start(at)..usize::from(at)];
            self.has_line_break(TextRange::new(end, at)) && !before.trim().is_empty()
        })
    }

    /// The first token of the line that places `token`'s line: its own,
    /// or for a line inside a list the list rule lays out, through its `)`
    /// or `]`, the line of its `(` or `[`, which the list's lines follow.
    fn placing_line(&self, token: SyntaxToken<'t>) -> Option<TextSize> {
        let mut token = token;
        loop {
            let first = std::iter::successors(Some(token), SyntaxToken::prev_token)
                .filter(|t| !t.is_trivia() && !t.is_missing())
                .find(SyntaxToken::is_first_on_line)?;
            let at = first.text_range().start();
            let open = first
                .parent_ancestors()
                .filter_map(|list| self.list_parts(list))
                .map(|parts| parts.open)
                .find(|open| open.text_range().start() < at);
            match open {
                Some(open) => token = open,
                None => return Some(at),
            }
        }
    }

    /// Whether `node` holds a block, region or `case` clauses that print
    /// over several lines whatever the width: two statements or more, or
    /// `case` clauses on lines of their own. A block of one statement may
    /// join onto one line, or break to fit.
    fn has_statement_lines(&self, node: SyntaxNode<'t>) -> bool {
        self.has_line_break(node.text_range())
            && node.descendants().any(|d| {
                let statements = || d.non_trivia_children().filter(|c| c.kind().is_named());
                let holds = match d.kind() {
                    // Asked of the `match`, since its first `case` may
                    // start the line after it.
                    SyntaxKind::IndentedCases | SyntaxKind::CaseBlock => {
                        return d.parent().is_some_and(|m| {
                            m.kind() == SyntaxKind::MatchExpression
                                && !self.prints_on_one_line(&NodeOrToken::Node(m))
                        });
                    }
                    // Also a lambda's body in braces, which is no region.
                    // And a `for`'s enumerators.
                    SyntaxKind::Block
                    | SyntaxKind::IndentedBlock
                    | SyntaxKind::TemplateBody
                    | SyntaxKind::Enumerators => statements().nth(1).is_some(),
                    _ => d.is_indentation_region() && statements().nth(1).is_some(),
                };
                holds && !self.prints_on_one_line(&NodeOrToken::Node(d))
            })
    }

    /// A container whose statements are placed from its frame: nested a
    /// level deeper than the frame's line, or at its width for `case`
    /// clauses written there. What's before them, such as the `{`, and the
    /// `}` after them stay at the frame's line; comments before the `}` go
    /// with the statements.
    fn container(&mut self, node: SyntaxNode<'t>, nested: indent::Nested) -> Doc<'t> {
        let children: Vec<_> = node.non_trivia_children().collect();
        let starts = children
            .iter()
            .position(|c| c.text_range().start() >= nested.inner.start())
            .unwrap_or(children.len());
        let ends = children
            .iter()
            .position(|c| c.text_range().start() >= nested.inner.end())
            .unwrap_or(children.len())
            .max(starts);
        let (before, statements, after) = (
            &children[..starts],
            &children[starts..ends],
            &children[ends..],
        );

        // A match type's frame starts at its `{`.
        let frame_at = self
            .layout
            .frames_at(node)
            .first()
            .and_then(|&(at, _)| before.iter().position(|c| c.text_range().start() == at));
        let mut outside = Vec::new();
        let mut framed = Vec::new();
        for (i, child) in before.iter().enumerate() {
            if Some(i) == frame_at {
                outside.push(self.leading_of_element(child));
            }
            let doc = self.element(*child);
            if frame_at.is_some_and(|at| i >= at) {
                framed.push(doc);
            } else {
                outside.push(doc);
            }
        }

        let mut inner = match is_case_list(node)
            .then(|| self.aligned(statements))
            .flatten()
        {
            Some(doc) => doc,
            None => Doc::concat(statements.iter().map(|c| self.element(*c))),
        };
        // The `}`'s leading trivia through its last comment: comments after
        // the last statement. The line breaks after them all go with the
        // `}`, so that dropping blank lines keeps the one before it.
        let mut close = Vec::new();
        if let Some(NodeOrToken::Token(token)) = after.first() {
            let leading: Vec<_> = token.leading_trivia().collect();
            let split = leading
                .iter()
                .rposition(|t| t.is_comment())
                .map_or(0, |i| i + 1);
            let mut docs = Vec::new();
            self.trivia_within_lines(&mut docs, &leading[..split], Gap::LineStart, Some(token));
            inner = inner + Doc::concat(docs);
            self.trivia_within_lines(&mut close, &leading[split..], Gap::LineStart, Some(token));
            self.leading_done = Some(token.text_range().start());
        }
        if nested.deeper {
            inner = inner.nest();
        }
        framed.push(inner);
        framed.extend(close);
        framed.extend(after.iter().map(|c| self.element(*c)));
        let framed = Doc::concat(framed);
        Doc::concat(outside)
            + if frame_at.is_some() {
                framed.from_line_indent()
            } else {
                framed
            }
    }

    /// The leading trivia of an element's first token, printed now.
    fn leading_of_element(&mut self, element: &SyntaxElement<'t>) -> Doc<'t> {
        match element {
            NodeOrToken::Node(node) => self.leading_of(*node),
            // A rule around this one, such as an end marker's, already
            // printed it.
            NodeOrToken::Token(token) if self.leading_done == Some(token.text_range().start()) => {
                Doc::Nil
            }
            NodeOrToken::Token(token) => {
                let doc = self.leading(*token);
                self.leading_done = Some(token.text_range().start());
                doc
            }
        }
    }

    /// The node's own rule, if its kind has one and it applies here.
    fn structural(&mut self, node: SyntaxNode<'t>) -> Option<Doc<'t>> {
        match node.kind() {
            // sbt's style: arguments indent 2 spaces and parameters 4, as
            // in scalafmt's `indent.callSite` and `indent.defnSite`.
            SyntaxKind::Arguments => self.list(node, 1),
            SyntaxKind::Parameters | SyntaxKind::ClassParameters => self.list(node, 2),
            // Type parameters and arguments follow the same layout.
            SyntaxKind::TypeArguments => self.list(node, 1),
            SyntaxKind::TypeParameters => self.list(node, 2),
            SyntaxKind::InfixExpression => self.infix(node),
            SyntaxKind::ParenthesizedExpression => self.chain_parens(node),
            SyntaxKind::CallExpression
            | SyntaxKind::FieldExpression
            | SyntaxKind::GenericFunction => self.select_chain(node),
            _ => None,
        }
    }

    /// A list in parentheses or brackets, such as arguments, parameters, or
    /// type arguments and parameters: on one line if it fits, otherwise one
    /// item per line, nested `levels` deep, with a trailing comma after
    /// several items and `)` or `]` on its own line (scalafmt's
    /// `danglingParentheses`).
    fn list(&mut self, node: SyntaxNode<'t>, levels: usize) -> Option<Doc<'t>> {
        let ListParts {
            open,
            close,
            items,
            commas,
        } = self.list_parts(node)?;
        let (open, close) = (&open, &close);

        // A lambda alone in the arguments keeps its head with the `(` and
        // breaks before its body instead, as in sbt's code: `f(x =>`, the
        // body, then `)`.
        let lambda = match items.as_slice() {
            [item] if levels == 1 => match item.as_slice() {
                [NodeOrToken::Node(node)] if node.kind() == SyntaxKind::LambdaExpression => {
                    self.lambda_parts(*node)
                }
                _ => None,
            },
            _ => None,
        };
        if let Some((head, arrow, body)) = lambda {
            let open = self.token_without_trailing(*open);
            self.skip_leading(&head[0]);
            let mut docs: Vec<_> = head.into_iter().map(|e| self.element(e)).collect();
            docs.push(self.token_without_trailing(arrow));
            self.skip_leading(&body);
            let body = (Doc::line() + self.element(body)).nest();
            self.skip_leading(&NodeOrToken::Token(*close));
            let close_text = self.token_without_trailing(*close);
            let close_trailing = self.trailing(*close);
            return Some(
                open + (Doc::concat(docs) + body + Doc::softline() + close_text)
                    .group()
                    .from_line_indent()
                    + close_trailing,
            );
        }

        // Named arguments and assignments, as `f(a = 1)` and sbt's
        // `.settings(name := "x", deps += d)`, go one per line however
        // short.
        let one_per_line = node.kind() == SyntaxKind::Arguments
            && items.iter().any(|item| match item.as_slice() {
                [NodeOrToken::Node(n)] => is_named_argument(*n),
                _ => false,
            });
        let open = self.token_without_trailing(*open);
        let mut inner = vec![Doc::softline()];
        let mut commas = commas.into_iter();
        let several = items.len() > 1;
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                let comma = commas.next().unwrap();
                self.skip_leading(&NodeOrToken::Token(comma));
                inner.push(self.token_without_trailing(comma));
                inner.push(Doc::line());
            }
            self.skip_leading(&item[0]);
            for element in item {
                inner.push(self.element(element));
            }
        }
        // A trailing comma when broken, whether or not the source has one,
        // unless there's only one item.
        if several {
            inner.push(Doc::if_break(Doc::text(","), Doc::Nil));
        }
        if one_per_line {
            inner.push(Doc::break_parent());
        }
        let mut inner = Doc::concat(inner);
        for _ in 0..levels {
            inner = inner.nest();
        }
        self.skip_leading(&NodeOrToken::Token(*close));
        let close_text = self.token_without_trailing(*close);
        let close_trailing = self.trailing(*close);
        let group = inner + Doc::softline() + close_text;
        // Type arguments are a poor place to break: a list before them on
        // the line breaks first, as the parameters before a result type
        // `(F[A], F[B])`.
        let group = match node.kind() {
            SyntaxKind::TypeArguments => group.reluctant_group(),
            _ => group.group(),
        };
        Some(open + group.from_line_indent() + close_trailing)
    }

    /// A block in braces holding one expression, such as `g { f(x) }` or
    /// `xs.map { x => f(x) }`: on one line if it fits, otherwise the
    /// expression goes on the next line indented 2 spaces and `}` on its own
    /// line, as in sbt's code. A lambda keeps `{ x =>` on the first line.
    /// An argument is laid out from its parts, however it was written.
    fn braced(&mut self, node: SyntaxNode<'t>) -> Option<Doc<'t>> {
        let BracedParts {
            open,
            inner,
            close,
            last,
            lambda,
        } = self.braced_parts(node)?;
        let leading = self.leading_of(node);
        let open = self.token_without_trailing(open);
        let mut docs = Vec::new();
        let body = match lambda {
            Some((head, arrow, body)) => {
                docs.push(Doc::text(" "));
                self.skip_leading(&head[0]);
                docs.extend(head.into_iter().map(|e| self.element(e)));
                docs.push(self.token_without_trailing(arrow));
                body
            }
            None => inner,
        };
        self.trailing_skipped.push(last.text_range().start());
        self.skip_leading(&body);
        let body = (Doc::line() + self.element(body)).nest();
        self.skip_leading(&NodeOrToken::Token(close));
        let close_text = self.token_without_trailing(close);
        let close_trailing = self.trailing(close);
        Some(
            leading
                + open
                + (Doc::concat(docs) + body + Doc::line() + close_text)
                    .group()
                    .from_line_indent()
                + close_trailing,
        )
    }

    /// The parts of a block the [`Self::braced`] rule lays out, or `None` if
    /// it keeps the block as written: it holds a definition or more than one
    /// expression, is written over several lines and isn't an argument, has
    /// comments inside, a part that doesn't print on one line, or a gap
    /// inside the braces where the spacing rules don't decide
    /// the space, so the block printed flat wouldn't match the fallback.
    fn braced_parts(&self, node: SyntaxNode<'t>) -> Option<BracedParts<'t>> {
        if node.kind() != SyntaxKind::Block || !self.can_relayout(node) {
            return None;
        }
        let children: Vec<_> = node.non_trivia_children().collect();
        let [NodeOrToken::Token(open), inner, NodeOrToken::Token(close)] = children.as_slice()
        else {
            return None;
        };
        let name = inner.kind().name();
        // Written over several lines, only an argument, as `g {` or
        // `xs.map { x =>`, is laid out: a body, as a `def`'s or an `if`'s,
        // keeps its breaks. And a `{` starting a line stays there: joined to
        // the line before, it can make the block an argument of what's on it.
        let broken = self.has_line_break(node.text_range());
        if open.text() != "{"
            || close.text() != "}"
            || broken && (open.is_first_on_line() || node.field_name() != Some("arguments"))
            || name.ends_with("_definition")
            || name.ends_with("_declaration")
        {
            return None;
        }
        // The spaces inside the braces must be ones the spacing rules put
        // there, so the block printed flat matches the fallback.
        let first = std::iter::successors(open.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia())?;
        let last = std::iter::successors(close.prev_token(), SyntaxToken::prev_token)
            .find(|t| !t.is_trivia())?;
        if self.rule_between(open, &first) != Some(true)
            || self.rule_between(&last, close) != Some(true)
        {
            return None;
        }
        let lambda = match inner {
            NodeOrToken::Node(n) if n.kind() == SyntaxKind::LambdaExpression => {
                let (head, arrow, body) = self.lambda_parts(*n)?;
                // A head on the line after `{` stays there: the fewer braces
                // rewrite leaves such a lambda alone, but not once joined.
                if first_code_token(&head[0])?.is_first_on_line() {
                    return None;
                }
                // Not statements after the arrow, as `a(); b`, which
                // tree-sitter-scala reads differently on one line, nor a
                // definition or an import alone.
                let only = match body {
                    NodeOrToken::Node(n) if n.kind() == SyntaxKind::IndentedBlock => {
                        let mut statements = n.non_trivia_children();
                        match (statements.next(), statements.next()) {
                            (Some(only), None) => only,
                            _ => return None,
                        }
                    }
                    _ => body,
                };
                let name = only.kind().name();
                if name.ends_with("_definition") || name.ends_with("_declaration") {
                    return None;
                }
                // The head and its arrow on one line, as the rule prints them.
                let mut line = head.clone();
                line.push(NodeOrToken::Token(arrow));
                if !self.items_on_one_line(&line) || !self.prints_on_one_line(&body) {
                    return None;
                }
                Some((head, arrow, body))
            }
            _ => {
                if !self.prints_on_one_line(inner) {
                    return None;
                }
                None
            }
        };
        Some(BracedParts {
            open: *open,
            inner: *inner,
            close: *close,
            last,
            lambda,
        })
    }

    /// A select chain with at least two calls, such as `xs.map(f).filter(g)`:
    /// on one line if it fits, otherwise broken before the first call's `.`
    /// and every `.` after it, each indented 2 spaces, as scalafmt's classic
    /// select chains in sbt's code. Field selects before the first call stay
    /// with the head: `sys.env` in `sys.env.get(k).map(f)`. A colon argument
    /// with more of the chain after it always breaks it, so what follows
    /// lines up with its call: `.map: x =>`, the body, then `.filter(p)`.
    /// When that's the chain's one call, it stays on the head's line, with
    /// its body two levels in and the rest one: `Def.task:`, the body, then
    /// `.taskValue`.
    fn select_chain(&mut self, node: SyntaxNode<'t>) -> Option<Doc<'t>> {
        let ChainParts {
            first_line,
            segments,
            colon_call,
            broken,
        } = self.chain_parts(node)?;
        let leading = self.leading_of(node);
        // Line breaks before a `.` go: the rule puts its own there.
        let mut line = Vec::new();
        for element in first_line {
            if element.kind() == T!["."] {
                self.skip_leading(&element);
            }
            line.push(self.element(element));
        }
        // Frames on the chain's nodes, such as a colon argument's from its
        // `:`, which the fallback would place.
        let mut frames = Vec::new();
        let mut current = NodeOrToken::Node(node);
        while let NodeOrToken::Node(n) = current
            && let Some(spine) = chain_spine(n)
        {
            frames.extend(self.layout.frames_at(n));
            current = spine;
        }
        // A colon call on the head's line nests its body one level deeper
        // than the parts after it, so they don't continue its last line.
        // A lambda's argument would place its body from its own line.
        if let Some(dot) = colon_call.first() {
            self.skip_leading(dot);
            self.unframed = colon_call
                .iter()
                .find(|e| e.kind() == SyntaxKind::ColonArgument)
                .and_then(|e| e.as_node().copied());
            let call = Doc::concat(colon_call.into_iter().map(|e| self.element(e)));
            self.unframed = None;
            line.push(call.nest().from_line_indent());
        }
        let mut rest = Vec::new();
        for segment in segments {
            rest.push(if broken {
                Doc::hardline()
            } else {
                Doc::softline()
            });
            self.skip_leading(&segment[0]);
            rest.extend(self.framed(&segment, &frames));
        }
        Some(
            leading
                + (Doc::concat(line) + Doc::concat(rest).nest())
                    .group()
                    .from_line_indent(),
        )
    }

    /// The parts of a chain the [`Self::select_chain`] rule lays out, or
    /// `None` if it keeps the chain as written: it's part of a longer chain,
    /// has fewer than two calls or comments inside, a part that doesn't
    /// print on one line, or a line break anywhere but before a `.`. A
    /// colon argument before the last part is the one part allowed over
    /// several lines.
    fn chain_parts(&self, node: SyntaxNode<'t>) -> Option<ChainParts<'t>> {
        // Only the outermost node of a chain; the ones inside are its parts.
        if let Some(parent) = node.parent()
            && chain_spine(parent).is_some_and(|spine| spine == NodeOrToken::Node(node))
        {
            return None;
        }
        chain_spine(node)?;
        if !self.can_relayout(node) {
            return None;
        }
        // Walk down the spine, from the last select to the head. `pending`
        // holds what follows the current part: arguments and type arguments.
        let mut segments: Vec<(Vec<SyntaxElement<'t>>, bool)> = Vec::new();
        let mut pending: Vec<SyntaxElement<'t>> = Vec::new();
        let mut applied = false;
        let mut current = NodeOrToken::Node(node);
        let head = loop {
            let NodeOrToken::Node(n) = current else {
                break current;
            };
            let Some(spine) = chain_spine(n) else {
                break current;
            };
            let extras: Vec<_> = n.non_trivia_children().filter(|c| *c != spine).collect();
            if n.kind() == SyntaxKind::FieldExpression {
                let mut segment = extras;
                segment.extend(pending.drain(..).rev());
                segments.push((segment, applied));
                applied = false;
            } else {
                applied |= n.kind() == SyntaxKind::CallExpression;
                pending.extend(extras.into_iter().rev());
            }
            current = spine;
        };
        segments.reverse();
        let calls = segments.iter().filter(|(_, applied)| *applied).count();
        let first_call = segments.iter().position(|(_, applied)| *applied)?;
        // A colon argument before the last part breaks the chain, so one
        // call is enough then.
        let colon_before_last = segments.split_last().is_some_and(|(_, before)| {
            before
                .iter()
                .flat_map(|(segment, _)| segment)
                .any(|e| e.kind() == SyntaxKind::ColonArgument)
        });
        if calls < 2 && !colon_before_last {
            return None;
        }
        let mut first_line = vec![head];
        first_line.extend(pending.into_iter().rev());
        let mut segments = segments.into_iter().map(|(segment, _)| segment);
        for segment in segments.by_ref().take(first_call) {
            first_line.extend(segment);
        }
        // With one call, taking a colon argument, the call stays on the
        // head's line, as one call does outside a chain.
        let colon_call = if calls < 2 {
            segments.next()?
        } else {
            Vec::new()
        };
        let segments: Vec<_> = segments.collect();

        // A colon argument, with its `:`, before the last part.
        let colon = |e: &SyntaxElement<'t>| {
            e.kind() == SyntaxKind::ColonArgument
                || e.kind() == T![":"]
                    && e.parent()
                        .is_some_and(|p| p.kind() == SyntaxKind::CallExpression)
        };
        let (last, before_last) = segments.split_last()?;
        let before_last: Vec<_> = colon_call
            .iter()
            .chain(before_last.iter().flatten())
            .collect();
        let broken = before_last
            .iter()
            .any(|e| e.kind() == SyntaxKind::ColonArgument);
        // Its body must be placed, or its lines would keep their widths
        // while the chain moves.
        let placed = |e: &SyntaxElement<'t>| {
            e.as_node().is_some_and(|argument| {
                argument
                    .children()
                    .filter(|c| {
                        matches!(
                            c.kind(),
                            SyntaxKind::IndentedBlock | SyntaxKind::IndentedCases
                        )
                    })
                    .all(|region| self.layout.container(region).is_some())
            })
        };
        if broken
            && !before_last
                .iter()
                .filter(|e| e.kind() == SyntaxKind::ColonArgument)
                .all(|e| placed(e))
        {
            return None;
        }
        let allowed = |e: &SyntaxElement<'t>| {
            broken && !last.contains(e) && !first_line.contains(e) && colon(e)
        };

        // Each part prints on one line, and the source breaks lines only
        // before a `.`, in a gap the spacing rules decide, so the chain
        // printed flat matches the fallback.
        let elements: Vec<_> = first_line
            .iter()
            .chain(&colon_call)
            .chain(segments.iter().flatten())
            .collect();
        if !elements
            .iter()
            .all(|e| allowed(e) || self.prints_on_one_line(e))
        {
            return None;
        }
        for pair in elements.windows(2) {
            let (Some(left), Some(right)) = (last_code_token(pair[0]), first_code_token(pair[1]))
            else {
                return None;
            };
            let gap = TextRange::new(left.text_range().end(), right.text_range().start());
            // Inside a colon argument, its own line breaks.
            let inside = allowed(pair[0]) && pair[1].kind() == SyntaxKind::ColonArgument;
            if self.text[gap].contains(['\n', '\r'])
                && !inside
                && (right.kind() != T!["."] || self.rule_between(&left, &right) != Some(false))
            {
                return None;
            }
        }
        Some(ChainParts {
            first_line,
            segments,
            colon_call,
            broken,
        })
    }

    /// A boolean chain, such as `a && b && c`: on one line if it fits,
    /// otherwise broken before each operator, with the operators starting
    /// the lines after the first, indented 2 spaces. Only the operators at
    /// the chain's own precedence break; an operand with operators that bind
    /// tighter, like `a == b` in `a == b && c`, is a chain of its own.
    ///
    /// A line may start with an infix operator only in Scala 3. In Scala 2,
    /// a broken chain is wrapped in parentheses, inside which line breaks
    /// don't end a statement, unless it's inside parentheses already, as an
    /// `if` condition or an argument is.
    ///
    /// The source's line breaks next to the operators don't matter: a chain
    /// written over several lines is laid out as if written on one.
    ///
    /// Only `&&` and `||` chains break. With sbt's configuration, scalafmt
    /// never adds breaks inside other infix chains, such as `"org" % "name"
    /// % "1.0"` or string concatenation, and leaves those lines long.
    fn infix(&mut self, node: SyntaxNode<'t>) -> Option<Doc<'t>> {
        let parts = self.infix_parts(node)?;
        // A condition without parentheses, as in `if c then`, is Scala 3's
        // syntax left as written, where a line may start with an operator.
        let scala3_condition = node.field_name() == Some("condition")
            && node.parent().is_some_and(|p| {
                matches!(
                    p.kind(),
                    SyntaxKind::IfExpression | SyntaxKind::WhileExpression
                )
            });
        let parens = !self.leading_infix && !scala3_condition && !inside_parentheses(node);
        let last = last_code_token(&NodeOrToken::Node(node))?;
        if parens && !self.gap_is_free(TextRange::empty(last.text_range().end())) {
            return None;
        }
        let leading = self.leading_of(node);
        if !parens {
            return Some(leading + self.chain_doc(parts).group().from_line_indent());
        }
        // The `)` goes before the last token's trailing trivia, such as a
        // comment.
        self.trailing_skipped.push(last.text_range().start());
        let chain = self.chain_doc(parts);
        // The skip was used up by the last operand: now it's printed.
        let trailing = self.trailing(last);
        Some(leading + parenthesized(chain).group().from_line_indent() + trailing)
    }

    /// Parentheses around a boolean chain where it needs none for its
    /// grouping, as in `val ok = (a && b)`: the chain's own, which Scala 2
    /// needs only when the chain is broken. They're printed only then, and
    /// never in Scala 3, so the result is the same with them or without.
    fn chain_parens(&mut self, node: SyntaxNode<'t>) -> Option<Doc<'t>> {
        let (chain, close) = self.chain_parens_parts(node)?;
        let parts = self.infix_parts(chain)?;
        let parens = !self.leading_infix && !inside_parentheses(node);
        let leading = self.leading_of(node);
        // `(` and `)` aren't printed, nor the gaps inside them.
        let last = last_code_token(&NodeOrToken::Node(chain))?;
        self.trailing_skipped.push(last.text_range().start());
        self.skip_leading(&NodeOrToken::Node(chain));
        let chain = self.chain_doc(parts);
        let trailing = self.trailing(close);
        let chain = if parens { parenthesized(chain) } else { chain };
        Some(leading + chain.group().from_line_indent() + trailing)
    }

    /// The chain inside parentheses [`Self::chain_parens`] leaves out, and
    /// the `)`.
    fn chain_parens_parts(
        &self,
        node: SyntaxNode<'t>,
    ) -> Option<(SyntaxNode<'t>, SyntaxToken<'t>)> {
        if node.kind() != SyntaxKind::ParenthesizedExpression
            || !needs_no_parentheses(node)
            || !self.can_relayout(node)
        {
            return None;
        }
        let children: Vec<_> = node.non_trivia_children().collect();
        let [
            NodeOrToken::Token(open),
            NodeOrToken::Node(chain),
            NodeOrToken::Token(close),
        ] = children.as_slice()
        else {
            return None;
        };
        if open.text() != "(" || close.text() != ")" || self.infix_parts(*chain).is_none() {
            return None;
        }
        let first = first_code_token(&NodeOrToken::Node(*chain))?;
        let last = last_code_token(&NodeOrToken::Node(*chain))?;
        let inside = TextRange::new(open.text_range().end(), first.text_range().start());
        let before_close = TextRange::new(last.text_range().end(), close.text_range().start());
        (self.gap_is_free(inside) && self.gap_is_free(before_close)).then_some((*chain, *close))
    }

    /// The operands with the operators before them, each starting a line
    /// when the chain breaks.
    fn chain_doc(&mut self, parts: InfixParts<'t>) -> Doc<'t> {
        let InfixParts {
            operands,
            operators,
            before_operators,
        } = parts;
        // The rule prints the gaps around each operator: a space, or a line
        // break before it.
        for prev in &before_operators {
            self.trailing_skipped.push(prev.text_range().start());
        }
        let mut operands = operands.into_iter();
        let first = operands.next().map_or(Doc::Nil, |o| self.element(o));
        let mut rest = Vec::new();
        for (op, operand) in operators.into_iter().zip(operands) {
            self.skip_leading(&NodeOrToken::Token(op));
            rest.push(Doc::line());
            rest.push(self.token_without_trailing(op));
            rest.push(Doc::text(" "));
            self.skip_leading(&operand);
            rest.push(self.element(operand));
        }
        first + Doc::concat(rest).nest()
    }

    /// The parts of a boolean chain the [`Self::infix`] rule lays out, or
    /// `None` if it keeps the chain as written: it has an operator other
    /// than `&&` and `||`, comments inside, an operand that doesn't print
    /// on one line, or a gap around an operator the spacing rules don't
    /// decide. Line breaks next to an operator go, wherever they are.
    fn infix_parts(&self, node: SyntaxNode<'t>) -> Option<InfixParts<'t>> {
        if node.kind() != SyntaxKind::InfixExpression
            || is_part_of_outer_chain(node)
            || !self.broken_chains && self.has_line_break(node.text_range())
            || !self.can_relayout(node)
        {
            return None;
        }
        let (mut operands, mut operators) = (Vec::new(), Vec::new());
        flatten_infix(node, &mut operands, &mut operators)?;
        let mut before_operators = Vec::new();
        for op in &operators {
            if !matches!(op.text(), "&&" | "||") {
                return None;
            }
            let prev = std::iter::successors(op.prev_token(), SyntaxToken::prev_token)
                .find(|t| !t.is_trivia())?;
            let next = std::iter::successors(op.next_token(), SyntaxToken::next_token)
                .find(|t| !t.is_trivia())?;
            // An operator starting a line keeps the gap after it as the
            // spacing rules see it; here it's the chain's own, so a space
            // written after it stays one.
            let after = TextRange::new(op.text_range().end(), next.text_range().start());
            let spaced_leading = op.is_first_on_line()
                && self.gap_is_free(after)
                && !self.text[after].is_empty()
                && self.text[after].chars().all(|c| c == ' ');
            if self.rule_between(&prev, op) != Some(true)
                || !spaced_leading && self.rule_between(op, &next) != Some(true)
            {
                return None;
            }
            before_operators.push(prev);
        }
        if !operands.iter().all(|o| self.prints_on_one_line(o)) {
            return None;
        }
        Some(InfixParts {
            operands,
            operators,
            before_operators,
        })
    }

    /// The parts of a list the [`Self::list`] rule lays out, or `None` if it
    /// keeps the list as written: it has comments, an item that doesn't
    /// print on one line, or a gap where the spacing rules don't decide the
    /// space, so the list printed flat wouldn't match the fallback.
    fn list_parts(&self, node: SyntaxNode<'t>) -> Option<ListParts<'t>> {
        if !matches!(
            node.kind(),
            SyntaxKind::Arguments
                | SyntaxKind::Parameters
                | SyntaxKind::ClassParameters
                | SyntaxKind::TypeArguments
                | SyntaxKind::TypeParameters
        ) || !self.can_relayout(node)
        {
            return None;
        }
        let children: Vec<_> = node.non_trivia_children().collect();
        let (NodeOrToken::Token(open), NodeOrToken::Token(close)) =
            (children.first()?, children.last()?)
        else {
            return None;
        };
        if !matches!((open.text(), close.text()), ("(", ")") | ("[", "]")) || children.len() < 3 {
            return None;
        }

        // The items between `(` and `)`, split at commas.
        let mut items = vec![Vec::new()];
        let mut commas = Vec::new();
        for child in &children[1..children.len() - 1] {
            match child {
                NodeOrToken::Token(t) if t.text() == "," => {
                    commas.push(*t);
                    items.push(Vec::new());
                }
                _ => items.last_mut().unwrap().push(*child),
            }
        }
        // A trailing comma, which needs `)` on the next line, leaves the last
        // item empty.
        let trailing_comma = items.len() > 1 && items.last().is_some_and(Vec::is_empty);
        if trailing_comma {
            items.pop();
            // One before `)` on the same line is an error the rule mustn't fix.
            if !close
                .leading_trivia()
                .any(|t| t.kind() == TriviaKind::Newline)
            {
                return None;
            }
        }
        if items.iter().any(Vec::is_empty) {
            return None;
        }
        // Line breaks between items go, but not ones inside an item.
        if !items.iter().all(|item| self.items_on_one_line(item)) {
            return None;
        }
        let first = std::iter::successors(open.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia())?;
        if self.rule_between(open, &first) != Some(false) {
            return None;
        }
        for (i, comma) in commas.iter().enumerate() {
            let prev = std::iter::successors(comma.prev_token(), SyntaxToken::prev_token)
                .find(|t| !t.is_trivia())?;
            if self.rule_between(&prev, comma) != Some(false) {
                return None;
            }
            if i + 1 == items.len() {
                break;
            }
            let next = std::iter::successors(comma.next_token(), SyntaxToken::next_token)
                .find(|t| !t.is_trivia())?;
            if self.rule_between(comma, &next) != Some(true) {
                return None;
            }
        }
        let last = std::iter::successors(close.prev_token(), SyntaxToken::prev_token)
            .find(|t| !t.is_trivia())?;
        // After a trailing comma, `)` goes on the next line anyway.
        let gap = TextRange::new(last.text_range().end(), close.text_range().start());
        if !trailing_comma && self.rule_between(&last, close) != Some(false)
            || !self.gap_is_free(gap)
        {
            return None;
        }
        // The source's trailing comma isn't printed: the rule adds its own.
        commas.truncate(items.len() - 1);
        Some(ListParts {
            open: *open,
            close: *close,
            items,
            commas,
        })
    }

    /// Whether the element prints on one line, unless a rule breaks it to
    /// fit: it's written on one line, or its line breaks are all between
    /// the items of lists that [`Self::list`] joins. Rules that decide on
    /// one-line code ask this rather than look at the source, so the first
    /// run decides as the second, on its output, would.
    fn prints_on_one_line(&self, element: &SyntaxElement<'t>) -> bool {
        if !self.has_line_break(element.text_range()) {
            return true;
        }
        let NodeOrToken::Node(node) = element else {
            return false;
        };
        if let Some(&known) = self.one_line.borrow().get(node) {
            return known;
        }
        let one_line = self.rules == Rules::Structural
            && (self.list_parts(*node).is_some()
                || self.chain_parts(*node).is_some()
                || self.infix_parts(*node).is_some()
                || self.chain_parens_parts(*node).is_some()
                || self.braced_parts(*node).is_some())
            || self.items_on_one_line(&node.non_trivia_children().collect::<Vec<_>>());
        self.one_line.borrow_mut().insert(*node, one_line);
        one_line
    }

    /// Whether the elements, in a row, print on one line: each does, and
    /// there are no line breaks between them.
    fn items_on_one_line(&self, elements: &[SyntaxElement<'t>]) -> bool {
        // Between tokens: a node's range can take in trivia after its last
        // token, as `enumerators` does the line break before `yield`.
        elements.windows(2).all(|pair| {
            // A body that joins its keyword's line if it fits.
            if pair[1]
                .as_node()
                .is_some_and(|n| self.joinable_body(*n).is_some())
            {
                return true;
            }
            let (Some(left), Some(right)) = (last_code_token(&pair[0]), first_code_token(&pair[1]))
            else {
                return false;
            };
            let gap = TextRange::new(left.text_range().end(), right.text_range().start());
            !self.text[gap].contains(['\n', '\r'])
        }) && elements.iter().all(|e| self.prints_on_one_line(e))
    }

    /// Whether a rule may decide where `node` has line breaks: it has no
    /// comments inside, and starts a line of code, since broken lines are
    /// indented from that line's indentation.
    fn can_relayout(&self, node: SyntaxNode<'t>) -> bool {
        let range = node.text_range();
        if !self.free_at(range.start()) {
            return false;
        }
        let line_start = self.text[..usize::from(range.start())]
            .rfind(['\n', '\r'])
            .map_or(0, |i| i + 1);
        if !TextSize::try_from(line_start).is_ok_and(|start| self.free_at(start)) {
            return false;
        }
        // Comments between its first token and its last.
        let element = NodeOrToken::Node(node);
        match (first_code_token(&element), last_code_token(&element)) {
            (Some(first), Some(last)) if first.text_range().end() < last.text_range().start() => {
                !self.has_comment(TextRange::new(
                    first.text_range().end(),
                    last.text_range().start(),
                ))
            }
            _ => true,
        }
    }

    /// A lambda split at its arrow: what comes before it, the arrow, and the
    /// body. `None` when the gap after the arrow isn't one the spacing rules
    /// put a space in, so the lambda printed flat matches the fallback.
    #[allow(clippy::type_complexity)]
    fn lambda_parts(
        &self,
        lambda: SyntaxNode<'t>,
    ) -> Option<(Vec<SyntaxElement<'t>>, SyntaxToken<'t>, SyntaxElement<'t>)> {
        let children: Vec<_> = lambda.non_trivia_children().collect();
        let arrow_at = children.iter().position(
            |c| matches!(c, NodeOrToken::Token(t) if matches!(t.text(), "=>" | "\u{21D2}")),
        )?;
        let NodeOrToken::Token(arrow) = children[arrow_at] else {
            return None;
        };
        let [body] = &children[arrow_at + 1..] else {
            return None;
        };
        let next = std::iter::successors(arrow.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia())?;
        if self.rule_between(&arrow, &next) != Some(true) {
            return None;
        }
        Some((children[..arrow_at].to_vec(), arrow, *body))
    }

    /// `case` clauses with their `=>` aligned, as scalafmt's default
    /// `align.preset = some`: each clause a [`Doc::Align`] row split at its
    /// `=>`, which continues the run of the clause before it unless a blank
    /// line or a comment comes between them. The printer decides the
    /// padding. `None` when no two clauses are next to each other.
    /// `children` are a clause list's, or its statements'.
    fn aligned(&mut self, children: &[SyntaxElement<'t>]) -> Option<Doc<'t>> {
        let alignable: Vec<bool> = children.iter().map(alignable_clause).collect();
        if !alignable.windows(2).any(|w| w[0] && w[1]) {
            return None;
        }
        let mut parts = Vec::new();
        for (child, alignable) in children.iter().zip(&alignable) {
            let row = match child {
                NodeOrToken::Node(n) if *alignable => self.clause_row(*n),
                _ => None,
            };
            parts.push(row.unwrap_or_else(|| AlignPart::Other(self.element(*child))));
        }
        Some(Doc::Align(parts))
    }

    /// A clause as a row split at its `=>`, or `None` if it has no arrow.
    fn clause_row(&mut self, clause: SyntaxNode<'t>) -> Option<AlignPart<'t>> {
        let children: Vec<_> = clause.non_trivia_children().collect();
        let arrow = children.iter().position(
            |c| matches!(c, NodeOrToken::Token(t) if matches!(t.text(), "=>" | "\u{21D2}")),
        )?;
        let first = first_code_token(&children[0])?;
        let adjacent = !first.leading_trivia().any(|t| t.is_comment())
            && first
                .leading_trivia()
                .filter(|t| t.kind() == TriviaKind::Newline)
                .count()
                == 1;
        let before = self.leading_of(clause);
        let head = Doc::concat(children[..arrow].iter().map(|c| self.element(*c)));
        let mut tail = Doc::concat(children[arrow..].iter().map(|c| self.element(*c)));
        // The clause's frame, for a body on later lines: from the line the
        // head is on, the same line the tail starts on.
        if self.layout.frames_after_leading(clause) {
            tail = tail.from_line_indent();
        }
        Some(AlignPart::Row {
            before,
            head,
            tail,
            adjacent,
        })
    }

    /// The default rule: the node's code children in order, each token with
    /// its comments and whitespace.
    fn fallback(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        let children: Vec<_> = node.non_trivia_children().collect();
        let frames = self.layout.frames_at(node);
        Doc::concat(self.framed(&children, &frames))
    }

    /// `children` in order, with frames from child tokens: each from a
    /// region's opener through the region, placing its lines from the line
    /// the opener is on.
    fn framed(
        &mut self,
        children: &[SyntaxElement<'t>],
        frames: &[(TextSize, TextSize)],
    ) -> Vec<Doc<'t>> {
        let mut docs = Vec::new();
        let mut i = 0;
        while i < children.len() {
            let child = children[i];
            let frame = frames
                .iter()
                .find(|(at, _)| child.text_range().start() == *at)
                .and_then(|(_, end)| {
                    let last = children[i..]
                        .iter()
                        .rposition(|c| c.text_range().start() < *end)?;
                    Some(i + last + 1)
                });
            match frame {
                Some(to) => {
                    docs.push(self.leading_of_element(&child));
                    let framed = Doc::concat(children[i..to].iter().map(|c| self.element(*c)));
                    docs.push(framed.from_line_indent());
                    i = to;
                }
                None => {
                    docs.push(self.element(child));
                    i += 1;
                }
            }
        }
        docs
    }

    fn element(&mut self, element: SyntaxElement<'t>) -> Doc<'t> {
        // The start of a marked statement's last block, before its leading
        // line break: where its end marker starts counting.
        let count_from = self.count_from.last().is_some_and(|&at| {
            first_code_token(&element).is_some_and(|t| t.text_range().start() == at)
        });
        if count_from {
            self.count_from.pop();
        }
        let doc = match element {
            NodeOrToken::Node(node) => self.node(node),
            NodeOrToken::Token(token) => self.token(token),
        };
        if count_from {
            Doc::count_from() + doc
        } else {
            doc
        }
    }

    fn token(&mut self, token: SyntaxToken<'t>) -> Doc<'t> {
        self.token_without_trailing(token) + self.trailing(token)
    }

    /// The token with its leading trivia.
    fn token_without_trailing(&mut self, token: SyntaxToken<'t>) -> Doc<'t> {
        let mut docs = Vec::new();
        if self.leading_done == Some(token.text_range().start()) {
            self.leading_done = None;
        } else {
            docs.push(self.leading(token));
        }
        docs.push(self.source(token.text_range(), Piece::Code));
        self.at_file_start = false;
        self.line_breaks = 0;
        Doc::concat(docs)
    }

    /// The token's leading trivia: line breaks, indentation and comments
    /// before it.
    fn leading(&mut self, token: SyntaxToken<'t>) -> Doc<'t> {
        let mut docs = Vec::new();
        let leading: Vec<_> = token.leading_trivia().collect();
        // A statement that needs blank lines above it opens a frame, which
        // closes after the statement: see [`Formatter::node`].
        let frame = blank_rules_above(&token).map(|(statement, rules)| {
            let id = u32::from(statement.text_range().start());
            self.blank_open.push((statement, id));
            self.blank_anchor_wanted = true;
            Doc::BlankLines {
                id,
                // Never more than the style allows in a row.
                always: rules.always.min(self.style.max_blank_lines_in_templates),
                when_long: rules.when_long.min(self.style.max_blank_lines_in_templates),
                after_long: rules.after_long.min(self.style.max_blank_lines_in_templates),
                binding: rules.binding,
                min_breaks: self.style.long_statement_min_breaks,
            }
        });
        self.trivia_within_lines(&mut docs, &leading, Gap::LineStart, Some(&token));
        self.blank_anchor_wanted = false;
        let mut doc = Doc::concat(docs);
        if self.joining == Some(token.text_range().start()) {
            self.joining = None;
            doc = Doc::if_break(doc, Doc::text(" "));
        }
        match frame {
            Some(frame) => frame + doc + Doc::BlankStart,
            None => doc,
        }
    }

    /// Drops the leading trivia of the element's first token: the line
    /// breaks and indentation before it, which a rule replaces with its own.
    /// The trivia must have no comments.
    fn skip_leading(&mut self, element: &SyntaxElement<'t>) {
        let first = match element {
            NodeOrToken::Token(t) => Some(*t),
            NodeOrToken::Node(n) => std::iter::successors(n.first_token(), SyntaxToken::next_token)
                .find(|t| !t.is_trivia()),
        };
        if let Some(first) = first {
            self.leading_done = Some(first.text_range().start());
        }
    }

    /// The leading trivia of the node's first token, printed now so that a
    /// group around the node doesn't contain its line break.
    fn leading_of(&mut self, node: SyntaxNode<'t>) -> Doc<'t> {
        let Some(first) = std::iter::successors(node.first_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia())
        else {
            return Doc::Nil;
        };
        // A rule around this one already printed it.
        if self.leading_done == Some(first.text_range().start()) {
            return Doc::Nil;
        }
        let doc = self.leading(first);
        self.leading_done = Some(first.text_range().start());
        doc
    }

    /// Whether the spacing rules put a space between two tokens on the same
    /// line, or `None` if the gap is kept as written.
    fn rule_between(&self, left: &SyntaxToken<'t>, right: &SyntaxToken<'t>) -> Option<bool> {
        let gap = TextRange::new(left.text_range().end(), right.text_range().start());
        if self.gap_is_free(gap) {
            spacing::space_between(left, right)
        } else {
            None
        }
    }

    /// The token's trailing trivia: the gap up to the next token on its line,
    /// or the rest of the line.
    fn trailing(&mut self, token: SyntaxToken<'t>) -> Doc<'t> {
        let start = token.text_range().start();
        if let Some(i) = self.trailing_skipped.iter().position(|&s| s == start) {
            self.trailing_skipped.swap_remove(i);
            return Doc::Nil;
        }
        let mut docs = Vec::new();
        let trailing: Vec<_> = token.trailing_trivia().collect();
        let next = std::iter::successors(token.next_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia() && !t.is_missing());
        let next_on_this_line = next.filter(|b| b.leading_trivia().len() == 0);
        let only_spaces = trailing.iter().all(|t| t.kind() == TriviaKind::Whitespace);
        match next_on_this_line {
            Some(next) => {
                match self.rule_between(&token, &next) {
                    // Only spaces before the next code token: the rule decides.
                    Some(space) if only_spaces => {
                        if space {
                            docs.push(Doc::text(" "));
                        }
                    }
                    Some(_) => {
                        self.trivia_within_lines(&mut docs, &trailing, Gap::AfterCode, Some(&next));
                    }
                    None => {
                        for trivia in trailing {
                            docs.push(self.trivia(trivia));
                        }
                    }
                }
            }
            // The end of the line: one space before a comment. Spaces at the
            // very end go; the printer trims them anyway.
            None if only_spaces => {}
            None => self.trivia_within_lines(&mut docs, &trailing, Gap::AfterCode, None),
        }
        Doc::concat(docs)
    }

    /// Pushes `trivia`, which comes before `next`, with scalafmt's spacing
    /// around comments: one space on each side of a comment within a line,
    /// except none before `,`, `;` or `:`. Indentation and anything verbatim
    /// stay as written.
    fn trivia_within_lines(
        &mut self,
        docs: &mut Vec<Doc<'t>>,
        trivia: &[SyntaxTrivia<'t>],
        mut before: Gap,
        next: Option<&SyntaxToken<'t>>,
    ) {
        // Whether a line break before this point placed the line.
        let mut placed = false;
        // Whether the printer may add blank lines at the first line break.
        let mut anchor_wanted = std::mem::take(&mut self.blank_anchor_wanted);
        let mut anchor_here = false;
        for (i, &t) in trivia.iter().enumerate() {
            let start = t.text_range().start();
            match t.kind() {
                TriviaKind::Newline => {
                    if self.is_free(start) {
                        self.line_breaks += 1;
                        // A statement that needs blank lines above it gets
                        // them at the first line break, before any comment,
                        // where the printer adds them. A comment indented deeper than the statement ends
                        // the body above it, and the blank line is not for it.
                        let belongs_above = next.is_some_and(|n| {
                            trivia[i..]
                                .iter()
                                .find(|t| {
                                    matches!(
                                        t.kind(),
                                        TriviaKind::LineComment | TriviaKind::BlockComment
                                    )
                                })
                                .is_some_and(|c| {
                                    column_of(self.text, c.text_range().start())
                                        > column_of(self.text, n.text_range().start())
                                })
                        });
                        if anchor_wanted && self.line_breaks == 1 && !belongs_above {
                            anchor_wanted = false;
                            anchor_here = true;
                        }
                        // The code after the line breaks decides how many blank
                        // lines it may have before it. A comment between them
                        // belongs to that code. Asked only when it matters:
                        // finding out climbs the tree.
                        let style = self.style;
                        let fewest = style
                            .max_blank_lines_in_blocks
                            .min(style.max_blank_lines_in_templates);
                        let too_many = self.line_breaks > fewest + 1 && {
                            let blank_lines = match next {
                                Some(next) if !in_template(next) => style.max_blank_lines_in_blocks,
                                _ => style.max_blank_lines_in_templates,
                            };
                            // The line break ending a line, then the blank lines.
                            self.line_breaks > blank_lines + 1
                        };
                        if self.at_file_start || too_many {
                            continue;
                        }
                    }
                    // The line's indentation is the layout's, past blank
                    // lines to the line with code or a comment.
                    let line = trivia[i + 1..]
                        .iter()
                        .find(|t| !matches!(t.kind(), TriviaKind::Newline | TriviaKind::Whitespace))
                        .map(|t| t.text_range().start())
                        .or(next.map(|n| n.text_range().start()));
                    match line
                        .and_then(|l| self.layout.offset(l))
                        .filter(|_| self.is_free(start))
                    {
                        Some(offset) => {
                            docs.push(self.trivia(t).offset(offset));
                            placed = true;
                        }
                        None => {
                            docs.push(self.trivia(t));
                            placed = false;
                        }
                    }
                    before = Gap::LineStart;
                    if std::mem::take(&mut anchor_here) {
                        docs.push(Doc::BlankAnchor);
                    }
                }
                // A blank line's whitespace, which the printer would trim
                // anyway. Keeping it would make it the next line's
                // indentation when a line break between them is dropped.
                TriviaKind::Whitespace
                    if before == Gap::LineStart
                        && self.is_free(start)
                        && trivia
                            .get(i + 1)
                            .is_some_and(|t| t.kind() == TriviaKind::Newline) => {}
                TriviaKind::Whitespace if before == Gap::LineStart && self.is_free(start) => {
                    match self.layout.offset(t.text_range().end()) {
                        // The line break put it there.
                        Some(_) if placed => {}
                        // The first line of the file.
                        Some(offset) => {
                            docs.push(Doc::text(" ".repeat(offset.max(0) as usize)));
                        }
                        None => docs.push(self.trivia(t)),
                    }
                    before = Gap::Space;
                }
                TriviaKind::Whitespace if before == Gap::LineStart || !self.is_free(start) => {
                    docs.push(self.trivia(t));
                    before = Gap::Space;
                }
                // Within a line: the space, if any, depends on what's around it.
                TriviaKind::Whitespace => {}
                TriviaKind::LineComment | TriviaKind::BlockComment => {
                    let after_code = matches!(before, Gap::AfterCode | Gap::AfterComment);
                    let mut comment = Doc::Nil;
                    if after_code && self.free_at(start) {
                        comment = Doc::text(" ");
                    }
                    comment = comment + self.trivia(t);
                    self.at_file_start = false;
                    self.line_breaks = 0;
                    // A `//` comment after code takes no width, as in scalafmt,
                    // so a long one doesn't make the code on its line break.
                    if after_code && t.kind() == TriviaKind::LineComment && self.is_free(start) {
                        comment = comment.line_suffix();
                    }
                    docs.push(comment);
                    before = Gap::AfterComment;
                }
            }
        }
        if let Some(next) = next
            && before == Gap::AfterComment
            && !matches!(next.text(), "," | ";" | ":")
            && self.free_at(next.text_range().start())
        {
            docs.push(Doc::text(" "));
        }
    }

    /// Whether a space may be inserted at `offset`: it's not inside a
    /// verbatim range, though it may be at the start of one.
    fn free_at(&self, offset: TextSize) -> bool {
        self.gap_is_free(TextRange::new(offset, offset))
    }

    /// Whether the source at `offset` may be changed: it's not in a verbatim
    /// range.
    fn is_free(&self, offset: TextSize) -> bool {
        self.tree.verbatim_at(offset).is_none()
    }

    /// Whether no verbatim range overlaps `gap`. An empty gap right before a
    /// range, as in `f(` before `"x"`, is outside it.
    fn gap_is_free(&self, gap: TextRange) -> bool {
        self.tree
            .verbatim_at(gap.start())
            .is_none_or(|v| v.range.start() >= gap.end())
    }

    fn trivia(&mut self, trivia: SyntaxTrivia<'t>) -> Doc<'t> {
        let piece = match trivia.kind() {
            TriviaKind::Whitespace => Piece::Whitespace,
            TriviaKind::Newline => Piece::Newline,
            TriviaKind::LineComment | TriviaKind::BlockComment => Piece::Comment,
        };
        self.source(trivia.text_range(), piece)
    }

    /// The document for a stretch of source. Where it overlaps a verbatim
    /// range, the whole range is copied once, when first reached.
    fn source(&mut self, range: TextRange, piece: Piece) -> Doc<'t> {
        let (mut start, end) = (range.start(), range.end());
        let mut docs = Vec::new();
        while start < end {
            if start < self.copied_until {
                start = self.copied_until.min(end);
                continue;
            }
            while self
                .verbatim
                .get(self.next_verbatim)
                .is_some_and(|v| v.range.end() <= start)
            {
                self.next_verbatim += 1;
            }
            match self.verbatim.get(self.next_verbatim) {
                Some(v) if v.range.start() <= start => {
                    docs.push(self.verbatim_range(*v));
                    self.copied_until = v.range.end();
                    self.next_verbatim += 1;
                }
                Some(v) if v.range.start() < end => {
                    docs.push(self.plain(TextRange::new(start, v.range.start()), piece));
                    start = v.range.start();
                }
                _ => {
                    docs.push(self.plain(TextRange::new(start, end), piece));
                    start = end;
                }
            }
        }
        Doc::concat(docs)
    }

    /// A verbatim range, copied. A multi-line comment starting a line that
    /// moves moves all its lines by as much, so a Scaladoc's ` *` lines stay
    /// under its `/**`. A Scaladoc starting a line is in Scala 3's style
    /// first, as [`scaladoc`] writes it, and one with one line of text is
    /// on one line where it fits.
    fn verbatim_range(&self, v: VerbatimRange) -> Doc<'t> {
        let text = &self.text[v.range];
        let start = v.range.start();
        if v.reason != VerbatimReason::Comment {
            return Doc::verbatim(text);
        }
        let line_start = self.text[..usize::from(start)]
            .rfind('\n')
            .map_or(0, |i| i + 1);
        let before = &self.text[line_start..usize::from(start)];
        let column = before.chars().count();
        let lines = match before.bytes().all(|b| b == b' ') {
            true => scaladoc_lines(text),
            false => None,
        };
        let broken: std::borrow::Cow<'t, str> = match &lines {
            Some(lines) => scaladoc(lines, column).into(),
            None => text.into(),
        };
        let broken = match self.layout.offset(start) {
            None => Doc::verbatim(broken),
            Some(_) => Doc::Relative {
                text: broken,
                column,
            },
        };
        match lines.as_deref() {
            Some([only]) => Doc::if_break(broken, Doc::text(format!("/** {only} */"))).group(),
            _ => broken,
        }
    }

    fn plain(&self, range: TextRange, piece: Piece) -> Doc<'t> {
        let text = &self.text[range];
        match piece {
            Piece::Newline => Doc::hardline(),
            _ if text.contains(['\n', '\r']) => Doc::verbatim(text),
            _ => Doc::text(text),
        }
    }
}

/// A Scaladoc's text `lines`, as [`scaladoc_lines`] reads them, with a
/// gutter for `/**` at `column`, in Scala 3's style: the text starting on
/// `/**`'s line, each later line's `*` under the first `*` of `/**`, and two
/// spaces after it, so the text lines up with the first line's.
fn scaladoc(lines: &[&str], column: usize) -> String {
    let gutter = format!("\n{}*", " ".repeat(column + 1));
    let mut out = String::from("/**");
    let rest = match lines.split_first() {
        // A blank first line stays below `/**`.
        Some((first, rest)) if !first.is_empty() => {
            out.push(' ');
            out.push_str(first);
            rest
        }
        _ => lines,
    };
    for line in rest {
        out.push_str(&gutter);
        if !line.is_empty() {
            out.push_str("  ");
            out.push_str(line);
        }
    }
    out.push_str(&gutter);
    out.push('/');
    out
}

/// A block of one expression split for [`Formatter::braced`].
struct BracedParts<'t> {
    open: SyntaxToken<'t>,
    inner: SyntaxElement<'t>,
    close: SyntaxToken<'t>,
    /// The last token before `}`.
    last: SyntaxToken<'t>,
    /// The expression split at its arrow, when it's a lambda.
    #[allow(clippy::type_complexity)]
    lambda: Option<(Vec<SyntaxElement<'t>>, SyntaxToken<'t>, SyntaxElement<'t>)>,
}

/// A boolean chain split for [`Formatter::infix`].
struct InfixParts<'t> {
    operands: Vec<SyntaxElement<'t>>,
    operators: Vec<SyntaxToken<'t>>,
    /// The last token before each operator.
    before_operators: Vec<SyntaxToken<'t>>,
}

/// Whether the infix expression is part of the chain around it, as `a && b`
/// in `a && b && c`, which [`flatten_infix`] merges into it. Only the
/// outermost node of a chain is laid out: where it's kept as written, so
/// are its parts.
fn is_part_of_outer_chain(node: SyntaxNode<'_>) -> bool {
    fn operator(n: SyntaxNode<'_>) -> Option<SyntaxToken<'_>> {
        n.non_trivia_children()
            .find(|c| c.field_name() == Some("operator"))
            .and_then(|o| o.as_token().copied())
    }
    let Some(parent) = node
        .parent()
        .filter(|p| p.kind() == SyntaxKind::InfixExpression)
    else {
        return false;
    };
    let (Some(outer), Some(inner)) = (operator(parent), operator(node)) else {
        return false;
    };
    let right_assoc = outer.text().ends_with(':');
    precedence(outer.text()) == precedence(inner.text())
        && inner.text().ends_with(':') == right_assoc
        && node.field_name() == Some(if right_assoc { "right" } else { "left" })
}

/// A broken chain in parentheses; on one line, without them.
fn parenthesized(chain: Doc<'_>) -> Doc<'_> {
    Doc::if_break(Doc::text("("), Doc::Nil) + chain + Doc::if_break(Doc::text(")"), Doc::Nil)
}

/// Whether parentheses, `node`, are where what's inside them needs none
/// for its grouping: the whole of a definition's body, an assignment's
/// right side, a lambda's or case's body, an `if` branch, a returned
/// value, or a statement.
fn needs_no_parentheses(node: SyntaxNode<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let field = node.field_name();
    match parent.kind() {
        SyntaxKind::FunctionDefinition => field == Some("body"),
        SyntaxKind::ValDefinition | SyntaxKind::VarDefinition => field == Some("value"),
        SyntaxKind::AssignmentExpression => field == Some("right"),
        SyntaxKind::CaseClause => field == Some("body"),
        SyntaxKind::IfExpression => matches!(field, Some("consequence" | "alternative")),
        SyntaxKind::LambdaExpression | SyntaxKind::ReturnExpression => {
            parent.non_trivia_children().last() == Some(NodeOrToken::Node(node))
        }
        SyntaxKind::Block
        | SyntaxKind::TemplateBody
        | SyntaxKind::IndentedBlock
        | SyntaxKind::CompilationUnit => field.is_none(),
        _ => false,
    }
}

/// A chain split for [`Formatter::select_chain`].
struct ChainParts<'t> {
    /// The head, with what stays on its line: arguments right after it, and
    /// field selects before the first call.
    first_line: Vec<SyntaxElement<'t>>,
    /// The rest, from the first call's `.` on: each starts with its `.`.
    segments: Vec<Vec<SyntaxElement<'t>>>,
    /// The chain's one call when it takes a colon argument, which stays on
    /// the head's line: `Def.task:` in `Def.task: body .taskValue`.
    colon_call: Vec<SyntaxElement<'t>>,
    /// Whether the chain always breaks: a colon argument comes before its
    /// last part.
    broken: bool,
}

/// A list split for [`Formatter::list`].
struct ListParts<'t> {
    open: SyntaxToken<'t>,
    close: SyntaxToken<'t>,
    /// The items between the commas, each a run of elements.
    items: Vec<Vec<SyntaxElement<'t>>>,
    /// The commas between the items, without a trailing one.
    commas: Vec<SyntaxToken<'t>>,
}

/// Where a definition's body is written, for
/// [`Formatter::definition_body`].
enum DefinitionBody<'t> {
    /// On the `=`'s line.
    SameLine {
        equals: SyntaxToken<'t>,
        body: SyntaxNode<'t>,
        /// Where the body's line goes if it breaks, past where the
        /// definition's lines are placed from, a level less.
        definition: isize,
        /// How much deeper than the definition's first line the line the
        /// body starts on is.
        deeper: isize,
    },
    /// On the next line: the body, the block's one statement where it's
    /// wrapped in one, and the levels its lines come back by when it joins
    /// the `=`'s line: one out of the block.
    NextLine(SyntaxNode<'t>, isize),
}

/// Whether an argument is a named one, `a = 1`, or an assignment, as an
/// sbt setting, `key := value` or `deps ++= Seq(...)`: an operator ending
/// in `=` that isn't a comparison.
fn is_named_argument(node: SyntaxNode<'_>) -> bool {
    match node.kind() {
        SyntaxKind::AssignmentExpression => true,
        SyntaxKind::InfixExpression => node
            .child_by_field_name("operator")
            .is_some_and(|op| is_assignment_operator(op.text())),
        _ => false,
    }
}

/// Whether a call, or one before it in a chain, as `f(a)(b => b)` or
/// `xs.map { x => x }.toList`, takes a lambda or a block: `f(x => x)`,
/// `xs.map { x => x }` or `f: x => x`.
fn takes_lambda(call: SyntaxNode<'_>) -> bool {
    std::iter::successors(Some(call), |c| {
        let field = match c.kind() {
            SyntaxKind::CallExpression | SyntaxKind::GenericFunction => "function",
            SyntaxKind::FieldExpression => "value",
            _ => return None,
        };
        c.child_by_field_name(field)?.into_node()
    })
    .filter(|c| c.kind() == SyntaxKind::CallExpression)
    .any(|c| {
        c.child_by_field_name("arguments")
            .and_then(|a| a.into_node())
            .is_some_and(|args| match args.kind() {
                SyntaxKind::Arguments => args
                    .non_trivia_children()
                    .any(|a| a.kind() == SyntaxKind::LambdaExpression),
                _ => true,
            })
    })
}

/// An assignment operator, such as `+=` or `:=`, and not `==`, `<=`, `>=`
/// or `!=`.
fn is_assignment_operator(op: &str) -> bool {
    op.ends_with('=') && !op.starts_with('=') && !matches!(op, "<=" | ">=" | "!=")
}

/// Whether a `;` follows the node.
fn followed_by_semicolon(node: SyntaxNode<'_>) -> bool {
    node.last_token()
        .and_then(|t| {
            std::iter::successors(t.next_token(), SyntaxToken::next_token).find(|t| !t.is_trivia())
        })
        .is_some_and(|t| t.text() == ";")
}

/// Whether the node holds `case` clauses that may be aligned.
fn is_case_list(node: SyntaxNode<'_>) -> bool {
    matches!(
        node.kind(),
        SyntaxKind::CaseBlock | SyntaxKind::IndentedCases | SyntaxKind::MatchType
    )
}

/// Whether a child of a clause list can be aligned: a `case` clause that
/// starts its line, with everything up to its `=>` on that line.
fn alignable_clause(child: &SyntaxElement<'_>) -> bool {
    let NodeOrToken::Node(clause) = child else {
        return false;
    };
    if !matches!(
        clause.kind(),
        SyntaxKind::CaseClause | SyntaxKind::TypeCaseClause
    ) {
        return false;
    }
    let Some(arrow) = clause
        .non_trivia_children()
        .find(|c| matches!(c, NodeOrToken::Token(t) if matches!(t.text(), "=>" | "\u{21D2}")))
    else {
        return false;
    };
    let head = usize::from(arrow.text_range().end() - clause.text_range().start());
    first_code_token(child).is_some_and(|t| starts_line(&t))
        && !clause.text()[..head].contains(['\n', '\r'])
}

/// The first token of an element that isn't trivia or missing.
fn first_code_token<'t>(element: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match element {
        NodeOrToken::Token(token) => Some(*token).filter(|t| !t.is_trivia() && !t.is_missing()),
        NodeOrToken::Node(node) => node.tokens().find(|t| !t.is_trivia() && !t.is_missing()),
    }
}

/// The last token of an element that isn't trivia or missing.
fn last_code_token<'t>(element: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match element {
        NodeOrToken::Token(token) => Some(*token).filter(|t| !t.is_trivia() && !t.is_missing()),
        NodeOrToken::Node(node) => node.last_code_token(),
    }
}

/// Where each comment in the tree starts, in order.
fn comment_starts(tree: &Tree) -> Vec<TextSize> {
    let mut starts: Vec<_> = tree
        .syntax()
        .tokens()
        .flat_map(|t| t.leading_trivia().chain(t.trailing_trivia()))
        .chain(tree.eof_trivia())
        .filter(|t| t.is_comment())
        .map(|t| t.text_range().start())
        .collect();
    starts.sort();
    starts
}

/// Whether one of the sorted `offsets` is in `range`.
fn has_offset_in(offsets: &[TextSize], range: TextRange) -> bool {
    let i = offsets.partition_point(|&o| o < range.start());
    offsets.get(i).is_some_and(|&o| o < range.end())
}

/// Whether the token starts its line.
fn starts_line(token: &SyntaxToken<'_>) -> bool {
    token
        .leading_trivia()
        .any(|t| t.kind() == TriviaKind::Newline)
}

/// The blank lines a top-level or member statement needs above it.
struct BlankRules {
    /// At least this many, always.
    always: usize,
    /// At least this many if the statement prints over several lines: the
    /// printer finds out.
    when_long: usize,
    /// At least this many if the statement before it is a val, var or def
    /// that prints over several lines.
    after_long: usize,
    /// Whether the statement is a val, var or def.
    binding: bool,
}

/// The statement `next` starts, if it needs blank lines above it, and how
/// many: 2 above a top-level class, trait, object or enum, 1 between
/// companions, 1 between the imports and any other statement, and 1 after
/// the package clauses. A class, trait, object or enum in a body needs 1,
/// unless it comes first. A val, var or def needs 1 after anything else, and
/// when it's long, after anything; and anything needs 1 after a long one.
/// Every statement is returned, as the printer measures each one. Nothing is
/// asked at the start of the file.
fn blank_rules_above<'t>(next: &SyntaxToken<'t>) -> Option<(SyntaxNode<'t>, BlankRules)> {
    if next.is_trivia() || next.is_missing() {
        return None;
    }
    // The statement `next` starts: the outermost node it's the first token
    // of, if the file or a body is its parent.
    let mut statement = None;
    for node in next.parent_ancestors() {
        let first = std::iter::successors(node.first_token(), SyntaxToken::next_token)
            .find(|t| !t.is_trivia());
        if first.is_none_or(|first| first != *next) {
            break;
        }
        if node.parent().is_some_and(|p| {
            matches!(
                p.kind(),
                SyntaxKind::CompilationUnit | SyntaxKind::TemplateBody | SyntaxKind::EnumBody
            )
        }) {
            statement = Some(node);
            break;
        }
    }
    let statement = statement?;
    let mut previous = statement.prev_sibling_or_token();
    let previous = loop {
        match previous {
            Some(NodeOrToken::Node(n)) if !n.is_extra() => break n,
            Some(NodeOrToken::Node(n)) => previous = n.prev_sibling_or_token(),
            Some(NodeOrToken::Token(t)) => previous = t.prev_sibling_or_token(),
            None => {
                // The first statement needs nothing above it, but a
                // later one may need to know if it was long.
                let binding = matches!(
                    statement.kind(),
                    SyntaxKind::ValDefinition
                        | SyntaxKind::ValDeclaration
                        | SyntaxKind::VarDefinition
                        | SyntaxKind::VarDeclaration
                        | SyntaxKind::FunctionDefinition
                        | SyntaxKind::FunctionDeclaration
                );
                return Some((
                    statement,
                    BlankRules {
                        always: 0,
                        when_long: 0,
                        after_long: 0,
                        binding,
                    },
                ));
            }
        }
    };
    let is_template = |n: &SyntaxNode<'t>| {
        matches!(
            n.kind(),
            SyntaxKind::ClassDefinition
                | SyntaxKind::TraitDefinition
                | SyntaxKind::ObjectDefinition
                | SyntaxKind::EnumDefinition
        )
    };
    let name = |n: &SyntaxNode<'t>| {
        n.child_by_field_name("name").map(|e| match e {
            NodeOrToken::Node(n) => n.text(),
            NodeOrToken::Token(t) => t.text(),
        })
    };
    let is_binding = |n: &SyntaxNode<'t>| {
        matches!(
            n.kind(),
            SyntaxKind::ValDefinition
                | SyntaxKind::ValDeclaration
                | SyntaxKind::VarDefinition
                | SyntaxKind::VarDeclaration
                | SyntaxKind::FunctionDefinition
                | SyntaxKind::FunctionDeclaration
        )
    };
    let rules = |always, when_long| BlankRules {
        always,
        when_long,
        after_long: 1,
        binding: is_binding(&statement),
    };
    // A val, var or def may follow another with any number of blank lines,
    // unless it's long.
    let binding = || rules(usize::from(!is_binding(&previous)), 1);
    let found = if statement.parent().is_some_and(|p| p.kind() != SyntaxKind::CompilationUnit) {
        // In a body.
        match () {
            _ if is_template(&statement) => rules(1, 0),
            _ if is_binding(&statement) => binding(),
            _ => rules(0, 0),
        }
    } else {
        match (previous.kind(), is_template(&statement)) {
            // A package clause without a body, except one more of them.
            (SyntaxKind::PackageClause, _) if previous.child_by_field_name("body").is_none() => {
                rules(usize::from(statement.kind() != SyntaxKind::PackageClause), 0)
            }
            (SyntaxKind::ImportDeclaration, _)
                if statement.kind() == SyntaxKind::ImportDeclaration =>
            {
                rules(0, 0)
            }
            (SyntaxKind::ImportDeclaration, true) => rules(2, 0),
            (SyntaxKind::ImportDeclaration, false) => rules(1, 0),
            (_, false) if is_binding(&statement) => binding(),
            (_, false) => rules(0, 0),
            (_, true) => {
                let companions = is_template(&previous)
                    && (previous.kind() == SyntaxKind::ObjectDefinition)
                        != (statement.kind() == SyntaxKind::ObjectDefinition)
                    && name(&previous).is_some()
                    && name(&previous) == name(&statement);
                rules(if companions { 1 } else { 2 }, 0)
            }
        }
    };
    Some((statement, found))
}

/// The column of the offset in its line, in characters.
fn column_of(text: &str, offset: TextSize) -> usize {
    let before = &text[..usize::from(offset)];
    before[before.rfind('\n').map_or(0, |i| i + 1)..].chars().count()
}

/// Whether `token` is in a statement of a template: the body of a class,
/// object, trait or enum, an extension's methods, a package, or the file.
/// Otherwise it's in a block, a method body or `case` clauses.
fn in_template(token: &SyntaxToken<'_>) -> bool {
    for node in token.parent_ancestors() {
        match node.kind() {
            SyntaxKind::CompilationUnit
            | SyntaxKind::TemplateBody
            | SyntaxKind::EnumBody
            | SyntaxKind::WithTemplateBody
            | SyntaxKind::ExtensionDefinition => return true,
            SyntaxKind::Block
            | SyntaxKind::IndentedBlock
            | SyntaxKind::IndentedCases
            | SyntaxKind::CaseBlock => return false,
            _ => {}
        }
    }
    true
}

/// The part of a select chain a node continues: the qualifier of a select,
/// the function of a call, or the function given type arguments.
fn chain_spine(node: SyntaxNode<'_>) -> Option<SyntaxElement<'_>> {
    let field = match node.kind() {
        SyntaxKind::FieldExpression => "value",
        SyntaxKind::CallExpression | SyntaxKind::GenericFunction => "function",
        _ => return None,
    };
    node.non_trivia_children()
        .find(|c| c.field_name() == Some(field))
}

/// The operands and operators of an infix chain at one precedence, in
/// order. Nested chains at the same precedence on the associative side are
/// merged: the left one for most operators, the right one for operators
/// ending in `:`, such as `a :: b :: c`.
fn flatten_infix<'t>(
    node: SyntaxNode<'t>,
    operands: &mut Vec<SyntaxElement<'t>>,
    operators: &mut Vec<SyntaxToken<'t>>,
) -> Option<()> {
    let field = |name: &str| {
        node.non_trivia_children()
            .find(|c| c.field_name() == Some(name))
    };
    let (left, NodeOrToken::Token(op), right) =
        (field("left")?, field("operator")?, field("right")?)
    else {
        return None;
    };
    let level = precedence(op.text());
    let right_assoc = op.text().ends_with(':');
    let same = |e: &SyntaxElement<'t>| match e {
        NodeOrToken::Node(n) if n.kind() == SyntaxKind::InfixExpression => n
            .non_trivia_children()
            .find(|c| c.field_name() == Some("operator"))
            .is_some_and(|o| match o {
                NodeOrToken::Token(t) => {
                    precedence(t.text()) == level && t.text().ends_with(':') == right_assoc
                }
                NodeOrToken::Node(_) => false,
            }),
        _ => false,
    };
    match left {
        NodeOrToken::Node(n) if !right_assoc && same(&left) => {
            flatten_infix(n, operands, operators)?
        }
        _ => operands.push(left),
    }
    operators.push(op);
    match right {
        NodeOrToken::Node(n) if right_assoc && same(&right) => {
            flatten_infix(n, operands, operators)?
        }
        _ => operands.push(right),
    }
    Some(())
}

/// Whether `node` is inside parentheses or brackets, with no braces
/// between: there, in Scala 2, a line break doesn't end a statement, so a
/// line may start with an infix operator. What tree-sitter-scala reads as
/// an indented block, as a lambda's body on the line after its arrow, is
/// no region in Scala 2, where this is asked.
fn inside_parentheses(node: SyntaxNode<'_>) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        // The brackets among the parent's own tokens still open at `child`.
        let mut open = Vec::new();
        for c in parent.children_with_tokens() {
            if c.text_range().start() >= child.text_range().start() {
                break;
            }
            if let NodeOrToken::Token(t) = c {
                match t.text() {
                    "(" | "[" | "{" => open.push(t.text()),
                    ")" | "]" | "}" => {
                        open.pop();
                    }
                    _ => {}
                }
            }
        }
        if let Some(&innermost) = open.last() {
            return innermost != "{";
        }
        child = parent;
    }
    false
}

/// How tightly an infix operator binds, lowest first (SLS 6.12.3).
fn precedence(op: &str) -> u8 {
    // An assignment operator, such as `+=`, binds loosest.
    if is_assignment_operator(op) {
        return 0;
    }
    match op.chars().next() {
        Some('|') => 2,
        Some('^') => 3,
        Some('&') => 4,
        Some('=' | '!') => 5,
        Some('<' | '>') => 6,
        Some(':') => 7,
        Some('+' | '-') => 8,
        Some('*' | '/' | '%') => 9,
        Some(c) if c.is_alphanumeric() || c == '_' || c == '`' || c == '$' => 1,
        _ => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;

    fn fmt(source: &str) -> String {
        format(source, &FormatOptions::default()).unwrap_or_else(|e| panic!("{e}"))
    }

    /// Options for Scala 2.13, which keeps braces.
    fn scala2() -> FormatOptions {
        FormatOptions {
            dialect: crate::config::Dialect::named("scala213").unwrap(),
            ..FormatOptions::default()
        }
    }

    #[test]
    fn trailing_whitespace_is_removed_outside_strings() {
        let source = "val a = 1   \nval s = \"\"\"line   \n  more\"\"\"  \n";
        assert_eq!(
            fmt(source),
            "val a = 1\n\nval s = \"\"\"line   \n  more\"\"\"\n"
        );
    }

    #[test]
    fn crlf_files_keep_crlf() {
        let crlf = "object A {\r\n  val x =   1\r\n}\r\n";
        assert_eq!(
            format(crlf, &scala2()).unwrap(),
            "object A {\r\n  val x = 1\r\n}\r\n"
        );
        // Formatted as the LF file is, Scaladoc too.
        let doc = "/**\n   * a\n   */\nobject A {\n  val s = \"\"\"x\n  y\"\"\"\n}\n";
        let lf = format(doc, &scala2()).unwrap();
        let crlf = format(&doc.replace('\n', "\r\n"), &scala2()).unwrap();
        assert_eq!(crlf, lf.replace('\n', "\r\n"));
    }

    #[test]
    fn mixed_line_endings_become_newline() {
        let mixed = "object A {\r\n  val x = 1\n}\r\n";
        assert_eq!(
            format(mixed, &scala2()).unwrap(),
            "object A {\n  val x = 1\n}\n"
        );
        // Inside a string, a line ending is part of the value and stays.
        let string = "val s = \"\"\"a\r\nb\"\"\"\n";
        assert_eq!(fmt(string), "val s = \"\"\"a\r\nb\"\"\"\n");
    }

    #[test]
    fn blank_lines_in_a_row_follow_the_style() {
        let source = concat!(
            "val a = 1\n\n\n\n\nobject B {\n  object C {\n    val c = 1\n\n\n\n    val d = 2\n  }\n\n\n\n",
            "  def f = {\n    val x = 1\n\n\n\n    x\n  }\n}\n",
        );
        let options = scala2();
        let with = |templates, blocks| {
            let style = Style {
                max_blank_lines_in_templates: templates,
                max_blank_lines_in_blocks: blocks,
                ..STYLE
            };
            format_with(source, &options, Rules::Structural, style).unwrap()
        };
        // Templates, nested or not, get the first limit; a method body the
        // second.
        assert_eq!(
            with(2, 1),
            concat!(
                "val a = 1\n\n\nobject B {\n  object C {\n    val c = 1\n\n\n    val d = 2\n  }\n\n\n",
                "  def f = {\n    val x = 1\n\n    x\n  }\n}\n",
            )
        );
        assert_eq!(
            with(1, 0),
            concat!(
                "val a = 1\n\nobject B {\n  object C {\n    val c = 1\n\n    val d = 2\n  }\n\n",
                "  def f = {\n    val x = 1\n    x\n  }\n}\n",
            )
        );
    }

    #[test]
    fn a_blank_line_follows_top_level_imports() {
        assert_eq!(
            fmt("import a.b\nimport c.d\nval x = 1\n"),
            "import a.b\nimport c.d\n\nval x = 1\n"
        );
        assert_eq!(
            fmt("import a.b // c\n// doc\nval x = 1\n"),
            "import a.b // c\n\n// doc\nval x = 1\n"
        );
        assert_eq!(
            fmt("import a.b\n\n\nval x = 1\n"),
            "import a.b\n\n\nval x = 1\n",
            "more blank lines stay"
        );
        assert_eq!(fmt("import a.b\nimport c.d\n"), "import a.b\nimport c.d\n");
        let nested = "object A:\n  import a.b\n\n  val x = 1\n";
        assert_eq!(fmt(nested), nested, "in a body too, by the rule for a val");
    }

    #[test]
    fn two_blank_lines_go_above_top_level_templates() {
        assert_eq!(
            fmt("val v = 1\nclass A\ntrait B\nobject C\n"),
            "val v = 1\n\n\nclass A\n\n\ntrait B\n\n\nobject C\n"
        );
        assert_eq!(
            fmt("import a.b\nclass A\n"),
            "import a.b\n\n\nclass A\n",
            "after imports"
        );
        assert_eq!(
            fmt("class A\n// doc\nclass B\n"),
            "class A\n\n\n// doc\nclass B\n",
            "above the comment"
        );
        assert_eq!(
            fmt("class A\n\n\n\nclass B\n"),
            "class A\n\n\nclass B\n",
            "capped as before"
        );
        assert_eq!(
            fmt("class A\nobject A\nobject B\ntrait B\n"),
            "class A\n\nobject A\n\n\nobject B\n\ntrait B\n",
            "one between companions, either order"
        );
        assert_eq!(
            fmt("class A\nobject B\n"),
            "class A\n\n\nobject B\n",
            "different names"
        );
        assert_eq!(
            fmt("package p\nclass A\n"),
            "package p\n\nclass A\n",
            "one after a package clause"
        );
        assert_eq!(
            fmt("package p\npackage q\nimport a.b\n"),
            "package p\npackage q\n\nimport a.b\n",
            "none between package clauses"
        );
        assert_eq!(
            fmt("object A:\n  class B\n  class C\n"),
            "object A:\n  class B\n\n  class C\n",
            "one in a body, none for the first"
        );
        let block = "def f =\n  class B\n  class C\n";
        assert_eq!(fmt(block), block, "none in a block");
    }

    #[test]
    fn edge_cases() {
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("\n\n"), "");
        assert_eq!(fmt("// only a comment"), "// only a comment\n");
        assert_eq!(fmt("   "), "");
        assert_eq!(fmt("val x = 1\n  "), "val x = 1\n", "a last line of spaces");
        assert_eq!(
            fmt("val x = 1\n\n\n"),
            "val x = 1\n",
            "blank lines at the end"
        );
        assert_eq!(
            fmt("val x = 1"),
            "val x = 1\n",
            "a missing final line break"
        );
    }

    #[test]
    fn syntax_errors_are_refused() {
        let err = format("object A {\n  def f( = 1\n", &FormatOptions::default()).unwrap_err();
        assert!(
            matches!(err, FormatError::SyntaxErrors(ref errors) if !errors.is_empty()),
            "{err}"
        );
        assert!(
            err.to_string()
                .starts_with("not formatting code with syntax errors")
        );
    }

    /// Building the document takes time in proportion to the input, however
    /// deep the nesting: eight times the depth takes about eight times as
    /// long, not 64. The inputs have no indentation, so their size grows
    /// with their depth alone.
    #[test]
    fn building_scales_linearly_with_nesting() {
        let calls = |depth: usize| {
            format!(
                "val x =\n{}b{}\n",
                "f(a,\n".repeat(depth),
                ")".repeat(depth)
            )
        };
        let blocks = |depth: usize| {
            let opens: String = (0..depth).map(|i| format!("def f{i} = {{\n")).collect();
            format!("object A {{\n{opens}1\n{}}}\n", "}\n".repeat(depth))
        };
        // The best of a few runs, to keep out noise.
        let time = |source: &str| {
            let tree = parse(source);
            (0..3)
                .map(|_| {
                    let start = std::time::Instant::now();
                    drop(to_doc(&tree));
                    start.elapsed()
                })
                .min()
                .unwrap()
        };
        // Formatting is recursive, so deep nesting needs a deep stack.
        std::thread::Builder::new()
            .stack_size(256 << 20)
            .spawn(move || {
                let makers: [(&str, &dyn Fn(usize) -> String); 2] =
                    [("calls", &calls), ("blocks", &blocks)];
                for (name, make) in makers {
                    let small = time(&make(500));
                    let large = time(&make(4000));
                    assert!(
                        large < small * 20,
                        "{name}: {small:?} at depth 500, {large:?} at 4000"
                    );
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// Corpus files the formatter correctly refuses because tree-sitter-scala
    /// parses the formatted code differently, with the reason.
    const KNOWN_REFUSALS: &[(&str, &str)] = &[(
        "scala3/tests/neg/binaryLiterals.scala",
        "the invalid literal `0b` reads as `0` and a name `b`; the space \
         between them makes `b` an operator ending its line, continued by \
         the `0b2` below",
    )];

    /// Rewrites every error-free file in the corpus to Scala 3's syntax and
    /// to Scala 2.13's, checking that each result is
    /// equivalent, stable when formatted again, and has none of the other
    /// syntax left, unless tree-sitter-scala read a construct differently
    /// rewritten, which leaves it as written. Run with:
    /// `cargo test --release corpus_rewrites -- --ignored --nocapture`
    #[test]
    #[ignore = "slow; runs over corpora/ or $SCRUFF_CORPUS"]
    fn corpus_rewrites_to_the_dialect() {
        use crate::config::Dialect;
        let files = crate::lossless_tree::corpus::corpus_files();
        let mut failures = Vec::new();
        for (name, new) in [("scala3", true), ("scala213", false)] {
            let dialect = Dialect::named(name).unwrap();
            let options = FormatOptions {
                dialect,
                ..FormatOptions::default()
            };
            // For each file, whether it was rewritten, and its failures.
            let per_file: Vec<(bool, usize, Vec<String>)> = files
                .par_iter()
                .filter_map(|path| {
                    let source = std::fs::read_to_string(path).ok()?;
                    let tree = parse(&source);
                    if !tree.errors().is_empty()
                        || KNOWN_REFUSALS.iter().any(|(file, _)| path.ends_with(file))
                    {
                        return None;
                    }
                    let result = rewrite::rewrite(&tree, dialect);
                    let rewritten = result.is_some();
                    let dropped = result.map_or(0, |r| r.dropped);
                    let mut failures = Vec::new();
                    let out = match format(&source, &options) {
                        Ok(out) => out,
                        Err(e) => {
                            failures.push(format!("{name}: {}: {e}", path.display()));
                            return Some((rewritten, dropped, failures));
                        }
                    };
                    if format(&out, &options).as_deref() != Ok(out.as_str()) {
                        failures.push(format!("{name}: {}: not idempotent", path.display()));
                    }
                    let tree = parse(&out);
                    let other = tree.syntax().descendants().find(|n| {
                        let wildcard = if new { "_" } else { "*" };
                        let rename = if new {
                            SyntaxKind::ArrowRenamedIdentifier
                        } else {
                            SyntaxKind::AsRenamedIdentifier
                        };
                        // `import foo as f` has no Scala 2 form.
                        n.kind() == rename && (new || rewrite::after_a_path(*n))
                            || n.kind() == SyntaxKind::NamespaceWildcard && n.text() == wildcard
                            || new
                                && n.kind() == SyntaxKind::Wildcard
                                && crate::lossless_tree::is_type_wildcard_place(NodeOrToken::Node(
                                    *n,
                                ))
                            || matches!(n.kind(), SyntaxKind::Vararg | SyntaxKind::RepeatPattern)
                                && rewrite::splice(&out, *n, new).is_some()
                            || matches!(
                                n.kind(),
                                SyntaxKind::IfExpression
                                    | SyntaxKind::WhileExpression
                                    | SyntaxKind::ForExpression
                            ) && rewrite::control(&out, *n, dialect.quiet_syntax).is_some()
                            || new
                                && matches!(
                                    n.kind(),
                                    SyntaxKind::Block
                                        | SyntaxKind::CaseBlock
                                        | SyntaxKind::TemplateBody
                                        | SyntaxKind::EnumBody
                                        | SyntaxKind::ForExpression
                                )
                                && rewrite::braces(&tree, *n).is_some()
                            || !new
                                && matches!(
                                    n.kind(),
                                    SyntaxKind::IndentedBlock
                                        | SyntaxKind::IndentedCases
                                        | SyntaxKind::TemplateBody
                                        | SyntaxKind::EnumBody
                                )
                                && rewrite::braces_back(&tree, *n).is_some()
                            || new
                                && n.kind() == SyntaxKind::CallExpression
                                && rewrite::fewer_braces(&tree, *n).is_some()
                            || !new
                                && n.kind() == SyntaxKind::ColonArgument
                                && rewrite::fewer_braces_back(&tree, *n).is_some()
                            || new && rewrite::and_type(*n)
                            || n.kind() == SyntaxKind::ExtendsClause
                                && rewrite::extends_separators(&tree, *n, new).is_some()
                    });
                    // Unless it's copied verbatim, as in `format: off`, or left as
                    // written because tree-sitter-scala reads it differently
                    // rewritten: rewriting the output again leaves it out.
                    let other =
                        other.filter(|n| tree.verbatim_at(n.text_range().start()).is_none());
                    let misread = other.is_some()
                        && rewrite::rewrite(&tree, dialect).is_none_or(|r| r.dropped > 0);
                    let dropped = dropped.max(usize::from(misread));
                    if let Some(node) = other.filter(|_| !misread) {
                        let (line, col) = tree.line_col(node.text_range().start());
                        failures.push(format!(
                            "{name}: {}:{line}:{col}: other syntax left",
                            path.display()
                        ));
                    }
                    Some((rewritten, dropped, failures))
                })
                .collect();
            let rewritten = per_file
                .iter()
                .filter(|(rewritten, _, _)| *rewritten)
                .count();
            let with_dropped = per_file.iter().filter(|(_, d, _)| *d > 0).count();
            let dropped: usize = per_file.iter().map(|(_, d, _)| d).sum();
            failures.extend(per_file.into_iter().flat_map(|(_, _, failures)| failures));
            println!(
                "{name}: {} files, {rewritten} rewritten, {dropped} constructs in {with_dropped} files left as written",
                files.len()
            );
        }
        for failure in failures
            .iter()
            .take(std::env::var("SCRUFF_SHOW").map_or(15, |n| n.parse().unwrap_or(15)))
        {
            println!("{failure}");
        }
        println!("{} failed", failures.len());
        assert!(failures.is_empty());
    }

    /// Formats every error-free file in the corpus, checking that each
    /// result is equivalent and is stable when formatted again. Run with:
    /// `cargo test --release corpus -- --ignored --nocapture`
    #[test]
    #[ignore = "slow; runs over corpora/ or $SCRUFF_CORPUS"]
    fn corpus_formats_every_file() {
        let files = crate::lossless_tree::corpus::corpus_files();
        /// What became of one file.
        enum Outcome {
            Skipped,
            Refused,
            Failed(String),
            /// Lines before and after, and whether formatting again changed it.
            Formatted {
                changed: bool,
                before: usize,
                after: usize,
                stable: bool,
            },
        }
        // Lines with code or comments.
        let lines = |s: &str| s.lines().filter(|l| !l.trim().is_empty()).count();
        let outcomes: Vec<(&std::path::PathBuf, Outcome)> = files
            .par_iter()
            .filter_map(|path| {
                let source = std::fs::read_to_string(path).ok()?;
                let outcome = match format(&source, &FormatOptions::default()) {
                    Err(FormatError::SyntaxErrors(_)) => Outcome::Skipped,
                    Err(FormatError::NotEquivalent(_))
                        if KNOWN_REFUSALS.iter().any(|(file, _)| path.ends_with(file)) =>
                    {
                        Outcome::Refused
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                    Ok(out) => Outcome::Formatted {
                        changed: out != source,
                        before: lines(&source),
                        after: lines(&out),
                        stable: format(&out, &FormatOptions::default()).as_deref()
                            == Ok(out.as_str()),
                    },
                };
                Some((path, outcome))
            })
            .collect();
        let mut refused = 0;
        let (mut formatted, mut changed, mut skipped, mut failures) = (0, 0, 0, Vec::new());
        let (mut broken, mut lines_added, mut joined, mut lines_removed) = (0, 0, 0, 0);
        for (path, outcome) in outcomes {
            match outcome {
                Outcome::Skipped => skipped += 1,
                Outcome::Refused => refused += 1,
                Outcome::Failed(e) => failures.push(format!("{}: {e}", path.display())),
                Outcome::Formatted {
                    changed: c,
                    before,
                    after,
                    stable,
                } => {
                    formatted += 1;
                    changed += usize::from(c);
                    if after > before {
                        broken += 1;
                        lines_added += after - before;
                    }
                    if after < before {
                        joined += 1;
                        lines_removed += before - after;
                    }
                    if !stable {
                        failures.push(format!("{}: not idempotent", path.display()));
                    }
                }
            }
        }
        for failure in failures.iter().take(15) {
            println!("{failure}");
        }
        println!(
            "{} files: {formatted} formatted ({changed} changed, {broken} with {lines_added} lines added, {joined} with {lines_removed} lines removed), {skipped} skipped for syntax errors, {refused} known refusals, {} failed",
            files.len(),
            failures.len()
        );
        assert!(failures.is_empty());
    }

    /// Formats `source` with structural rules and with only the fallback,
    /// at a width where nothing needs to break, describing the first
    /// difference, if any. Lists written over several lines are joined
    /// first, since the fallback keeps the source's line breaks.
    fn rules_disagree(source: &str) -> Option<String> {
        let options = FormatOptions {
            max_width: 1_000_000,
            ..FormatOptions::default()
        };
        let joined = format_with(source, &options, Rules::Structural, STYLE);
        let source = joined.as_deref().unwrap_or(source);
        let structural = format_with(source, &options, Rules::Structural, STYLE);
        let fallback = format_with(source, &options, Rules::Fallback, STYLE);
        if structural == fallback {
            return None;
        }
        let show = |r: &Result<String, FormatError>| match r {
            Ok(out) => out.clone(),
            Err(e) => format!("error: {e}\n"),
        };
        let (s, f) = (show(&structural), show(&fallback));
        let line = s
            .lines()
            .zip(f.lines())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| s.lines().count().min(f.lines().count()));
        Some(format!(
            "first difference at output line {}:\n  structural: {:?}\n  fallback:   {:?}",
            line + 1,
            s.lines().nth(line).unwrap_or("<end>"),
            f.lines().nth(line).unwrap_or("<end>"),
        ))
    }

    /// Where everything fits on its line, a structural rule must print
    /// exactly what the fallback prints. Checks the inputs of every `.stat`
    /// case.
    #[test]
    fn structural_rules_match_fallback_on_stat_inputs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fmt");
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "stat") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for case in text.split("\n<<<").skip(1) {
                let (title, body) = case.split_once('\n').unwrap_or((case, ""));
                let input = body.split("\n>>>").next().unwrap_or("");
                let input = input.split_once("===\n").map_or(input, |(_, rest)| rest);
                if let Some(difference) = rules_disagree(&format!("{input}\n")) {
                    failures.push(format!(
                        "{}: {}\n{difference}",
                        path.display(),
                        title.trim()
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    /// The same check over the corpus. Run with:
    /// `cargo test --release corpus -- --ignored --nocapture`
    #[test]
    #[ignore = "slow; runs over corpora/ or $SCRUFF_CORPUS"]
    fn corpus_structural_rules_match_fallback() {
        let files = crate::lossless_tree::corpus::corpus_files();
        let failures: Vec<String> = files
            .par_iter()
            .filter_map(|path| {
                let source = std::fs::read_to_string(path).ok()?;
                let difference = rules_disagree(&source)?;
                Some(format!("{}: {difference}", path.display()))
            })
            .collect();
        for failure in failures.iter().take(15) {
            println!("{failure}");
        }
        println!("{} files, {} differ", files.len(), failures.len());
        assert!(failures.is_empty());
    }

    /// sbt's code is formatted by scalafmt with the configuration scruff
    /// follows, so every change scruff makes to it is a disagreement with
    /// scalafmt. Writes the changed files out to compare, without failing.
    /// Run with:
    /// `cargo test --release corpus_sbt -- --ignored --nocapture`
    #[test]
    #[ignore = "slow; reports differences from scalafmt over corpora/sbt"]
    fn corpus_sbt_differences_from_scalafmt() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora/sbt");
        // The files sbt's `.scalafmt.conf` formats, at its width and in its
        // dialects.
        let config = crate::config::Config::load(&root.join(crate::config::SCALAFMT_FILE)).unwrap();
        let options = |file: &std::path::Path| FormatOptions {
            max_width: config.max_column,
            dialect: config.dialect_for(Some(file)),
        };
        let files: Vec<_> = crate::lossless_tree::corpus::corpus_files()
            .into_iter()
            .filter(|f| f.starts_with(&root) && config.includes(f))
            .collect();
        // Formatted copies of the changed files, to compare with `diff -r`.
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sbt-formatted");
        let _ = std::fs::remove_dir_all(&out_dir);
        let changed = files
            .par_iter()
            .filter(|path| {
                let Ok(source) = std::fs::read_to_string(path) else {
                    return false;
                };
                let Ok(out) = format(&source, &options(path)) else {
                    return false;
                };
                if out == source {
                    return false;
                }
                let copy = out_dir.join(path.strip_prefix(&root).unwrap());
                std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
                std::fs::write(copy, out).unwrap();
                true
            })
            .count();
        println!("formatted copies in {}", out_dir.display());
        println!("{} sbt files, {changed} changed", files.len());
    }
}
