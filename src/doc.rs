//! A Wadler-style pretty-printing document, in the spirit of Prettier's
//! `Doc`.
//!
//! A formatter describes output as a [`Doc`]: text, places where a line may
//! break, groups that should stay on one line if they fit, and indentation.
//! [`print`] then picks the line breaks for a given width. Each group is
//! printed flat (every `line` becomes a space, every `softline` nothing) if
//! the group and the rest of its line fit, and broken (every `line` and
//! `softline` becomes a newline) otherwise. Groups decide outermost first,
//! so an outer group can break while the groups inside it stay flat.
//!
//! ```
//! use scruff::doc::{Doc, PrintOptions, print};
//!
//! let args = Doc::join(["first", "second", "third"].map(Doc::text), Doc::text(",") + Doc::line());
//! let call = Doc::text("f(") + (Doc::softline() + args).nest() + Doc::softline() + Doc::text(")");
//! let doc = call.group();
//! assert_eq!(print(&doc, &PrintOptions::default()), "f(first, second, third)");
//! let narrow = PrintOptions { max_width: 10, ..PrintOptions::default() };
//! assert_eq!(print(&doc, &narrow), "f(\n  first,\n  second,\n  third\n)");
//! ```

use std::borrow::Cow;
use std::ops::Add;

/// A document to print. Text can borrow from the source being formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doc<'a> {
    Nil,
    /// Text without line breaks.
    Text(Cow<'a, str>),
    /// Text printed exactly as given. It may contain line breaks; lines
    /// after the first are not indented. A line break forces the enclosing
    /// groups to break.
    Verbatim(Cow<'a, str>),
    /// Text printed as given, except that its lines after the first move
    /// with the first: it was written at `column`, and if it's printed
    /// elsewhere, each later line moves by as much. For a multi-line
    /// comment, whose ` *` lines should stay under its `/**`.
    Relative {
        text: Cow<'a, str>,
        column: usize,
    },
    Line(LineKind),
    Concat(Vec<Doc<'a>>),
    /// Indents the lines that start inside it by one level.
    Nest(Box<Doc<'a>>),
    /// Indents the lines that start inside it by this many more columns, or
    /// fewer if negative, but never past the start of the line.
    Offset(isize, Box<Doc<'a>>),
    /// Indents the lines that start inside it from the indentation of the
    /// line it starts on, instead of from the enclosing nesting. For laying
    /// out part of a line whose indentation came from elsewhere, such as
    /// copied from the source.
    FromLineIndent(Box<Doc<'a>>),
    /// Printed flat if it fits, broken otherwise.
    Group(Box<Doc<'a>>),
    /// A group that breaks only when it must: printed flat if it fits, as
    /// any group, but the groups before it on its line don't count on it
    /// breaking. When they measure the rest of their line, it counts as
    /// flat, so they break first. For type arguments in a result type, so a
    /// parameter list before them breaks instead.
    ReluctantGroup(Box<Doc<'a>>),
    /// `flat` and then `body`, unless `body` would start more than `limit`
    /// columns past its line's indentation, or its first line, as
    /// `first_line` measures it, wouldn't fit: then `broken` and `body`. Each way, the lines inside move by its
    /// shift. For a definition's body, as `def f(x: A): B = x match`, which
    /// starts the next line after a long header. The printer decides from
    /// where it is, so the source's layout doesn't matter.
    StartsPast {
        limit: usize,
        broken: Box<Doc<'a>>,
        flat: Box<Doc<'a>>,
        body: Box<Doc<'a>>,
        broken_shift: Shift,
        flat_shift: Shift,
        first_line: FirstLine,
    },
    /// Prints `broken` if the enclosing group breaks, `flat` if not.
    IfBreak {
        broken: Box<Doc<'a>>,
        flat: Box<Doc<'a>>,
    },
    /// Printed at the end of the current line, just before the next line
    /// break, such as a trailing `// comment`. Takes no width.
    LineSuffix(Box<Doc<'a>>),
    /// Forces the enclosing groups to break. Use with a line suffix that
    /// must end its line, such as a `//` comment.
    BreakParent,
    /// Rows, each starting its own line, whose tails line up where they
    /// fit. See [`AlignPart`].
    Align(Vec<AlignPart<'a>>),
    /// `body`, then `marker` on a line of its own if `body` printed over
    /// at least `min_breaks` line breaks, counted from its start or from
    /// the last [`Doc::CountFrom`] in it. The marker goes at the
    /// indentation of the line `body` starts on, and counts toward the
    /// line breaks of an end marker around it. For Scala 3's `end f`,
    /// which a long block gets and a short one doesn't.
    EndMarker {
        body: Box<Doc<'a>>,
        marker: Box<Doc<'a>>,
        min_breaks: usize,
    },
    /// Where the innermost [`Doc::EndMarker`] around it starts counting
    /// line breaks: at its last block, which the marker closes as a `}`
    /// would, so `end if` doesn't follow a short `else`. Prints nothing.
    CountFrom,
}

/// What of a [`Doc::StartsPast`] body must fit on its line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstLine {
    /// Its first line, through its first hard line break, with its groups
    /// flat: a `match` starts the next line sooner than break its
    /// scrutinee.
    Flat,
    /// What's before its first place to break, as `f(`: a call breaks its
    /// arguments sooner than start the next line.
    ToBreak,
    /// All of it, with its groups flat, on its line or else on the next:
    /// a call of plain arguments, as `f(a, b)`, starts the next line
    /// sooner than break them. If it fits on neither, as
    /// [`FirstLine::ToBreak`].
    Whole,
}

/// How far the lines inside a [`Doc::StartsPast`] move: levels of
/// indentation and columns, either negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shift {
    pub levels: isize,
    pub columns: isize,
}

/// A part of [`Doc::Align`].
///
/// A run is rows in a row, each `adjacent` to the one before, starting at
/// the same column. Within a run, the tails start at one column, after the
/// widest head, while every row's first line stays in the width; a row that
/// would push one over starts the next group. A row whose tail doesn't fit
/// on its line, so breaks, aligns its first line and ends the run. A row
/// that can't be aligned at all ends the run too: its head has a line
/// break, or its first line is too long even unpadded.
///
/// What a row's line is measured with counts a trailing `//` comment, a
/// line suffix, which the printer otherwise leaves out: a row isn't padded
/// into a comment that then runs past the width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlignPart<'a> {
    /// `before`, which ends in the line break and indentation that start
    /// the row's line, then `head`, padding, and `tail`.
    Row {
        before: Doc<'a>,
        head: Doc<'a>,
        tail: Doc<'a>,
        /// Whether the row can continue the run of the row before it.
        adjacent: bool,
    },
    /// Anything else, which ends the run.
    Other(Doc<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// A space when flat, a line break when broken.
    Line,
    /// Nothing when flat, a line break when broken.
    Soft,
    /// Always a line break; forces the enclosing groups to break.
    Hard,
}

impl<'a> Doc<'a> {
    /// Text without line breaks. Use [`Doc::verbatim`] for text that has them.
    pub fn text(text: impl Into<Cow<'a, str>>) -> Doc<'a> {
        let text = text.into();
        debug_assert!(
            !text.contains(['\n', '\r']),
            "Doc::text with a line break: {text:?}"
        );
        Doc::Text(text)
    }

    pub fn verbatim(text: impl Into<Cow<'a, str>>) -> Doc<'a> {
        Doc::Verbatim(text.into())
    }

    pub fn line() -> Doc<'a> {
        Doc::Line(LineKind::Line)
    }

    pub fn softline() -> Doc<'a> {
        Doc::Line(LineKind::Soft)
    }

    pub fn hardline() -> Doc<'a> {
        Doc::Line(LineKind::Hard)
    }

    pub fn concat(docs: impl IntoIterator<Item = Doc<'a>>) -> Doc<'a> {
        Doc::Concat(docs.into_iter().collect())
    }

    /// The docs with `separator` between each pair.
    pub fn join(docs: impl IntoIterator<Item = Doc<'a>>, separator: Doc<'a>) -> Doc<'a> {
        let mut parts = Vec::new();
        for (i, doc) in docs.into_iter().enumerate() {
            if i > 0 {
                parts.push(separator.clone());
            }
            parts.push(doc);
        }
        Doc::Concat(parts)
    }

    pub fn nest(self) -> Doc<'a> {
        Doc::Nest(Box::new(self))
    }

    pub fn offset(self, columns: isize) -> Doc<'a> {
        match columns {
            0 => self,
            _ => Doc::Offset(columns, Box::new(self)),
        }
    }

    pub fn from_line_indent(self) -> Doc<'a> {
        Doc::FromLineIndent(Box::new(self))
    }

    pub fn group(self) -> Doc<'a> {
        Doc::Group(Box::new(self))
    }

    pub fn reluctant_group(self) -> Doc<'a> {
        Doc::ReluctantGroup(Box::new(self))
    }

    pub fn if_break(broken: Doc<'a>, flat: Doc<'a>) -> Doc<'a> {
        Doc::IfBreak {
            broken: Box::new(broken),
            flat: Box::new(flat),
        }
    }

    /// See [`Doc::StartsPast`].
    pub fn starts_past(
        limit: usize,
        (broken, broken_shift): (Doc<'a>, Shift),
        (flat, flat_shift): (Doc<'a>, Shift),
        body: Doc<'a>,
        first_line: FirstLine,
    ) -> Doc<'a> {
        Doc::StartsPast {
            limit,
            broken: Box::new(broken),
            flat: Box::new(flat),
            body: Box::new(body),
            broken_shift,
            flat_shift,
            first_line,
        }
    }

    pub fn line_suffix(self) -> Doc<'a> {
        Doc::LineSuffix(Box::new(self))
    }

    pub fn break_parent() -> Doc<'a> {
        Doc::BreakParent
    }

    pub fn count_from() -> Doc<'a> {
        Doc::CountFrom
    }

    /// This document, then `marker` on a line of its own if this one prints
    /// over at least `min_breaks` line breaks. See [`Doc::EndMarker`].
    pub fn end_marker(self, marker: impl Into<Cow<'a, str>>, min_breaks: usize) -> Doc<'a> {
        Doc::EndMarker {
            body: Box::new(self),
            marker: Box::new(Doc::text(marker)),
            min_breaks,
        }
    }

    /// The width of this document up to its first place a line can break,
    /// printed flat until there: what's left on a line when it breaks as
    /// early as it can. A line suffix takes no width.
    fn width_to_first_break(&self) -> usize {
        fn walk(doc: &Doc<'_>, width: &mut usize) -> bool {
            match doc {
                Doc::Nil | Doc::LineSuffix(_) | Doc::BreakParent | Doc::CountFrom => false,
                Doc::Text(text) => {
                    *width += self::width(text);
                    false
                }
                Doc::Verbatim(text) | Doc::Relative { text, .. } => match text.find(['\n', '\r']) {
                    Some(i) => {
                        *width += self::width(&text[..i]);
                        true
                    }
                    None => {
                        *width += self::width(text);
                        false
                    }
                },
                Doc::Line(_) => true,
                Doc::Concat(parts) => parts.iter().any(|part| walk(part, width)),
                Doc::Nest(inner)
                | Doc::Offset(_, inner)
                | Doc::FromLineIndent(inner)
                | Doc::Group(inner)
                | Doc::ReluctantGroup(inner)
                | Doc::EndMarker { body: inner, .. } => walk(inner, width),
                Doc::IfBreak { flat, .. } => walk(flat, width),
                Doc::StartsPast { flat, body, .. } => walk(flat, width) || walk(body, width),
                Doc::Align(parts) => parts
                    .iter()
                    .any(|part| part.docs().iter().any(|doc| walk(doc, width))),
            }
        }
        let mut width = 0;
        walk(self, &mut width);
        width
    }

    /// The width of this document printed on one line, or `None` if it
    /// can't be: it has a hard line break, or text with one. A line suffix,
    /// such as a trailing `//` comment, takes no width.
    fn flat_width(&self) -> Option<usize> {
        self.flat_width_counting(false)
    }

    /// [`Doc::flat_width`] with line suffixes counted too: the whole line as
    /// it reads.
    fn full_flat_width(&self) -> Option<usize> {
        self.flat_width_counting(true)
    }

    fn flat_width_counting(&self, suffixes: bool) -> Option<usize> {
        match self {
            Doc::LineSuffix(inner) if suffixes => inner.flat_width_counting(true),
            Doc::Nil | Doc::LineSuffix(_) | Doc::CountFrom => Some(0),
            Doc::Text(text) => Some(width(text)),
            Doc::Verbatim(text) | Doc::Relative { text, .. } if text.contains(['\n', '\r']) => None,
            Doc::Verbatim(text) | Doc::Relative { text, .. } => Some(width(text)),
            Doc::Line(LineKind::Line) => Some(1),
            Doc::Line(LineKind::Soft) => Some(0),
            Doc::Line(LineKind::Hard) | Doc::BreakParent => None,
            Doc::Concat(parts) => parts
                .iter()
                .map(|part| part.flat_width_counting(suffixes))
                .sum(),
            Doc::Nest(inner)
            | Doc::Offset(_, inner)
            | Doc::FromLineIndent(inner)
            | Doc::Group(inner)
            | Doc::ReluctantGroup(inner)
            | Doc::EndMarker { body: inner, .. } => inner.flat_width_counting(suffixes),
            Doc::IfBreak { flat, .. } => flat.flat_width_counting(suffixes),
            Doc::StartsPast { flat, body, .. } => {
                Some(flat.flat_width_counting(suffixes)? + body.flat_width_counting(suffixes)?)
            }
            Doc::Align(parts) => parts
                .iter()
                .flat_map(AlignPart::docs)
                .map(|doc| doc.flat_width_counting(suffixes))
                .sum(),
        }
    }
}

impl<'a> AlignPart<'a> {
    /// Its documents in order, unpadded.
    fn docs(&self) -> Vec<&Doc<'a>> {
        match self {
            AlignPart::Row {
                before, head, tail, ..
            } => vec![before, head, tail],
            AlignPart::Other(doc) => vec![doc],
        }
    }
}

/// `a + b` concatenates, flattening nested concatenations.
impl<'a> Add for Doc<'a> {
    type Output = Doc<'a>;

    fn add(self, rhs: Doc<'a>) -> Doc<'a> {
        let mut parts = match self {
            Doc::Concat(parts) => parts,
            Doc::Nil => Vec::new(),
            other => vec![other],
        };
        match rhs {
            Doc::Concat(more) => parts.extend(more),
            Doc::Nil => {}
            other => parts.push(other),
        }
        Doc::Concat(parts)
    }
}

impl<'a> From<&'a str> for Doc<'a> {
    fn from(text: &'a str) -> Doc<'a> {
        Doc::text(text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintOptions {
    /// Lines longer than this, in characters, make groups break. Defaults
    /// to 100, sbt's scalafmt `maxColumn`.
    pub max_width: usize,
    /// One level of indentation.
    pub indent: String,
    /// What a line break prints as, such as `"\n"` or `"\r\n"`.
    pub line_ending: String,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            max_width: 100,
            indent: "  ".to_string(),
            line_ending: "\n".to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Flat,
    Break,
}

#[derive(Clone, Copy)]
struct Command<'d, 'a> {
    /// Index into the base indentations, which come before `indent` levels.
    base: usize,
    indent: usize,
    /// Columns more, or fewer, than `indent` levels.
    offset: isize,
    mode: Mode,
    doc: Item<'d, 'a>,
}

/// What a command prints.
#[derive(Clone, Copy)]
enum Item<'d, 'a> {
    Doc(&'d Doc<'a>),
    /// Spaces that line up a [`Doc::Align`] row.
    Padding(usize),
    /// The end of a [`Doc::EndMarker`]'s body: its marker, printed if the
    /// body took at least `min_breaks` line breaks since it started
    /// counting.
    EndMarker {
        marker: &'d Doc<'a>,
        min_breaks: usize,
    },
}

/// The line break before a [`Doc::EndMarker`]'s marker.
static HARD_LINE: Doc<'static> = Doc::Line(LineKind::Hard);

/// The line breaks in printed text.
fn line_breaks(text: &str) -> usize {
    text.bytes().filter(|&b| b == b'\n').count()
}

fn width(text: &str) -> usize {
    text.chars().count()
}

/// The spaces and tabs that start the last line of `out`.
fn line_indentation(out: &str) -> &str {
    let line = &out[out.rfind(['\n', '\r']).map_or(0, |i| i + 1)..];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

/// Lays out `doc` within `options.max_width` where it can. Lines never end
/// in spaces or tabs, unless verbatim text put them there.
pub fn print(doc: &Doc<'_>, options: &PrintOptions) -> String {
    let indent_width = width(&options.indent);
    let mut out = String::new();
    let mut column = 0;
    // The base indentation of each `FromLineIndent`, after none at all.
    let mut bases = vec![String::new()];
    let mut commands = vec![Command {
        base: 0,
        indent: 0,
        offset: 0,
        mode: Mode::Break,
        doc: Item::Doc(doc),
    }];
    let mut suffixes: Vec<Command<'_, '_>> = Vec::new();
    // Trimming trailing spaces must not reach back into verbatim text.
    let mut verbatim_end = 0;
    // Line breaks printed so far, and for each end marker being printed,
    // innermost last, how many there were when it started counting.
    let mut breaks = 0;
    let mut counting_since: Vec<usize> = Vec::new();

    while let Some(command) = commands.pop() {
        let Command {
            base,
            indent,
            offset,
            mode,
            doc,
        } = command;
        // The width of a new line's indentation here.
        let line_indent = (width(&bases[base]) as isize + (indent * indent_width) as isize + offset)
            .max(0) as usize;
        let doc = match doc {
            Item::Padding(spaces) => {
                out.push_str(&" ".repeat(spaces));
                column += spaces;
                continue;
            }
            Item::EndMarker { marker, min_breaks } => {
                let since = counting_since.pop().unwrap_or(breaks);
                if breaks - since >= min_breaks {
                    // Its own line, at the body's first line's indentation.
                    let marker_line = Command {
                        doc: Item::Doc(marker),
                        ..command
                    };
                    commands.push(marker_line);
                    commands.push(Command {
                        doc: Item::Doc(&HARD_LINE),
                        ..command
                    });
                }
                continue;
            }
            Item::Doc(doc) => doc,
        };
        // A command for a part of this one, with its indentation and mode.
        let part = |doc| Command {
            base,
            indent,
            offset,
            mode,
            doc: Item::Doc(doc),
        };
        match doc {
            Doc::Nil | Doc::BreakParent => {}
            Doc::Text(text) => {
                out.push_str(text);
                column += width(text);
            }
            Doc::Verbatim(text) => {
                out.push_str(text);
                verbatim_end = out.len();
                breaks += line_breaks(text);
                column = match text.rfind(['\n', '\r']) {
                    Some(i) => width(&text[i + 1..]),
                    None => column + width(text),
                };
            }
            Doc::Relative { text, column: from } => {
                let shift = column as isize - *from as isize;
                let mut moved = String::with_capacity(text.len());
                for (i, line) in text.split_inclusive('\n').enumerate() {
                    if i == 0 || line.trim().is_empty() || shift == 0 {
                        moved.push_str(line);
                    } else if shift > 0 {
                        moved.push_str(&" ".repeat(shift as usize));
                        moved.push_str(line);
                    } else {
                        let spaces = line.len() - line.trim_start_matches(' ').len();
                        moved.push_str(&line[spaces.min(shift.unsigned_abs())..]);
                    }
                }
                out.push_str(&moved);
                verbatim_end = out.len();
                breaks += line_breaks(&moved);
                column = match moved.rfind(['\n', '\r']) {
                    Some(i) => width(&moved[i + 1..]),
                    None => column + width(&moved),
                };
            }
            Doc::Concat(parts) => commands.extend(parts.iter().rev().map(part)),
            Doc::Offset(columns, inner) => commands.push(Command {
                offset: offset + columns,
                ..part(inner)
            }),
            Doc::Nest(inner) => commands.push(Command {
                indent: indent + 1,
                ..part(inner)
            }),
            Doc::FromLineIndent(inner) => {
                bases.push(line_indentation(&out).to_string());
                commands.push(Command {
                    base: bases.len() - 1,
                    indent: 0,
                    offset: 0,
                    ..part(inner)
                });
            }
            Doc::EndMarker {
                body,
                marker,
                min_breaks,
            } => {
                bases.push(line_indentation(&out).to_string());
                commands.push(Command {
                    base: bases.len() - 1,
                    indent: 0,
                    offset: 0,
                    mode: Mode::Break,
                    doc: Item::EndMarker {
                        marker,
                        min_breaks: *min_breaks,
                    },
                });
                counting_since.push(breaks);
                commands.push(part(body));
            }
            Doc::CountFrom => {
                if let Some(since) = counting_since.last_mut() {
                    *since = breaks;
                }
            }
            Doc::Group(inner) | Doc::ReluctantGroup(inner) => {
                let flat = Command {
                    mode: Mode::Flat,
                    ..part(inner)
                };
                let room = options.max_width as isize - column as isize;
                let mode = if mode == Mode::Flat || fits(flat, &commands, room) {
                    Mode::Flat
                } else {
                    Mode::Break
                };
                commands.push(Command {
                    mode,
                    ..part(inner)
                });
            }
            Doc::IfBreak { broken, flat } => {
                commands.push(part(if mode == Mode::Break { broken } else { flat }));
            }
            Doc::StartsPast {
                limit,
                broken,
                flat,
                body,
                broken_shift,
                flat_shift,
                first_line,
            } => {
                let start = column + flat.flat_width().unwrap_or(0);
                let past = start > width(line_indentation(&out)).saturating_add(*limit);
                let room = options.max_width as isize - start as isize;
                let head_fits = || {
                    let head = Command {
                        mode: Mode::Break,
                        ..part(body)
                    };
                    fits(head, &commands, room)
                };
                let fits = match first_line {
                    FirstLine::Flat => first_line_fits(body, room),
                    FirstLine::ToBreak => head_fits(),
                    FirstLine::Whole => {
                        let flat = Command {
                            mode: Mode::Flat,
                            ..part(body)
                        };
                        // Where it would start on the next line.
                        let next_indent = (line_indent as isize
                            + broken_shift.levels * indent_width as isize
                            + broken_shift.columns)
                            .max(0) as usize;
                        let next_room = line_start_after(broken, next_indent, indent_width)
                            .map_or(-1, |start| options.max_width as isize - start as isize);
                        fits(flat, &commands, room)
                            || !fits(flat, &commands, next_room) && head_fits()
                    }
                };
                let (before, shift) = if past || !fits {
                    (broken, broken_shift)
                } else {
                    (flat, flat_shift)
                };
                let shifted = |doc| Command {
                    offset: offset + shift.levels * indent_width as isize + shift.columns,
                    ..part(doc)
                };
                commands.push(shifted(body));
                commands.push(shifted(before));
            }
            Doc::LineSuffix(inner) => suffixes.push(part(inner)),
            Doc::Align(parts) => {
                let padding = align(parts, line_indent, indent_width, options.max_width);
                for (align_part, pad) in parts.iter().zip(padding).rev() {
                    match align_part {
                        AlignPart::Row {
                            before, head, tail, ..
                        } => {
                            commands.push(part(tail));
                            commands.push(Command {
                                doc: Item::Padding(pad),
                                ..part(tail)
                            });
                            commands.push(part(head));
                            commands.push(part(before));
                        }
                        AlignPart::Other(doc) => commands.push(part(doc)),
                    }
                }
            }
            Doc::Line(kind) => match (mode, kind) {
                (Mode::Flat, LineKind::Soft) => {}
                (Mode::Flat, LineKind::Line) => {
                    out.push(' ');
                    column += 1;
                }
                _ => {
                    if !suffixes.is_empty() {
                        // Print pending suffixes first, then come back here.
                        commands.push(command);
                        commands.extend(suffixes.drain(..).rev());
                        continue;
                    }
                    let trimmed = out.trim_end_matches([' ', '\t']).len();
                    out.truncate(trimmed.max(verbatim_end));
                    out.push_str(&options.line_ending);
                    breaks += 1;
                    if offset == 0 {
                        out.push_str(&bases[base]);
                        for _ in 0..indent {
                            out.push_str(&options.indent);
                        }
                    } else {
                        out.push_str(&" ".repeat(line_indent));
                    }
                    column = line_indent;
                }
            },
        }
        if commands.is_empty() && !suffixes.is_empty() {
            commands.extend(suffixes.drain(..).rev());
        }
    }
    let trimmed = out.trim_end_matches([' ', '\t']).len();
    out.truncate(trimmed.max(verbatim_end));
    out
}

/// A [`Doc::Align`] row, measured.
struct Row {
    /// The column its head starts at.
    start: usize,
    /// Its head's width.
    head: usize,
    /// The width of its tail's first line.
    after: usize,
    /// Whether its tail goes on to later lines, which ends its run.
    multi_line: bool,
    adjacent: bool,
}

/// The padding before each part's tail, for parts printed on lines indented
/// `line_indent`. See [`AlignPart`] for the rules.
fn align(
    parts: &[AlignPart<'_>],
    line_indent: usize,
    indent_width: usize,
    max_width: usize,
) -> Vec<usize> {
    let rows: Vec<Option<Row>> = parts
        .iter()
        .map(|part| {
            let AlignPart::Row {
                before,
                head,
                tail,
                adjacent,
            } = part
            else {
                return None;
            };
            let start = line_start_after(before, line_indent, indent_width)?;
            let head = head.flat_width()?;
            // A tail too long for its line will break: what counts then is
            // its first line, up to its first place to break. If even that
            // doesn't fit, the head would have to break, so the row isn't
            // aligned. That way a row measures the same before and after
            // it's broken.
            match tail.flat_width() {
                Some(width) if start + head + width <= max_width => Some(Row {
                    start,
                    head,
                    after: tail.full_flat_width().unwrap_or(width),
                    multi_line: false,
                    adjacent: *adjacent,
                }),
                _ => {
                    let after = tail.width_to_first_break();
                    (start + head + after <= max_width).then_some(Row {
                        start,
                        head,
                        after,
                        multi_line: true,
                        adjacent: *adjacent,
                    })
                }
            }
        })
        .collect();

    let mut padding = vec![0; parts.len()];
    let mut group: Vec<usize> = Vec::new();
    let mut column = 0;
    for (i, row) in rows.iter().enumerate() {
        let Some(row) = row else {
            group.clear();
            continue;
        };
        let own = row.start + row.head;
        let wider = column.max(own);
        let continues = group.last().is_some_and(|&k| {
            rows[k]
                .as_ref()
                .is_some_and(|prev| !prev.multi_line && prev.start == row.start)
        });
        let fits = |k: &usize| {
            rows[*k]
                .as_ref()
                .is_some_and(|r| wider + r.after <= max_width)
        };
        if continues && row.adjacent && group.iter().chain(std::iter::once(&i)).all(fits) {
            column = wider;
            group.push(i);
        } else {
            group = vec![i];
            column = own;
        }
        for &k in &group {
            if let Some(r) = &rows[k] {
                padding[k] = column - (r.start + r.head);
            }
        }
    }
    padding
}

/// The column of the line `doc` ends on, printed broken after its last line
/// break, for a doc nested on lines indented `line_indent`. `None` if it has
/// no line break.
fn line_start_after(doc: &Doc<'_>, line_indent: usize, indent_width: usize) -> Option<usize> {
    fn walk(doc: &Doc<'_>, line_indent: usize, indent_width: usize, column: &mut Option<usize>) {
        match doc {
            Doc::Nil | Doc::BreakParent | Doc::LineSuffix(_) | Doc::CountFrom => {}
            Doc::Text(text) => {
                if let Some(c) = column {
                    *c += width(text);
                }
            }
            Doc::Verbatim(text) | Doc::Relative { text, .. } => match text.rfind(['\n', '\r']) {
                Some(i) => *column = Some(width(&text[i + 1..])),
                None => {
                    if let Some(c) = column {
                        *c += width(text);
                    }
                }
            },
            Doc::Line(_) => *column = Some(line_indent),
            Doc::Concat(parts) => {
                for part in parts {
                    walk(part, line_indent, indent_width, column);
                }
            }
            Doc::Nest(inner) => walk(inner, line_indent + indent_width, indent_width, column),
            Doc::Offset(columns, inner) => {
                let line_indent = (line_indent as isize + columns).max(0) as usize;
                walk(inner, line_indent, indent_width, column);
            }
            Doc::FromLineIndent(inner)
            | Doc::Group(inner)
            | Doc::ReluctantGroup(inner)
            | Doc::EndMarker { body: inner, .. } => {
                walk(inner, line_indent, indent_width, column);
            }
            Doc::IfBreak { broken, .. } => walk(broken, line_indent, indent_width, column),
            Doc::StartsPast { flat, body, .. } => {
                walk(flat, line_indent, indent_width, column);
                walk(body, line_indent, indent_width, column);
            }
            Doc::Align(parts) => {
                for doc in parts.iter().flat_map(AlignPart::docs) {
                    walk(doc, line_indent, indent_width, column);
                }
            }
        }
    }
    let mut column = None;
    walk(doc, line_indent, indent_width, &mut column);
    column
}

/// Whether `doc`'s first line, through its first hard line break, fits in
/// `room` columns with its groups flat: for a [`Doc::StartsPast`] body, which
/// starts the next line sooner than break on its first.
fn first_line_fits(doc: &Doc<'_>, room: isize) -> bool {
    fn walk(doc: &Doc<'_>, room: &mut isize) -> Option<bool> {
        let done = match doc {
            Doc::Nil | Doc::LineSuffix(_) | Doc::BreakParent | Doc::CountFrom => None,
            Doc::Text(text) => {
                *room -= width(text) as isize;
                None
            }
            Doc::Verbatim(text) | Doc::Relative { text, .. } => match text.find(['\n', '\r']) {
                Some(i) => Some(*room >= width(&text[..i]) as isize),
                None => {
                    *room -= width(text) as isize;
                    None
                }
            },
            Doc::Line(LineKind::Hard) => Some(*room >= 0),
            Doc::Line(LineKind::Line) => {
                *room -= 1;
                None
            }
            Doc::Line(LineKind::Soft) => None,
            Doc::Concat(parts) => parts.iter().find_map(|part| walk(part, room)),
            Doc::Nest(inner)
            | Doc::Offset(_, inner)
            | Doc::FromLineIndent(inner)
            | Doc::Group(inner)
            | Doc::ReluctantGroup(inner)
            | Doc::EndMarker { body: inner, .. } => walk(inner, room),
            Doc::IfBreak { flat, .. } => walk(flat, room),
            Doc::StartsPast { flat, body, .. } => walk(flat, room).or_else(|| walk(body, room)),
            Doc::Align(parts) => parts
                .iter()
                .flat_map(AlignPart::docs)
                .find_map(|doc| walk(doc, room)),
        };
        match done {
            None if *room < 0 => Some(false),
            done => done,
        }
    }
    walk(doc, &mut { room }).unwrap_or(room >= 0)
}

/// Whether `next`, printed in its mode, and then the rest of the current
/// line fit in `room` columns. [`Doc::Align`] rows count unpadded.
fn fits(next: Command<'_, '_>, rest: &[Command<'_, '_>], mut room: isize) -> bool {
    let mut stack = vec![(next.mode, next.doc)];
    let mut rest = rest.iter().rev();
    loop {
        if room < 0 {
            return false;
        }
        let (mode, item) = match stack.pop() {
            Some(item) => item,
            None => match rest.next() {
                Some(command) => (command.mode, command.doc),
                None => return true,
            },
        };
        let doc = match item {
            Item::Padding(spaces) => {
                room -= spaces as isize;
                continue;
            }
            // A marker starts its own line, so the line ends before it.
            Item::EndMarker { .. } => continue,
            Item::Doc(doc) => doc,
        };
        match doc {
            Doc::Nil | Doc::LineSuffix(_) | Doc::CountFrom => {}
            Doc::Text(text) => room -= width(text) as isize,
            Doc::Verbatim(text) | Doc::Relative { text, .. } => match text.find(['\n', '\r']) {
                Some(_) if mode == Mode::Flat => return false,
                Some(i) => return room >= width(&text[..i]) as isize,
                None => room -= width(text) as isize,
            },
            Doc::Line(LineKind::Hard) | Doc::BreakParent if mode == Mode::Flat => return false,
            Doc::BreakParent => {}
            Doc::Line(_) if mode == Mode::Break => return true,
            Doc::Line(LineKind::Line) => room -= 1,
            Doc::Line(_) => {}
            Doc::Concat(parts) => {
                stack.extend(parts.iter().rev().map(|doc| (mode, Item::Doc(doc))));
            }
            Doc::Nest(inner)
            | Doc::Offset(_, inner)
            | Doc::FromLineIndent(inner)
            | Doc::Group(inner)
            | Doc::EndMarker { body: inner, .. } => {
                stack.push((mode, Item::Doc(inner)));
            }
            // Never counted on to break.
            Doc::ReluctantGroup(inner) => stack.push((Mode::Flat, Item::Doc(inner))),
            Doc::IfBreak { broken, flat } => {
                let doc = if mode == Mode::Break { broken } else { flat };
                stack.push((mode, Item::Doc(doc)));
            }
            // Measured as if it breaks where it may: what's before it
            // doesn't count on the body sharing its line.
            Doc::StartsPast {
                broken, flat, body, ..
            } => {
                stack.push((mode, Item::Doc(body)));
                let before = if mode == Mode::Break { broken } else { flat };
                stack.push((mode, Item::Doc(before)));
            }
            Doc::Align(parts) => stack.extend(
                parts
                    .iter()
                    .flat_map(AlignPart::docs)
                    .rev()
                    .map(|doc| (mode, Item::Doc(doc))),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(width: usize, doc: &Doc<'_>) -> String {
        print(
            doc,
            &PrintOptions {
                max_width: width,
                ..PrintOptions::default()
            },
        )
    }

    fn t(text: &str) -> Doc<'_> {
        Doc::text(text)
    }

    /// `name(arg, arg, ...)`, breaking one argument per line.
    fn call<'a>(name: &'a str, args: Vec<Doc<'a>>) -> Doc<'a> {
        let args = Doc::join(args, t(",") + Doc::line());
        (t(name) + t("(") + (Doc::softline() + args).nest() + Doc::softline() + t(")")).group()
    }

    /// `def name =` with `n` statements indented below, marked at 3 breaks.
    fn def_with(name: &'static str, n: usize) -> Doc<'static> {
        let body = (0..n).map(|_| Doc::hardline() + t("a"));
        (t("def ") + t(name) + t(" =") + Doc::concat(body).nest())
            .end_marker(format!("end {name}"), 3)
    }

    #[test]
    fn an_end_marker_follows_a_long_body_only() {
        assert_eq!(at(80, &def_with("f", 2)), "def f =\n  a\n  a");
        assert_eq!(at(80, &def_with("f", 3)), "def f =\n  a\n  a\n  a\nend f");
    }

    #[test]
    fn an_end_marker_goes_at_its_first_lines_indentation() {
        let doc = t("object A:") + (Doc::hardline() + def_with("f", 3)).nest();
        let expected = "object A:\n  def f =\n    a\n    a\n    a\n  end f";
        assert_eq!(at(80, &doc), expected);
    }

    #[test]
    fn inner_end_markers_count_toward_outer_ones() {
        // Four breaks to the inner body's end, and a fifth for its marker.
        let inner = (Doc::hardline() + def_with("f", 3)).nest();
        let outer = (t("def g =") + inner).end_marker("end g", 5);
        assert_eq!(
            at(80, &outer),
            "def g =\n  def f =\n    a\n    a\n    a\n  end f\nend g"
        );
    }

    #[test]
    fn an_end_marker_counts_from_its_last_block() {
        // `if c then` with three lines, `else` with one.
        let branch = |n: usize| Doc::concat((0..n).map(|_| Doc::hardline() + t("a"))).nest();
        let doc = |else_lines| {
            let then = t("if c then") + branch(3) + Doc::hardline();
            (then + Doc::count_from() + t("else") + branch(else_lines)).end_marker("end if", 3)
        };
        let short = "if c then\n  a\n  a\n  a\nelse\n  a";
        assert_eq!(at(80, &doc(1)), short);
        let long = "if c then\n  a\n  a\n  a\nelse\n  a\n  a\n  a\nend if";
        assert_eq!(at(80, &doc(3)), long);
    }

    #[test]
    fn an_end_marker_counts_the_breaks_the_printer_chose() {
        let doc = call("f", vec![t("a"), t("b")]).end_marker("end f", 3);
        assert_eq!(at(80, &doc), "f(a, b)");
        assert_eq!(at(6, &doc), "f(\n  a,\n  b\n)\nend f");
    }

    /// `head = x match` with a case below, the body on the next line if it
    /// starts past 10 columns from its line's indentation.
    fn match_after<'a>(head: &'a str, scrutinee: &'a str) -> Doc<'a> {
        let body = t(scrutinee) + t(" match") + (Doc::hardline() + t("case _ => 1")).nest();
        let down = Shift {
            levels: 1,
            columns: 0,
        };
        t(head)
            + t(" =")
            + Doc::starts_past(
                10,
                (Doc::hardline(), down),
                (t(" "), Shift::default()),
                body,
                FirstLine::Flat,
            )
    }

    #[test]
    fn starts_past_stays_on_the_line_within_its_limit() {
        assert_eq!(
            at(80, &match_after("val a", "x")),
            "val a = x match
  case _ => 1"
        );
    }

    #[test]
    fn starts_past_breaks_past_its_limit() {
        assert_eq!(
            at(80, &match_after("def longer", "x")),
            "def longer =
  x match
    case _ => 1"
        );
    }

    #[test]
    fn starts_past_counts_from_the_line_indentation() {
        let doc = t("object A:") + (Doc::hardline() + match_after("val a", "x")).nest();
        assert_eq!(
            at(80, &doc),
            "object A:
  val a = x match
    case _ => 1"
        );
    }

    #[test]
    fn starts_past_breaks_when_its_first_line_would_not_fit() {
        assert_eq!(
            at(20, &match_after("val a", "scrutinee")),
            "val a =
  scrutinee match
    case _ => 1"
        );
    }

    #[test]
    fn starts_past_shifts_the_lines_inside_the_way_it_goes() {
        // Written a level deeper, as a body on its own line: joined, it
        // comes back a level.
        let body = (Doc::hardline() + t("case _ => 1")).nest().nest();
        let back = Shift {
            levels: -1,
            columns: 0,
        };
        let doc = t("val a =")
            + Doc::starts_past(
                10,
                (Doc::hardline(), Shift::default()),
                (t(" "), back),
                t("x match") + body,
                FirstLine::Flat,
            );
        assert_eq!(
            at(80, &doc),
            "val a = x match
  case _ => 1"
        );
    }

    #[test]
    fn starts_past_to_a_break_breaks_the_call_sooner_than_the_line() {
        let args = (Doc::softline() + t("alpha, beta")).nest() + Doc::softline();
        let call = (t("f(") + args + t(")")).group();
        let doc = t("val a =")
            + Doc::starts_past(
                usize::MAX,
                (
                    Doc::hardline(),
                    Shift {
                        levels: 1,
                        columns: 0,
                    },
                ),
                (t(" "), Shift::default()),
                call,
                FirstLine::ToBreak,
            );
        assert_eq!(at(20, &doc), "val a = f(\n  alpha, beta\n)");
        assert_eq!(at(80, &doc), "val a = f(alpha, beta)");
    }

    #[test]
    fn starts_past_whole_starts_the_next_line_sooner_than_break_the_call() {
        let args = (Doc::softline() + t("alpha, beta")).nest() + Doc::softline();
        let call = (t("f(") + args + t(")")).group();
        let doc = t("val a =")
            + Doc::starts_past(
                usize::MAX,
                (
                    Doc::hardline(),
                    Shift {
                        levels: 1,
                        columns: 0,
                    },
                ),
                (t(" "), Shift::default()),
                call,
                FirstLine::Whole,
            );
        assert_eq!(at(80, &doc), "val a = f(alpha, beta)");
        assert_eq!(at(20, &doc), "val a =\n  f(alpha, beta)");
        // Fitting on neither line, it breaks the call after all.
        assert_eq!(at(15, &doc), "val a = f(\n  alpha, beta\n)");
    }

    #[test]
    fn group_stays_flat_when_it_fits() {
        let doc = call("f", vec![t("a"), t("b")]);
        assert_eq!(at(80, &doc), "f(a, b)");
        assert_eq!(at(7, &doc), "f(a, b)", "exactly the width fits");
        assert_eq!(at(6, &doc), "f(\n  a,\n  b\n)");
    }

    #[test]
    fn a_reluctant_group_lets_the_group_before_it_break() {
        let args = |a: &'static str, b: &'static str| {
            (t("(")
                + (Doc::softline() + t(a) + t(",") + Doc::line() + t(b)).nest()
                + Doc::softline()
                + t(")"))
            .group()
        };
        let types = (t("[") + (Doc::softline() + t("B")).nest() + Doc::softline() + t("]"))
            .reluctant_group();
        let doc = t("f") + args("a", "b") + t(": F") + types.clone();
        // Fits flat.
        assert_eq!(at(80, &doc), "f(a, b): F[B]");
        // `f(a, b): F[` fits in 12 but `f(a, b): F[B]` doesn't: the
        // arguments break, not the type's brackets.
        assert_eq!(at(12, &doc), "f(\n  a,\n  b\n): F[B]");
        // With an ordinary group, the last one breaks.
        let plain = t("f")
            + args("a", "b")
            + t(": F")
            + (t("[") + (Doc::softline() + t("B")).nest() + Doc::softline() + t("]")).group();
        assert_eq!(at(12, &plain), "f(a, b): F[\n  B\n]");
        // Alone on its line, it breaks when it must.
        assert_eq!(at(3, &(t("F") + types)), "F[\n  B\n]");
    }

    #[test]
    fn outer_group_breaks_first() {
        let doc = call(
            "outer",
            vec![call("inner", vec![t("x"), t("y")]), t("zzzzzzzzzz")],
        );
        assert_eq!(at(80, &doc), "outer(inner(x, y), zzzzzzzzzz)");
        assert_eq!(at(20, &doc), "outer(\n  inner(x, y),\n  zzzzzzzzzz\n)");
        assert_eq!(
            at(10, &doc),
            "outer(\n  inner(\n    x,\n    y\n  ),\n  zzzzzzzzzz\n)"
        );
    }

    #[test]
    fn the_rest_of_the_line_counts() {
        // The group fits alone, but not with the text after it.
        let doc = call("f", vec![t("a")]) + t(".method()");
        assert_eq!(at(12, &doc), "f(\n  a\n).method()");
        assert_eq!(at(13, &doc), "f(a).method()");
    }

    #[test]
    fn hardline_forces_its_groups_to_break() {
        let doc = (t("{")
            + (Doc::line() + t("a") + Doc::hardline() + t("b")).nest()
            + Doc::line()
            + t("}"))
        .group();
        assert_eq!(at(80, &doc), "{\n  a\n  b\n}");
    }

    #[test]
    fn if_break_adds_a_trailing_comma_only_when_broken() {
        let args =
            Doc::join(vec![t("a"), t("b")], t(",") + Doc::line()) + Doc::if_break(t(","), Doc::Nil);
        let doc = (t("f(") + (Doc::softline() + args).nest() + Doc::softline() + t(")")).group();
        assert_eq!(at(80, &doc), "f(a, b)");
        assert_eq!(at(4, &doc), "f(\n  a,\n  b,\n)");
    }

    /// A row on its own line: `head` and `tail` split at a ` =>`.
    fn row<'a>(head: &'a str, tail: Doc<'a>, adjacent: bool) -> AlignPart<'a> {
        AlignPart::Row {
            before: Doc::hardline(),
            head: t(head),
            tail: t(" =>") + tail,
            adjacent,
        }
    }

    fn cases(rows: Vec<AlignPart<'_>>) -> Doc<'_> {
        t("{") + Doc::Align(rows).nest() + Doc::hardline() + t("}")
    }

    #[test]
    fn align_lines_up_tails_after_the_widest_head() {
        let doc = cases(vec![
            row("case Some(x)", t(" x"), true),
            row("case None", t(" 0"), true),
        ]);
        assert_eq!(
            at(80, &doc),
            "{\n  case Some(x) => x\n  case None    => 0\n}"
        );
    }

    #[test]
    fn align_starts_a_new_group_where_a_row_would_not_fit() {
        let doc = cases(vec![
            row("case LongPattern", t(" a"), true),
            row("case B", t(" a long body"), true),
        ]);
        // Padded to the first row's arrow, the second line is 33 wide.
        assert_eq!(
            at(33, &doc),
            "{\n  case LongPattern => a\n  case B           => a long body\n}"
        );
        assert_eq!(
            at(32, &doc),
            "{\n  case LongPattern => a\n  case B => a long body\n}"
        );
    }

    #[test]
    fn align_runs_end_at_rows_that_are_not_adjacent() {
        let doc = cases(vec![
            row("case Some(x)", t(" x"), true),
            row("case None", t(" 0"), false),
            row("case _", t(" 1"), true),
        ]);
        assert_eq!(
            at(80, &doc),
            "{\n  case Some(x) => x\n  case None => 0\n  case _    => 1\n}"
        );
    }

    #[test]
    fn align_a_broken_tail_aligns_its_first_line_and_ends_the_run() {
        let body = (Doc::hardline() + t("two")).nest();
        let doc = cases(vec![
            row("case 1", t(" one"), true),
            row("case 22", body, true),
            row("case 333", t(" three"), true),
        ]);
        assert_eq!(
            at(80, &doc),
            "{\n  case 1  => one\n  case 22 =>\n    two\n  case 333 => three\n}"
        );
    }

    #[test]
    fn offset_moves_the_lines_that_start_inside_it() {
        let doc = t("a")
            + (Doc::hardline() + t("b")).offset(3)
            + (Doc::hardline() + t("c")).nest()
            + (Doc::hardline() + t("d")).nest().offset(-1);
        assert_eq!(at(80, &doc), "a\n   b\n  c\n d");
        // Never past the start of the line.
        assert_eq!(at(80, &(Doc::hardline() + t("e")).offset(-4)), "\ne");
    }

    #[test]
    fn relative_text_moves_its_later_lines_with_its_first() {
        let comment = || Doc::Relative {
            text: "/** a\n    * b\n    */".into(),
            column: 4,
        };
        assert_eq!(at(80, &(t("  ") + comment())), "  /** a\n  * b\n  */");
        assert_eq!(
            at(80, &(t("      ") + comment())),
            "      /** a\n      * b\n      */"
        );
    }

    #[test]
    fn line_suffix_goes_to_the_end_of_the_line() {
        let comment = t(" // note").line_suffix();
        let doc = t("val a = 1") + comment + t(";") + Doc::hardline() + t("val b = 2");
        assert_eq!(at(80, &doc), "val a = 1; // note\nval b = 2");
        // At the very end, the suffix still gets printed.
        assert_eq!(at(80, &(t("x") + t(" // end").line_suffix())), "x // end");
    }

    #[test]
    fn line_comment_with_break_parent_keeps_its_line() {
        // `f(a, // first` / `b)`: the comment must stay after `a,`.
        let comment = t(" // first").line_suffix() + Doc::break_parent();
        let args = t("a") + t(",") + comment + Doc::line() + t("b");
        let doc = (t("f(") + (Doc::softline() + args).nest() + Doc::softline() + t(")")).group();
        assert_eq!(at(80, &doc), "f(\n  a, // first\n  b\n)");
    }

    #[test]
    fn verbatim_keeps_its_lines_and_breaks_groups() {
        let string = Doc::verbatim("\"\"\"one\n  two\"\"\"");
        let doc = (t("f(") + (Doc::softline() + string).nest() + Doc::softline() + t(")")).group();
        assert_eq!(at(80, &doc), "f(\n  \"\"\"one\n  two\"\"\"\n)");
        // The column continues from the verbatim text's last line.
        let after = Doc::verbatim("ab\ncd") + call("g", vec![t("x")]);
        assert_eq!(at(6, &after), "ab\ncdg(x)");
        assert_eq!(at(5, &after), "ab\ncdg(\n  x\n)");
    }

    #[test]
    fn verbatim_keeps_trailing_spaces_but_other_lines_lose_them() {
        let doc = t("a ")
            + Doc::hardline()
            + Doc::hardline()
            + Doc::verbatim("b  \nc  ")
            + Doc::hardline();
        assert_eq!(at(80, &doc.nest()), "a\n\n  b  \nc  \n");
    }

    #[test]
    fn nest_indents_lines_started_inside_it() {
        let body = (Doc::hardline() + t("val x = 1") + Doc::hardline() + t("x")).nest();
        let doc = t("def f =") + body + Doc::hardline() + t("end f");
        assert_eq!(at(80, &doc), "def f =\n  val x = 1\n  x\nend f");
        let tabs = PrintOptions {
            indent: "\t".to_string(),
            ..PrintOptions::default()
        };
        assert_eq!(print(&doc, &tabs), "def f =\n\tval x = 1\n\tx\nend f");
    }

    #[test]
    fn from_line_indent_starts_from_the_lines_indentation() {
        // Indentation written as text, as when it's copied from the source.
        let args = Doc::softline() + t("a") + t(",") + Doc::line() + t("b");
        let list = (t("(") + args.nest() + Doc::softline() + t(")"))
            .group()
            .from_line_indent();
        let doc = t("    \tf") + list.clone() + Doc::hardline() + t("g") + list.clone();
        assert_eq!(at(80, &doc), "    \tf(a, b)\ng(a, b)");
        assert_eq!(
            at(7, &doc),
            "    \tf(\n    \t  a,\n    \t  b\n    \t)\ng(a, b)"
        );
        // Inside a nest, the line's indentation replaces the nesting.
        let nested = (Doc::hardline() + t("x") + Doc::hardline() + t(" y") + list).nest();
        assert_eq!(at(5, &nested), "\n  x\n   y(\n     a,\n     b\n   )");
    }

    #[test]
    fn line_ending_is_configurable() {
        let crlf = PrintOptions {
            line_ending: "\r\n".to_string(),
            ..PrintOptions::default()
        };
        let doc = t("a") + Doc::hardline() + t("b");
        assert_eq!(print(&doc, &crlf), "a\r\nb");
    }

    #[test]
    fn width_counts_characters() {
        let doc = call("f", vec![t("日本語"), t("é")]);
        assert_eq!(at(9, &doc), "f(日本語, é)");
        assert_eq!(at(8, &doc), "f(\n  日本語,\n  é\n)");
    }

    #[test]
    fn flat_width_measures_one_line() {
        assert_eq!(call("f", vec![t("a"), t("bc")]).flat_width(), Some(8));
        let comment = t(" // c").line_suffix();
        assert_eq!((t("x") + comment.clone()).flat_width(), Some(1));
        assert_eq!((t("x") + comment).full_flat_width(), Some(6));
        assert_eq!((t("a") + Doc::hardline() + t("b")).flat_width(), None);
        assert_eq!(Doc::verbatim("a\nb").flat_width(), None);
    }

    #[test]
    fn width_to_first_break_stops_at_a_line() {
        assert_eq!(call("f", vec![t("a"), t("bc")]).width_to_first_break(), 2);
        assert_eq!((t("ab") + Doc::verbatim("c\nde")).width_to_first_break(), 3);
        assert_eq!(t("abc").width_to_first_break(), 3);
    }

    #[test]
    fn add_flattens() {
        let doc = t("a") + t("b") + Doc::Nil + (t("c") + t("d"));
        assert_eq!(doc, Doc::Concat(vec![t("a"), t("b"), t("c"), t("d")]));
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let mut doc = t("x");
        for _ in 0..1000 {
            doc = call("f", vec![doc]);
        }
        let out = at(80, &doc);
        assert_eq!(out.matches('\n').count(), out.lines().count() - 1);
        assert!(out.starts_with("f(\n"));
    }
}
