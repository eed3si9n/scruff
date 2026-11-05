//! Scaladoc comments: on one line, `/** a */`, or over several, with `*/`
//! on a line of its own. As Scaladoc reads them, a `*` starting a line is
//! its gutter, and a line without one is text too. Their text is what
//! follows the gutter, less the narrowest gap after any `*`, or a line
//! without one less the narrowest indentation of such lines. So a formatter
//! may restyle the gutter, move the first line's text up onto `/**`'s
//! line, and put a comment of one line of text on one line or back.

/// The text lines of a Scaladoc comment, or `None` for any other comment.
/// The first is the text on `/**`'s line, or the line after it where `/**`
/// is alone. Blank lines, with or without a `*`, are empty. The lines are
/// trimmed at the end, those after the first lose the gap they all share
/// after their `*`, or, without one, the indentation they all share, and
/// blank lines at the end are left out.
pub(crate) fn scaladoc_lines(comment: &str) -> Option<Vec<&str>> {
    let body = comment.strip_prefix("/**")?.strip_suffix("*/")?;
    if body.starts_with(['*', '/']) || body.contains('\r') {
        return None;
    }
    if !body.contains('\n') {
        // `/** a */`, on one line.
        let text = body.trim();
        return (!text.is_empty()).then(|| vec![text]);
    }
    let mut lines = body.split('\n');
    let first = lines.next()?.trim();
    let mut rest: Vec<&str> = lines.collect();
    // What's before `*/` on its line: nothing but indentation.
    if !rest.pop()?.trim().is_empty() {
        return None;
    }
    // Each line's text, and whether it follows a `*`.
    let mut texts: Vec<(&str, bool)> = rest
        .into_iter()
        .map(|line| {
            let line = line.trim_end();
            match line.trim_start().strip_prefix('*') {
                Some(text) => (text, true),
                None if line.trim_start().is_empty() => ("", false),
                None => (line, false),
            }
        })
        .collect();
    // Where `/**` is alone, the line after it is the first, unless blank.
    let first = match (first, texts.first()) {
        ("", Some((next, _))) if !next.is_empty() => texts.remove(0).0.trim_start(),
        ("", None) => return None,
        (first, _) => first,
    };
    // The first line's own gap doesn't count.
    let gap = |gutter: bool| {
        texts
            .iter()
            .filter(|&&(t, g)| g == gutter && !t.is_empty())
            .map(|(t, _)| t.len() - t.trim_start_matches([' ', '\t']).len())
            .min()
            .unwrap_or(0)
    };
    let (gutter_gap, plain_gap) = (gap(true), gap(false));
    let texts = texts.into_iter().map(|(t, gutter)| {
        let gap = if gutter { gutter_gap } else { plain_gap };
        t.get(gap..).unwrap_or("")
    });
    let mut lines: Vec<&str> = if first.is_empty() {
        // A blank line after `/**` alone, then the rest.
        texts.collect()
    } else {
        std::iter::once(first).chain(texts).collect()
    };
    // Blank lines before `*/` say nothing.
    while lines.last() == Some(&"") {
        lines.pop();
    }
    (!lines.is_empty()).then_some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_with_the_same_text() {
        let lines = Some(vec!["A b", "", "c", "  d"]);
        for comment in [
            "/** A b\n *\n * c\n *   d\n */",
            "/** A b\n  *\n  *  c\n  *    d\n  */",
            "/**\n * A b\n *\n * c\n *   d\n */",
            "/**\n\t* A b\n\n\t* c\n\t*   d\n\t*/",
        ] {
            assert_eq!(scaladoc_lines(comment), lines, "{comment:?}");
        }
    }

    #[test]
    fn one_line_of_text_on_one_line_or_more() {
        for comment in [
            "/** A b */",
            "/**  A b  */",
            "/** A b\n */",
            "/**\n * A b\n */",
            "/** A b\n *\n *\n */",
        ] {
            assert_eq!(scaladoc_lines(comment), Some(vec!["A b"]), "{comment:?}");
        }
    }

    #[test]
    fn the_line_moving_up_to_the_opening_has_no_say_in_the_gap() {
        assert_eq!(
            scaladoc_lines("/**\n * Using a\n *  b\n *  c\n */"),
            Some(vec!["Using a", "b", "c"])
        );
    }

    #[test]
    fn lines_without_a_gutter_keep_their_own_indentation() {
        assert_eq!(
            scaladoc_lines("/** a\n  b\n  {{{\n    c\n  }}}\n  */"),
            Some(vec!["a", "b", "{{{", "  c", "}}}"])
        );
        assert_eq!(
            scaladoc_lines("/**\n  a\n   * b\n    c\n */"),
            Some(vec!["a", "b", "c"])
        );
    }

    #[test]
    fn a_blank_line_after_the_opening_stays() {
        assert_eq!(scaladoc_lines("/**\n *\n * a\n */"), Some(vec!["", "a"]));
    }

    #[test]
    fn other_comments() {
        for comment in [
            "/** */",
            "/* a\n * b\n */",
            "/** a\n * b */",
            "/** a\n  b\n}*/",
            "/**/",
            "/***\n * a\n */",
            "/**\n */",
            "/**\n *\n */",
        ] {
            assert_eq!(scaladoc_lines(comment), None, "{comment:?}");
        }
    }
}
