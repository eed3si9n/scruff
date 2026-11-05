//! The linter: rules that flag code to change by hand, as `scruff check`
//! runs them.
//!
//! Each rule has a file of its own under `lint/`, with its documentation,
//! its message, a `check` function that looks at one node or token, and
//! its tests.
//! This file holds the table of rules: each one's name, code and whether it
//! runs by default. [`check`] walks the tree once and asks the selected
//! rules about every node and token.
//!
//! A `// scruff: ignore(S004)` comment turns rules off for a line; see
//! `lint/suppression.rs`.
//!
//! To add a rule, add its file, a [`Rule`] variant, and the variant's arm in
//! each `match` below.

mod no_asinstanceof;
mod no_default_param;
mod no_null;
mod no_procedure_syntax;
mod no_return;
mod no_var;
mod no_while;
mod no_xml;
mod suppression;
mod untyped_implicit_val;

use std::fmt;

use crate::lossless_tree::{SyntaxElement, SyntaxError, TextRange, Tree, parse};

/// What kind of problem a rule finds. Its letter starts the rule's code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Code that is wrong, or that a compiler rejects.
    Correctness,
    /// Code that works but goes against Scala's conventions.
    Style,
}

impl Category {
    pub const ALL: &'static [Category] = &[Category::Correctness, Category::Style];

    /// The letter that starts the codes of its rules.
    pub fn letter(self) -> char {
        match self {
            Category::Correctness => 'C',
            Category::Style => 'S',
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Category::Correctness => "correctness",
            Category::Style => "style",
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A lint rule. Each has a category and a number, unique in its category,
/// that together make its code, such as `S001`. A rule's code never
/// changes, and a removed rule's number isn't reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    /// `return`; see `lint/no_return.rs`.
    NoReturn,
    /// `var`; see `lint/no_var.rs`.
    NoVar,
    /// A parameter's default value; see `lint/no_default_param.rs`.
    NoDefaultParam,
    /// `null`; see `lint/no_null.rs`.
    NoNull,
    /// `while`; see `lint/no_while.rs`.
    NoWhile,
    /// `asInstanceOf`; see `lint/no_asinstanceof.rs`.
    NoAsInstanceOf,
    /// XML literals; see `lint/no_xml.rs`.
    NoXml,
    /// `def f() { ... }`; see `lint/no_procedure_syntax.rs`.
    NoProcedureSyntax,
    /// `implicit val x = ...`; see `lint/untyped_implicit_val.rs`.
    UntypedImplicitVal,
}

impl Rule {
    /// Every rule, in the order they're reported for the same place.
    pub const ALL: &'static [Rule] = &[
        Rule::NoReturn,
        Rule::NoVar,
        Rule::NoDefaultParam,
        Rule::NoNull,
        Rule::NoWhile,
        Rule::NoAsInstanceOf,
        Rule::NoXml,
        Rule::NoProcedureSyntax,
        Rule::UntypedImplicitVal,
    ];

    /// The rule's name, as printed.
    pub fn name(self) -> &'static str {
        match self {
            Rule::NoReturn => "no_return",
            Rule::NoVar => "no_var",
            Rule::NoDefaultParam => "no_default_param",
            Rule::NoNull => "no_null",
            Rule::NoWhile => "no_while",
            Rule::NoAsInstanceOf => "no_asinstanceof",
            Rule::NoXml => "no_xml",
            Rule::NoProcedureSyntax => "no_procedure_syntax",
            Rule::UntypedImplicitVal => "untyped_implicit_val",
        }
    }

    /// Whether the rule runs without being selected. The others are
    /// opt-in, through `[lint]` in `.scruff.toml`.
    pub fn is_default(self) -> bool {
        match self {
            Rule::NoReturn => true,
            // Common in local, performance-minded code.
            Rule::NoVar => false,
            // Common in application code; a problem for libraries.
            Rule::NoDefaultParam => false,
            Rule::NoNull => true,
            // Goes with `var`, in the same performance-minded code.
            Rule::NoWhile => false,
            // Libraries cast on purpose, where the types prove it safe.
            Rule::NoAsInstanceOf => false,
            // Rare, and Scala 3 plans to drop them.
            Rule::NoXml => true,
            // Scala 3 doesn't accept it.
            Rule::NoProcedureSyntax => true,
            // Scala 3 rejects it.
            Rule::UntypedImplicitVal => true,
        }
    }

    /// The rules that run without a `[lint]` table.
    pub fn defaults() -> Vec<Rule> {
        Rule::ALL
            .iter()
            .copied()
            .filter(|r| r.is_default())
            .collect()
    }

    /// The rule's category and its number there, from 1.
    pub fn code(self) -> (Category, u16) {
        match self {
            Rule::NoReturn => (Category::Style, 1),
            Rule::NoVar => (Category::Style, 2),
            Rule::NoDefaultParam => (Category::Style, 3),
            Rule::NoNull => (Category::Style, 4),
            Rule::NoWhile => (Category::Style, 5),
            Rule::NoAsInstanceOf => (Category::Style, 6),
            Rule::NoXml => (Category::Style, 7),
            Rule::NoProcedureSyntax => (Category::Style, 8),
            Rule::UntypedImplicitVal => (Category::Correctness, 1),
        }
    }

    /// The code as printed: the category's letter and the number, as
    /// `S001`.
    pub fn code_text(self) -> String {
        let (category, number) = self.code();
        format!("{}{number:03}", category.letter())
    }

    /// The rule with `code`, such as `S001`.
    pub fn from_code(code: &str) -> Option<Rule> {
        Rule::ALL
            .iter()
            .copied()
            .find(|rule| rule.code_text() == code)
    }

    /// What the rule flags, in a few words.
    pub fn summary(self) -> &'static str {
        match self {
            Rule::NoReturn => no_return::SUMMARY,
            Rule::NoVar => no_var::SUMMARY,
            Rule::NoDefaultParam => no_default_param::SUMMARY,
            Rule::NoNull => no_null::SUMMARY,
            Rule::NoWhile => no_while::SUMMARY,
            Rule::NoAsInstanceOf => no_asinstanceof::SUMMARY,
            Rule::NoXml => no_xml::SUMMARY,
            Rule::NoProcedureSyntax => no_procedure_syntax::SUMMARY,
            Rule::UntypedImplicitVal => untyped_implicit_val::SUMMARY,
        }
    }

    /// What the rule says about each place it flags.
    pub fn message(self) -> &'static str {
        match self {
            Rule::NoReturn => no_return::MESSAGE,
            Rule::NoVar => no_var::MESSAGE,
            Rule::NoDefaultParam => no_default_param::MESSAGE,
            Rule::NoNull => no_null::MESSAGE,
            Rule::NoWhile => no_while::MESSAGE,
            Rule::NoAsInstanceOf => no_asinstanceof::MESSAGE,
            Rule::NoXml => no_xml::MESSAGE,
            Rule::NoProcedureSyntax => no_procedure_syntax::MESSAGE,
            Rule::UntypedImplicitVal => untyped_implicit_val::MESSAGE,
        }
    }

    /// Where in `element` this rule flags, if anywhere.
    fn check(self, element: &SyntaxElement<'_>) -> Option<TextRange> {
        match self {
            Rule::NoReturn => no_return::check(element),
            Rule::NoVar => no_var::check(element),
            Rule::NoDefaultParam => no_default_param::check(element),
            Rule::NoNull => no_null::check(element),
            Rule::NoWhile => no_while::check(element),
            Rule::NoAsInstanceOf => no_asinstanceof::check(element),
            Rule::NoXml => no_xml::check(element),
            Rule::NoProcedureSyntax => no_procedure_syntax::check(element),
            Rule::UntypedImplicitVal => untyped_implicit_val::check(element),
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Which rules a project runs, from `[lint]` in `.scruff.toml`, as in Ruff:
///
/// ```toml
/// [lint]
/// extend-select = ["S002"]  # adds rules to the defaults
/// ignore = ["no_return"]    # turns rules off
/// # select = ["S"]          # replaces the defaults
/// ```
///
/// Each entry is a rule's name, its code, a start of codes (`S` for every
/// style rule), or `ALL`. `ignore` wins over both selects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// `None` for the default rules.
    pub select: Option<Vec<String>>,
    pub extend_select: Vec<String>,
    pub ignore: Vec<String>,
}

impl Selection {
    /// The rules selected, in [`Rule::ALL`]'s order, or the first entry no
    /// rule matches.
    pub fn rules(&self) -> Result<Vec<Rule>, String> {
        let matching = |entries: &[String]| -> Result<Vec<Rule>, String> {
            let mut rules = Vec::new();
            for entry in entries {
                let matched = Rule::matching(entry);
                if matched.is_empty() {
                    return Err(format!("no rule is named or coded {entry:?}"));
                }
                rules.extend(matched);
            }
            Ok(rules)
        };
        let mut selected = match &self.select {
            Some(entries) => matching(entries)?,
            None => Rule::defaults(),
        };
        selected.extend(matching(&self.extend_select)?);
        let ignored = matching(&self.ignore)?;
        Ok(Rule::ALL
            .iter()
            .copied()
            .filter(|rule| selected.contains(rule) && !ignored.contains(rule))
            .collect())
    }
}

impl Rule {
    /// The rules an entry of a [`Selection`] stands for.
    fn matching(entry: &str) -> Vec<Rule> {
        Rule::ALL
            .iter()
            .copied()
            .filter(|rule| {
                entry == "ALL"
                    || entry == rule.name()
                    || (!entry.is_empty() && rule.code_text().starts_with(entry))
            })
            .collect()
    }
}

/// A place a rule flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub rule: Rule,
    pub range: TextRange,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LintError {
    /// The input doesn't parse cleanly, so the tree can't be trusted.
    SyntaxErrors(Vec<SyntaxError>),
}

impl fmt::Display for LintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LintError::SyntaxErrors(errors) => {
                write!(f, "not checking code with syntax errors")?;
                if let Some(first) = errors.first() {
                    write!(f, ", the first: {first}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for LintError {}

/// Checks Scala source against `rules`.
pub fn check(source: &str, rules: &[Rule]) -> Result<Vec<Diagnostic>, LintError> {
    check_tree(&parse(source), rules)
}

/// Checks a parsed tree against `rules`, reporting in source order.
pub fn check_tree(tree: &Tree, rules: &[Rule]) -> Result<Vec<Diagnostic>, LintError> {
    // tree-sitter's error recovery can turn code into something else, so a
    // rule could flag what isn't there.
    if !tree.errors().is_empty() {
        return Err(LintError::SyntaxErrors(tree.errors().to_vec()));
    }
    let suppressions = suppression::Suppressions::find(tree);
    let mut diagnostics = Vec::new();
    for element in tree.syntax().descendants_with_tokens() {
        for &rule in rules {
            if let Some(range) = rule.check(&element)
                && !suppressions.covers(tree.line_col(range.start()).0, rule)
            {
                diagnostics.push(Diagnostic {
                    rule,
                    range,
                    message: rule.message().to_string(),
                });
            }
        }
    }
    Ok(diagnostics)
}

/// Where `rule` flags `source`, each place as `line:col`, for the rules'
/// tests.
#[cfg(test)]
fn flagged(rule: Rule, source: &str) -> Vec<String> {
    let tree = parse(source);
    check_tree(&tree, &[rule])
        .unwrap()
        .iter()
        .map(|d| {
            let (line, col) = tree.line_col(d.range.start());
            format!("{line}:{col}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for &rule in Rule::ALL {
            let (_, number) = rule.code();
            assert!(
                (1..=999).contains(&number),
                "{} has number {number}",
                rule.name()
            );
            assert!(
                seen.insert(rule.code()),
                "{} is used twice",
                rule.code_text()
            );
            assert_eq!(Rule::from_code(&rule.code_text()), Some(rule));
        }
        let letters: std::collections::HashSet<char> =
            Category::ALL.iter().map(|c| c.letter()).collect();
        assert_eq!(letters.len(), Category::ALL.len());
        assert_eq!(Rule::NoReturn.code(), (Category::Style, 1));
        assert_eq!(Rule::NoReturn.code_text(), "S001");
        assert_eq!(Rule::from_code("S999"), None);
    }

    fn selected(select: Option<&[&str]>, extend: &[&str], ignore: &[&str]) -> Vec<Rule> {
        let strings = |entries: &[&str]| entries.iter().map(|e| e.to_string()).collect();
        Selection {
            select: select.map(strings),
            extend_select: strings(extend),
            ignore: strings(ignore),
        }
        .rules()
        .unwrap()
    }

    #[test]
    fn only_default_rules_run_unless_selected() {
        use Rule::*;
        // The defaults, spelled out; the other cases follow from them.
        let defaults = [
            NoReturn,
            NoNull,
            NoXml,
            NoProcedureSyntax,
            UntypedImplicitVal,
        ];
        assert_eq!(selected(None, &[], &[]), defaults);
        let all_but = |left_out: &[Rule]| -> Vec<Rule> {
            Rule::ALL
                .iter()
                .copied()
                .filter(|r| !left_out.contains(r))
                .collect()
        };
        let with_var: Vec<Rule> = Rule::ALL
            .iter()
            .copied()
            .filter(|r| defaults.contains(r) || *r == NoVar)
            .collect();
        assert_eq!(selected(None, &["S002"], &[]), with_var);
        let codes: Vec<String> = defaults.iter().map(|r| r.code_text()).collect();
        let codes: Vec<&str> = codes.iter().map(String::as_str).collect();
        assert_eq!(selected(None, &["no_var"], &codes), [NoVar]);
        assert_eq!(selected(Some(&["no_var"]), &[], &[]), [NoVar]);
        let style: Vec<Rule> = Rule::ALL
            .iter()
            .copied()
            .filter(|r| r.code().0 == Category::Style)
            .collect();
        assert_eq!(selected(Some(&["S"]), &[], &[]), style);
        assert_eq!(selected(Some(&["C"]), &[], &[]), [UntypedImplicitVal]);
        assert_eq!(selected(Some(&["C001"]), &[], &[]), [UntypedImplicitVal]);
        assert_eq!(
            selected(Some(&["ALL"]), &[], &["no_var"]),
            all_but(&[NoVar])
        );
        assert_eq!(selected(Some(&[]), &[], &[]), []);
    }

    #[test]
    fn unknown_entries_are_errors() {
        let selection = Selection {
            ignore: vec!["S9".to_string()],
            ..Selection::default()
        };
        assert_eq!(
            selection.rules(),
            Err("no rule is named or coded \"S9\"".to_string())
        );
    }

    #[test]
    fn rules_report_in_source_order_then_table_order() {
        let tree = parse("object A:\n  var x = 1\n  def f: Int =\n    var y = 2\n    return y\n");
        let found: Vec<_> = check_tree(&tree, Rule::ALL)
            .unwrap()
            .iter()
            .map(|d| (tree.line_col(d.range.start()), d.rule))
            .collect();
        assert_eq!(
            found,
            [
                ((2, 3), Rule::NoVar),
                ((4, 5), Rule::NoVar),
                ((5, 5), Rule::NoReturn)
            ]
        );
    }

    #[test]
    fn syntax_errors_are_refused() {
        assert!(matches!(
            check("object A { def f = return", Rule::ALL),
            Err(LintError::SyntaxErrors(_))
        ));
    }
}
