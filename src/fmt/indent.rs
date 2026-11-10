//! Re-indentation by uniform shifts, decided from the source and laid out
//! by the printer.
//!
//! A container is a place whose statements sit on their own lines: a block
//! in braces, a Scala 3 indentation region, or the whole file. Its
//! statements belong 2 spaces deeper than the line that opens it, or at
//! column 0 for the file. That line is where what it belongs to starts: a
//! definition, or an `if`, a lambda or a call that starts on the line of
//! the `{` or the region's opener; otherwise the opener's own line.
//! Every line inside a container moves by the same amount, so a statement
//! keeps the shape of its continuation lines, and a nested container is then
//! placed from its opener's new position.
//!
//! A container whose statements aren't each on their own line at one
//! indentation moves with the container around it instead, unless it's in
//! braces and no statement is deeper than the one before it: then each run
//! of statements at one width moves to where they all belong. Files with tabs
//! in their indentation, or with code that is copied verbatim for its
//! indentation (`format: off`, syntax errors, unsafe regions), keep theirs.
//!
//! The result is a [`Layout`]: which nodes start a frame on their opener's
//! line, which containers nest their statements, and each line's offset
//! from where its frame puts lines. The printer turns those into columns,
//! so a container follows its opener wherever the opener ends up.

use std::collections::HashMap;

use crate::T;
use crate::lossless_tree::{
    NodeOrToken, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TextRange, TextSize, Tree,
    VerbatimReason,
};

/// What the formatter lays out itself, so the layout follows it.
#[derive(Default, Debug)]
pub(super) struct Relaid {
    /// The `(` and `)` of lists whose line breaks the formatter decides, or
    /// the `{` and `}` of such a block argument: what follows the close ends
    /// up on a line indented as the open's line, whether the list is joined
    /// or broken.
    pub(super) lists: Vec<(TextSize, TextSize)>,
    /// Where lines start that the formatter joins to the line before or
    /// breaks itself, as a boolean chain's: they aren't laid out, and what
    /// they hold is on the line before.
    pub(super) joined: Vec<TextSize>,
    /// Where lines start that the formatter breaks itself, as a select
    /// chain's `.`: they're laid out, but count as on the line before for
    /// where a block's frame starts.
    pub(super) continued: Vec<TextSize>,
    /// The `{` of each match after a select chain the formatter lays out,
    /// or its `match` without braces: its cases are indented from the line
    /// that ends up on, not from where the chain starts.
    pub(super) after_chains: Vec<TextSize>,
    /// Whether a boolean chain the formatter lays out is written over
    /// several lines.
    pub(super) broken_chains: bool,
}

/// How many spaces each container's statements are indented past its
/// opener's line.
const STEP: usize = 2;

/// Where lines go, for the formatter to build into its document. The
/// default keeps every line's indentation as written.
#[derive(Default, Debug)]
pub(super) struct Layout {
    /// Each line's indentation past where its frame puts lines, keyed by
    /// its first non-blank character.
    offsets: HashMap<TextSize, isize>,
    /// Nodes that start a frame: lines inside them are placed from the
    /// line the frame starts on.
    frames: HashMap<(SyntaxKind, TextRange), Vec<Frame>>,
    /// Containers whose statements are placed from their frame.
    containers: HashMap<(SyntaxKind, TextRange), Nested>,
    /// The new width of each line that moves, as the source reads, for
    /// tests of which containers are placed.
    #[cfg_attr(not(test), allow(dead_code))]
    widths: HashMap<TextSize, usize>,
}

/// Where a frame starts in its node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Frame {
    /// After the node's leading line break and indentation, on the line of
    /// its first token.
    AfterLeading,
    /// From this child token to the end of the region it opens, or of the
    /// node: a region's opener, or a match type's `{`.
    At(TextSize, TextSize),
}

/// A container placed from its frame.
#[derive(Clone, Copy, Debug)]
pub(super) struct Nested {
    /// Where its statements are, so what's nested.
    pub(super) inner: TextRange,
    /// Whether its statements go one level deeper than its frame's line;
    /// `case` clauses may stay at its width.
    pub(super) deeper: bool,
}

impl Layout {
    /// `relaid` is what the formatter lays out itself.
    pub(super) fn new(tree: &Tree, relaid: &Relaid) -> Layout {
        compute(tree, relaid).unwrap_or_default()
    }

    /// The line's indentation past where its frame puts lines, for the line
    /// whose first non-blank character is at `first`. `None` if the line
    /// keeps its indentation as written.
    pub(super) fn offset(&self, first: TextSize) -> Option<isize> {
        self.offsets.get(&first).copied()
    }

    /// Whether the node starts a frame after its leading trivia.
    pub(super) fn frames_after_leading(&self, node: SyntaxNode<'_>) -> bool {
        self.frames(node).contains(&Frame::AfterLeading)
    }

    /// Where the node's frames from child tokens start and end, in order.
    pub(super) fn frames_at(&self, node: SyntaxNode<'_>) -> Vec<(TextSize, TextSize)> {
        let mut at: Vec<_> = self
            .frames(node)
            .iter()
            .filter_map(|f| match f {
                Frame::At(token, end) => Some((*token, *end)),
                Frame::AfterLeading => None,
            })
            .collect();
        at.sort();
        at
    }

    fn frames(&self, node: SyntaxNode<'_>) -> &[Frame] {
        self.frames
            .get(&(node.kind(), node.text_range()))
            .map_or(&[], Vec::as_slice)
    }

    /// How the node nests its statements, if it's a placed container.
    pub(super) fn container(&self, node: SyntaxNode<'_>) -> Option<Nested> {
        self.containers
            .get(&(node.kind(), node.text_range()))
            .copied()
    }

    /// Whether every line keeps its indentation as written.
    pub(super) fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    #[cfg(test)]
    pub(super) fn width(&self, offset: TextSize) -> Option<usize> {
        self.widths.get(&offset).copied()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.widths.len()
    }
}

struct Container<'t> {
    /// The node that holds the statements.
    node: SyntaxNode<'t>,
    /// The node that starts its frame, and where.
    frame: Option<(SyntaxNode<'t>, Frame)>,
    /// Where its statements can be: between the braces, or the region.
    inner: TextRange,
    statements: Vec<SyntaxElement<'t>>,
    /// The token whose line the statements are indented from. `None` for the
    /// file.
    opener: Option<SyntaxToken<'t>>,
    /// Scala 3 `case` clauses, which may sit at the width of their `match`.
    cases: bool,
    /// Whether its statements are in braces, where their indentation doesn't
    /// change what they mean.
    braced: bool,
    /// For one of the runs a container in braces is split into, by its
    /// statements' widths: see [`width_runs`].
    piece: Option<Piece>,
}

/// One of the runs of a container split by its statements' widths, which
/// are placed together or not at all.
#[derive(Clone, Copy)]
struct Piece {
    /// Which split container it's from.
    group: usize,
    /// Whether it's the last run, which ends where the container does.
    last: bool,
}

/// A line that starts with code or a comment.
struct Line {
    /// Its first non-blank character.
    first: TextSize,
    /// Its indentation, in spaces.
    width: usize,
    /// Whether it starts with a comment.
    comment: bool,
}

fn compute(tree: &Tree, relaid: &Relaid) -> Option<Layout> {
    let text = tree.syntax().text();
    let copied_for_layout = tree.verbatim_ranges().iter().any(|v| {
        matches!(
            v.reason,
            VerbatimReason::UnsafeIndentation(_)
                | VerbatimReason::FormatOff
                | VerbatimReason::SyntaxError
        )
    });
    if copied_for_layout {
        return None;
    }

    let mut lines = Vec::new();
    // Every line's first character, for the lines that print on the line
    // before them unless a rule breaks them to fit.
    let mut firsts = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_start_matches([' ', '\t']);
        let indent = &line[..line.len() - content.len()];
        let line_start = TextSize::try_from(start).ok()?;
        start += line.len();
        if content.trim().is_empty() {
            continue;
        }
        let first = line_start + TextSize::try_from(indent.len()).ok()?;
        firsts.push(first);
        if relaid.joined.binary_search(&first).is_ok() {
            continue;
        }
        // A line inside a multi-line string or comment is content.
        if tree
            .verbatim_at(line_start)
            .is_some_and(|v| v.range.start() < line_start)
        {
            continue;
        }
        if indent.contains('\t') {
            return None;
        }
        lines.push(Line {
            first,
            width: indent.len(),
            comment: content.starts_with("//") || content.starts_with("/*"),
        });
    }
    let line_of = |offset: TextSize| lines.partition_point(|l| l.first <= offset).checked_sub(1);
    // The line of a relaid list's `)` stands for the line of its `(`.
    let mut moved = HashMap::new();
    for &(open, close) in &relaid.lists {
        if let (Some(open), Some(close)) = (line_of(open), line_of(close))
            && open != close
        {
            moved.insert(close, open);
        }
    }
    let opener_line = |mut line: usize| {
        while let Some(&to) = moved.get(&line) {
            line = to;
        }
        line
    };

    let continued = continued_lines(&firsts, relaid);
    let mut containers = vec![Container {
        node: tree.syntax(),
        frame: None,
        inner: tree.syntax().text_range(),
        statements: tree.syntax().non_trivia_children().collect(),
        opener: None,
        cases: false,
        braced: false,
        piece: None,
    }];
    let mut groups = 0;
    for node in tree.syntax().descendants() {
        let Some(container) = container(node, &continued, &relaid.after_chains) else {
            continue;
        };
        match width_runs(&container) {
            Some(runs) => {
                containers.extend(pieces(&container, runs, groups));
                groups += 1;
            }
            None => containers.push(container),
        }
    }
    // Outer containers first, so each one's opener line is placed before it.
    containers.sort_by_key(|c| (c.inner.start(), std::cmp::Reverse(c.inner.end())));
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); groups];
    for (index, container) in containers.iter().enumerate() {
        if let Some(piece) = container.piece {
            members[piece.group].push(index);
        }
    }

    let mut parent = vec![0; containers.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (index, container) in containers.iter().enumerate() {
        while stack
            .last()
            .is_some_and(|&outer| containers[outer].inner.end() <= container.inner.start())
        {
            stack.pop();
        }
        parent[index] = stack.last().copied().unwrap_or(0);
        stack.push(index);
    }
    // The innermost container of each line: the last one, in order, that
    // holds it. One sweep: containers are pushed as lines reach them, and
    // the ones that ended are popped.
    let mut owner = vec![0; lines.len()];
    let mut open: Vec<usize> = Vec::new();
    let mut next = 0;
    for (line, o) in lines.iter().zip(&mut owner) {
        while containers
            .get(next)
            .is_some_and(|c| c.inner.start() <= line.first)
        {
            open.push(next);
            next += 1;
        }
        while open
            .last()
            .is_some_and(|&c| containers[c].inner.end() <= line.first)
        {
            open.pop();
        }
        *o = open.last().copied().unwrap_or(0);
    }
    // Each container's own lines.
    let mut owned: Vec<Vec<usize>> = vec![Vec::new(); containers.len()];
    for (line, &o) in owner.iter().enumerate() {
        owned[o].push(line);
    }

    let mut shift = vec![0isize; containers.len()];
    // For a container placed on its own: its statements' old and new
    // indentation.
    let mut placed: Vec<Option<(usize, usize)>> = vec![None; containers.len()];
    // For a placed container: whether its statements go a step deeper than
    // its opener's line.
    let mut deeper = vec![false; containers.len()];
    // Whether each split container's runs are placed.
    let mut groups_placed: Vec<Option<bool>> = vec![None; groups];
    for (index, container) in containers.iter().enumerate() {
        let inherited = if index == 0 { 0 } else { shift[parent[index]] };
        shift[index] = inherited;
        let Some(indent) = statement_indent(container) else {
            continue;
        };
        // Cases sit deeper than their `match`'s line, or at its width: any
        // shallower means tree-sitter-scala read the layout differently from
        // scalac, as a leading `||` taken into the last case. Other bodies
        // may be written at any column, and are moved in.
        let mut floor = 0;
        let target = match container.opener {
            None => 0,
            Some(opener) => {
                let Some(line) = line_of(opener.text_range().start()).map(opener_line) else {
                    continue;
                };
                let width = lines[line].width as isize + shift[owner[line]];
                if width < 0 {
                    continue;
                }
                // Cases written at the width of their `match` stay there, as
                // scalafmt leaves them, but not after a chain the formatter
                // lays out, where they go a step deeper.
                let after_chain = relaid
                    .after_chains
                    .binary_search(&opener.text_range().start())
                    .is_ok();
                let step = if container.cases && indent == lines[line].width && !after_chain {
                    0
                } else {
                    STEP
                };
                if container.cases {
                    floor = lines[line].width + if after_chain { 0 } else { step };
                }
                deeper[index] = step > 0;
                width as usize + step
            }
        };
        let accepts = |index: usize, indent: usize| {
            let container = &containers[index];
            let candidate = target as isize - indent as isize;
            // Every line of the container must stay at column 0 or deeper.
            // Comment lines don't count: they're placed with the statements.
            let fits = owned[index]
                .iter()
                .map(|&line| &lines[line])
                .filter(|l| !l.comment)
                .all(|l| l.width >= floor && l.width as isize + candidate >= 0);
            // A line right after the container that was shallower than its
            // statements stays shallower: in `xs.map: x =>`, the body, then
            // `.filter(p)` between the two, a body moved to the `.filter`
            // width would take it in. After a run that isn't the last, that
            // line is the next run's, placed with it.
            let next = lines.partition_point(|l| l.first < container.inner.end());
            let keeps_next = container.piece.is_some_and(|p| !p.last)
                || lines.get(next).is_none_or(|l| {
                    l.comment || l.width >= indent || l.width as isize + inherited < target as isize
                });
            fits && keeps_next
        };
        let accepted = match container.piece {
            None => accepts(index, indent),
            // A split container's runs share its opener, so its target.
            Some(piece) => *groups_placed[piece.group].get_or_insert_with(|| {
                members[piece.group]
                    .iter()
                    .all(|&m| statement_indent(&containers[m]).is_some_and(|i| accepts(m, i)))
            }),
        };
        if accepted {
            shift[index] = target as isize - indent as isize;
            placed[index] = Some((indent, target));
        }
    }

    let mut widths = HashMap::new();
    for (line, &o) in lines.iter().zip(&owner) {
        let width = match placed[o] {
            // A comment on its own line, shallower than the statements
            // around it, moves to their indentation, as in scalafmt.
            Some((indent, target)) if line.comment && line.width < indent && shift[o] != 0 => {
                target
            }
            // A container that inherits a shift can push a line past column
            // 0; the file keeps its indentation then.
            _ => usize::try_from(line.width as isize + shift[o]).ok()?,
        };
        if width != line.width {
            widths.insert(line.first, width);
        }
    }

    // Where each line is placed from: the innermost frame or placed
    // container around it. A frame places lines from its opener's line, a
    // container from its statements' indentation.
    struct Span {
        range: TextRange,
        /// The source width lines are placed from.
        from: usize,
        /// The container, for a container's statements.
        container: Option<usize>,
    }
    let mut spans = vec![Span {
        range: tree.syntax().text_range(),
        from: placed[0].map_or(0, |(indent, _)| indent),
        container: Some(0),
    }];
    let mut frames = HashMap::new();
    let mut nested = HashMap::new();
    let mut closes = Vec::new();
    for (index, container) in containers.iter().enumerate().skip(1) {
        // A container that isn't placed moves with the one around it.
        let Some((indent, _)) = placed[index] else {
            continue;
        };
        // Placed from its own first statement, as `case` clauses after
        // `xs.map:` are, it stays where it is: no frame.
        let Some((node, frame)) = container.frame else {
            continue;
        };
        let node_frames: &mut Vec<Frame> =
            frames.entry((node.kind(), node.text_range())).or_default();
        if !node_frames.contains(&frame) {
            node_frames.push(frame);
        }
        // The frame starts after the line break before its first token, so
        // that token's line is placed from outside it, and ends at its last
        // token: a node's range can take in the comments and blank lines
        // after it.
        let (first, end) = match frame {
            Frame::AfterLeading => (
                node.text_range().start(),
                node.last_code_token()
                    .map_or(node.text_range().end(), |t| t.text_range().end()),
            ),
            Frame::At(token, end) => (token, end),
        };
        let start = first + TextSize::from(1);
        if let Some(line) = line_of(first)
            && start < end
        {
            spans.push(Span {
                range: TextRange::new(start, end),
                from: lines[opener_line(line)].width,
                container: None,
            });
            // A `}` after a frame that starts on an earlier line than its
            // `{`, or a match's after a broken chain, goes at the frame's
            // line: written, it may line up with either line.
            if container.braced
                && container.piece.is_none_or(|p| p.last)
                && let Some(opener) = container.opener
                && (line_of(opener.text_range().start()).map(opener_line)
                    != Some(opener_line(line))
                    || relaid
                        .after_chains
                        .binary_search(&opener.text_range().start())
                        .is_ok())
                && let Some(close) = container.node.last_code_token()
                && close.text() == "}"
                && close.is_first_on_line()
            {
                closes.push(close.text_range().start());
            }
        }
        // Its statements, from their indentation. A split container's runs
        // are its statements together.
        let node = container.node;
        nested
            .entry((node.kind(), node.text_range()))
            .and_modify(|n: &mut Nested| n.inner = n.inner.cover(container.inner))
            .or_insert(Nested {
                inner: container.inner,
                deeper: deeper[index],
            });
        spans.push(Span {
            range: container.inner,
            from: indent,
            container: Some(index),
        });
    }
    // Outer spans first; a container's statements inside its frame.
    spans.sort_by_key(|s| {
        (
            s.range.start(),
            std::cmp::Reverse(s.range.end()),
            s.container.is_some(),
        )
    });
    let mut offsets = HashMap::new();
    let mut open: Vec<&Span> = Vec::new();
    let mut next = 0;
    for (line, &o) in lines.iter().zip(&owner) {
        while let Some(span) = spans.get(next).filter(|s| s.range.start() <= line.first) {
            while open
                .last()
                .is_some_and(|top| top.range.end() <= span.range.start())
            {
                open.pop();
            }
            open.push(span);
            next += 1;
        }
        while open.last().is_some_and(|top| top.range.end() <= line.first) {
            open.pop();
        }
        let span = open.last()?;
        let offset = match span.container {
            // The same comment rule as above.
            Some(c) if c == o && line.comment && line.width < span.from && shift[o] != 0 => 0,
            None if closes.contains(&line.first) => 0,
            _ => line.width as isize - span.from as isize,
        };
        offsets.insert(line.first, offset);
    }
    Some(Layout {
        offsets,
        frames,
        containers: nested,
        widths,
    })
}

/// The container a node makes, if any.
fn container<'t>(
    node: SyntaxNode<'t>,
    continued: &[TextSize],
    after_chains: &[TextSize],
) -> Option<Container<'t>> {
    // A lambda body in braces is no region, since its indentation doesn't
    // matter, but its lines are indented from its arrow all the same.
    let lambda_body = node.kind() == SyntaxKind::IndentedBlock
        && node
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::LambdaExpression);
    // A typed lambda in braces, `{ x: A =>`, holds its statements itself,
    // with no block around them: the lambda is their container.
    let typed_lambda_body = braced_lambda_statements(node);
    let typed_lambda = typed_lambda_body.is_some();
    if node.is_indentation_region() || lambda_body || typed_lambda {
        let statements: Vec<_> = if let Some(statements) = typed_lambda_body {
            statements
        } else if lambda_body {
            node.non_trivia_children()
                .filter(|c| c.kind().is_named() && c.kind() != SyntaxKind::EndMarker)
                .collect()
        } else {
            node.region_statements().collect()
        };
        let first = first_token(statements.first()?)?;
        let opener = std::iter::successors(first.prev_token(), SyntaxToken::prev_token)
            .find(SyntaxToken::owns_trivia)?;
        // From after the opener, which an `extension`'s range includes, to
        // its last token: the node can end at the next line's first token.
        let last = node.last_code_token()?;
        let after_chain = after_chains
            .binary_search(&opener.text_range().start())
            .is_ok();
        let (frame, start) = match definition_start(node).filter(|_| !after_chain) {
            Some(start) => ((node.parent()?, Frame::AfterLeading), start),
            // An extension's range takes in its opener.
            None if node.text_range().start() <= opener.text_range().start() => {
                ((node, Frame::AfterLeading), opener)
            }
            None => match (!after_chain)
                .then(|| owner_frame(node, opener, continued))
                .flatten()
            {
                Some(frame) => (frame, opener),
                None => {
                    let (owner, from) = opener_frame(node, opener)?;
                    let frame = Frame::At(from.text_range().start(), last.text_range().end());
                    ((owner, frame), first_token(&from).unwrap_or(opener))
                }
            },
        };
        let inner = TextRange::new(opener.text_range().end(), last.text_range().end());
        let braced = typed_lambda || (lambda_body && !node.is_indentation_region());
        return Some(Container {
            node,
            frame: (start.text_range().start() < inner.start()).then_some(frame),
            inner,
            statements,
            opener: Some(start),
            cases: node.kind() == SyntaxKind::IndentedCases,
            braced,
            piece: None,
        });
    }
    let mut children: Vec<_> = node.non_trivia_children().collect();
    // A match type's braces come after its scrutinee and `match`.
    if node.kind() == SyntaxKind::MatchType {
        let open = children.iter().position(|c| c.kind() == T!["{"])?;
        children.drain(..open);
    }
    let (NodeOrToken::Token(open), NodeOrToken::Token(close)) =
        (children.first()?, children.last()?)
    else {
        return None;
    };
    if open.kind() != T!["{"] || close.kind() != T!["}"] {
        return None;
    }
    let statements = children[1..children.len() - 1]
        .iter()
        .filter(|c| c.kind().is_named())
        .copied()
        .collect();
    let (frame, start) = match definition_start(node) {
        Some(start) => ((node.parent()?, Frame::AfterLeading), start),
        None => {
            let own = if node.first_token() == Some(*open) {
                (node, Frame::AfterLeading)
            } else {
                (
                    node,
                    Frame::At(open.text_range().start(), close.text_range().end()),
                )
            };
            let after_chain = after_chains.binary_search(&open.text_range().start()).is_ok();
            let owner = (!after_chain)
                .then(|| owner_frame(node, *open, continued))
                .flatten();
            (owner.unwrap_or(own), *open)
        }
    };
    Some(Container {
        node,
        frame: Some(frame),
        inner: TextRange::new(open.text_range().end(), close.text_range().start()),
        statements,
        opener: Some(start),
        cases: false,
        braced: true,
        piece: None,
    })
}

/// For a container in braces whose statements don't share a width, its
/// statements in runs at one width, if each starts its own line (or follows
/// a `;` on the line before it) and none is deeper than the one before it.
/// In braces a statement's width doesn't change what it means, and one no
/// deeper than the statement before it can't be that statement's
/// continuation misread by tree-sitter-scala, so the runs can be placed
/// together. `None` for any other container.
fn width_runs<'t>(container: &Container<'t>) -> Option<Vec<Vec<SyntaxElement<'t>>>> {
    if !container.braced || statement_indent(container).is_some() {
        return None;
    }
    let mut runs: Vec<(usize, Vec<SyntaxElement<'t>>)> = Vec::new();
    for statement in &container.statements {
        let first = first_token(statement)?;
        // As in `statement_indent`.
        if first.text() == "end" {
            return None;
        }
        if !first.is_first_on_line() {
            let after_semicolon =
                std::iter::successors(first.prev_token(), SyntaxToken::prev_token)
                    .find(SyntaxToken::owns_trivia)
                    .is_some_and(|t| t.text() == ";");
            if !after_semicolon {
                return None;
            }
            runs.last_mut()?.1.push(*statement);
            continue;
        }
        let width = first.indentation().len();
        match runs.last_mut() {
            Some((at, run)) if *at == width => run.push(*statement),
            Some((at, _)) if *at < width => return None,
            _ => runs.push((width, vec![*statement])),
        }
    }
    (runs.len() > 1).then(|| runs.into_iter().map(|(_, run)| run).collect())
}

/// The container split into one container for each run of its statements:
/// each from the end of the run before it, the last to where the container
/// ends, so comments go with the statements after them.
fn pieces<'t>(
    container: &Container<'t>,
    runs: Vec<Vec<SyntaxElement<'t>>>,
    group: usize,
) -> Vec<Container<'t>> {
    let count = runs.len();
    let mut start = container.inner.start();
    runs.into_iter()
        .enumerate()
        .map(|(i, statements)| {
            let last = i + 1 == count;
            let end = match statements.last() {
                _ if last => container.inner.end(),
                Some(NodeOrToken::Node(node)) => node
                    .last_code_token()
                    .map_or(node.text_range().end(), |t| t.text_range().end()),
                Some(NodeOrToken::Token(token)) => token.text_range().end(),
                None => start,
            };
            let inner = TextRange::new(start, end);
            start = end;
            Container {
                node: container.node,
                frame: container.frame,
                inner,
                statements,
                opener: container.opener,
                cases: container.cases,
                braced: true,
                piece: Some(Piece { group, last }),
            }
        })
        .collect()
}

/// The statements after the arrow of a lambda in braces, or nested in one
/// as `{ x: A => y: B =>`, when they aren't in an indented block. `None`
/// for any other node, or a lambda without statements of its own.
fn braced_lambda_statements(node: SyntaxNode<'_>) -> Option<Vec<SyntaxElement<'_>>> {
    if node.kind() != SyntaxKind::LambdaExpression {
        return None;
    }
    let braced = std::iter::successors(node.parent(), SyntaxNode::parent)
        .find(|p| p.kind() != SyntaxKind::LambdaExpression)
        .is_some_and(|p| p.kind() == SyntaxKind::Block);
    if !braced {
        return None;
    }
    let statements: Vec<_> = node
        .non_trivia_children()
        .skip_while(
            |c| !matches!(c, NodeOrToken::Token(t) if matches!(t.text(), "=>" | "\u{21D2}")),
        )
        .skip(1)
        .filter(|c| c.kind().is_named())
        .collect();
    match statements.as_slice() {
        [] => None,
        [NodeOrToken::Node(n)]
            if matches!(
                n.kind(),
                SyntaxKind::IndentedBlock | SyntaxKind::LambdaExpression
            ) =>
        {
            None
        }
        _ => Some(statements),
    }
}

/// The lines that print on the line before them unless a rule breaks them
/// to fit, by their first character, in order: the lines a boolean chain
/// joins, and the lines inside a relaid list, through its `)`.
fn continued_lines(firsts: &[TextSize], relaid: &Relaid) -> Vec<TextSize> {
    // How many lists each line is inside, from where they open and close.
    let mut edges: Vec<(TextSize, isize)> = Vec::new();
    for &(open, close) in &relaid.lists {
        edges.push((open + TextSize::from(1), 1));
        edges.push((close + TextSize::from(1), -1));
    }
    edges.sort();
    let mut continued = Vec::new();
    let (mut depth, mut next) = (0, 0);
    for &first in firsts {
        while edges.get(next).is_some_and(|&(at, _)| at <= first) {
            depth += edges[next].1;
            next += 1;
        }
        if depth > 0
            || relaid.joined.binary_search(&first).is_ok()
            || relaid.continued.binary_search(&first).is_ok()
        {
            continued.push(first);
        }
    }
    continued
}

/// For a region or a block in braces that isn't a definition's body, a
/// frame from the start of what it belongs to, such as an `if`, a lambda
/// or a call, if that's on the opener's line: a rule that breaks the line
/// before the opener, as in a long condition, doesn't move the body. `None`
/// if it starts on an earlier line, as for an `else` on a line of its own,
/// so the frame starts at the opener. Lines that print on the line before
/// them unless a rule breaks them to fit, `continued`, count as on it.
fn owner_frame<'t>(
    node: SyntaxNode<'t>,
    opener: SyntaxToken<'t>,
    continued: &[TextSize],
) -> Option<(SyntaxNode<'t>, Frame)> {
    let owner = std::iter::successors(node.parent(), SyntaxNode::parent).find(|n| {
        n.tokens()
            .find(SyntaxToken::owns_trivia)
            .is_some_and(|t| t.text_range().start() < opener.text_range().start())
    })?;
    let start = owner.tokens().find(SyntaxToken::owns_trivia)?;
    let base = owner.text_range().start();
    let from = usize::from(start.text_range().start() - base);
    let between = &owner.text()[from..usize::from(opener.text_range().start() - base)];
    // Each line break, unless the line after it is continued.
    let separate = between.match_indices('\n').any(|(i, _)| {
        let rest = &between[i + 1..];
        let indent = rest.len() - rest.trim_start_matches([' ', '\t', '\r']).len();
        TextSize::try_from(from + i + 1 + indent)
            .is_ok_and(|at| continued.binary_search(&(base + at)).is_err())
    });
    (!separate).then_some((owner, Frame::AfterLeading))
}

/// For a region framed from its opener, the node around both and its child
/// holding the opener, where the frame starts: the condition `(c)` for the
/// `)` before an `if`'s body, so the body is placed from the line the
/// condition starts on, not the one it ends on.
fn opener_frame<'t>(
    node: SyntaxNode<'t>,
    opener: SyntaxToken<'t>,
) -> Option<(SyntaxNode<'t>, SyntaxElement<'t>)> {
    let mut child = NodeOrToken::Token(opener);
    loop {
        let parent = child.parent()?;
        if parent.text_range().contains_range(node.text_range()) {
            return Some((parent, child));
        }
        child = NodeOrToken::Node(parent);
    }
}

/// For the body of a definition, a class body or `case` clauses, the first
/// token of what it belongs to. A body is indented from that first line, as in scalafmt, even
/// when the header goes on over more lines: `extends` and `with` clauses, a
/// result type, or parameters.
fn definition_start(body: SyntaxNode<'_>) -> Option<SyntaxToken<'_>> {
    let owner = body.parent()?;
    // Also `case` clauses, from the start of their `match`, which may be on
    // an earlier line than the `match` keyword.
    // Not a colon argument's cases, which start after their `:`.
    let starts_owner = match body.kind() {
        SyntaxKind::TemplateBody | SyntaxKind::EnumBody | SyntaxKind::WithTemplateBody => true,
        SyntaxKind::IndentedCases => owner.kind() != SyntaxKind::ColonArgument,
        _ => false,
    };
    if !starts_owner && !owner.kind().name().ends_with("_definition") {
        return None;
    }
    owner.tokens().find(SyntaxToken::owns_trivia)
}

/// The indentation the container's statements share, if each starts its own
/// line at the same width, or follows a `;` on the line before it. `None`
/// for a container whose layout is left to the one around it, including one
/// with no statements.
fn statement_indent(container: &Container<'_>) -> Option<usize> {
    let mut indent = None;
    for statement in &container.statements {
        let first = first_token(statement)?;
        // A statement after a `;` on the line before it moves with that line.
        let after_semicolon = indent.is_some()
            && std::iter::successors(first.prev_token(), SyntaxToken::prev_token)
                .find(SyntaxToken::owns_trivia)
                .is_some_and(|t| t.text() == ";");
        if after_semicolon && !first.is_first_on_line() {
            continue;
        }
        // tree-sitter-scala reads `end Name` after a body holding only a
        // comment as a postfix call. Moving it would make it close something
        // else, and the tree wouldn't show it.
        if !first.is_first_on_line() || first.text() == "end" {
            return None;
        }
        let width = first.indentation().len();
        if *indent.get_or_insert(width) != width {
            return None;
        }
    }
    indent
}

fn first_token<'t>(element: &SyntaxElement<'t>) -> Option<SyntaxToken<'t>> {
    match element {
        NodeOrToken::Token(token) => Some(*token).filter(SyntaxToken::owns_trivia),
        NodeOrToken::Node(node) => node.tokens().find(SyntaxToken::owns_trivia),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lossless_tree::parse;

    /// The source with each line moved to its new indentation.
    fn reindented(source: &str) -> String {
        let tree = parse(source);
        let reindent = Layout::new(&tree, &Relaid::default());
        let mut out = String::new();
        let mut start = 0;
        for line in source.split_inclusive('\n') {
            let content = line.trim_start_matches(' ');
            let first = TextSize::try_from(start + line.len() - content.len()).unwrap();
            match reindent.width(first) {
                Some(width) => {
                    out.push_str(&" ".repeat(width));
                    out.push_str(content);
                }
                None => out.push_str(line),
            }
            start += line.len();
        }
        out
    }

    #[test]
    fn braces_get_two_spaces_per_level() {
        let source = "object A {\n    def f = {\n            1\n    }\n}\n";
        assert_eq!(
            reindented(source),
            "object A {\n  def f = {\n    1\n  }\n}\n"
        );
    }

    #[test]
    fn continuation_lines_move_with_their_statement() {
        let source = "object A {\n    val x = f(\n        a,\n        b\n    )\n}\n";
        assert_eq!(
            reindented(source),
            "object A {\n  val x = f(\n      a,\n      b\n  )\n}\n"
        );
    }

    #[test]
    fn scala3_regions_are_placed_from_their_opener() {
        let source = "object A:\n    def f =\n        1\n";
        assert_eq!(reindented(source), "object A:\n  def f =\n    1\n");
    }

    #[test]
    fn already_formatted_code_stays() {
        let source = "object A {\n  def f = {\n    g(\n      a\n    )\n  }\n}\n";
        assert_eq!(Layout::new(&parse(source), &Relaid::default()).len(), 0);
    }

    #[test]
    fn a_continuation_after_a_body_stays_outside_it() {
        // Moving the body to 4 would put `.map` inside it.
        let source = "object O:\n  val q = xs.filter: x =>\n      x > 1\n    .map: x =>\n      x\n";
        let out = reindented(source);
        assert!(out.contains("\n      x > 1\n    .map"), "{out}");
    }

    #[test]
    fn an_end_marker_read_as_a_statement_stays() {
        let source = "trait A:\n//\nend A\n";
        assert_eq!(Layout::new(&parse(source), &Relaid::default()).len(), 0);
    }

    #[test]
    fn mixed_statement_indentation_moves_with_the_parent() {
        // The inner block's statements disagree, so it keeps its shape and
        // moves with the object's body.
        let source = "object A {\n    def f = {\n        1\n          2\n    }\n}\n";
        assert_eq!(
            reindented(source),
            "object A {\n  def f = {\n      1\n        2\n  }\n}\n"
        );
    }
}
