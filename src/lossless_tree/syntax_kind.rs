//! The kind of a node or token, as an enum generated from tree-sitter-scala.
//!
//! - Named kinds are CamelCase: `SyntaxKind::ClassDefinition`.
//! - Keywords and punctuation are easiest to write with the `T!` macro:
//!   `T![class]`, `T!["{"]`, `T!["=>"]`. Their variants are named `ClassKw`,
//!   `LCurly`, `EqGt`.
//! - `Error`, `Whitespace` and `Text` are added by this crate.
//!
//! `generated.rs` is written by a test; after upgrading tree-sitter-scala run
//! `UPDATE_GENERATED=1 cargo test syntax_kinds_are_up_to_date`.

use std::fmt;
use std::sync::OnceLock;

use super::language;

mod generated;

pub use generated::SyntaxKind;

impl SyntaxKind {
    /// Converts a tree-sitter symbol id.
    pub(super) fn from_ts(id: u16) -> SyntaxKind {
        // tree-sitter's built-in error symbol.
        if id == u16::MAX {
            return SyntaxKind::Error;
        }
        static TABLE: OnceLock<Vec<SyntaxKind>> = OnceLock::new();
        let table = TABLE.get_or_init(|| {
            let lang = language();
            (0..lang.node_kind_count())
                .map(|id| {
                    let id = id as u16;
                    let name = lang.node_kind_for_id(id).unwrap_or("");
                    // Only an outdated generated.rs can miss a kind; the
                    // up-to-date test catches that. Treat it as an error so
                    // tools leave the code alone.
                    SyntaxKind::from_grammar(name, lang.node_kind_is_named(id))
                        .unwrap_or(SyntaxKind::Error)
                })
                .collect()
        });
        table
            .get(usize::from(id))
            .copied()
            .unwrap_or(SyntaxKind::Error)
    }

    /// Keywords and punctuation, such as `class` and `{`, as opposed to named
    /// kinds like `identifier`.
    pub fn is_anonymous(self) -> bool {
        !self.is_named()
    }
}

impl fmt::Debug for SyntaxKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_named() {
            f.write_str(self.name())
        } else {
            write!(f, "{:?}", self.name())
        }
    }
}

impl fmt::Display for SyntaxKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod codegen {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write as _;

    struct Kind {
        name: &'static str,
        named: bool,
        variant: String,
    }

    /// Every kind that can appear in a tree, one per (name, named) pair.
    fn grammar_kinds() -> Vec<Kind> {
        let lang = language();
        let mut seen = BTreeSet::new();
        let mut kinds = Vec::new();
        for id in 1..lang.node_kind_count() as u16 {
            if !lang.node_kind_is_visible(id) {
                continue;
            }
            let name = lang.node_kind_for_id(id).expect("visible kinds have names");
            let named = lang.node_kind_is_named(id);
            if seen.insert((name, named)) {
                let variant = if named {
                    camel_case(name)
                } else {
                    anonymous_variant(name)
                };
                kinds.push(Kind {
                    name,
                    named,
                    variant,
                });
            }
        }
        // Named first, then anonymous; each alphabetical.
        kinds.sort_by(|a, b| (!a.named, a.name).cmp(&(!b.named, b.name)));
        kinds
    }

    fn camel_case(name: &str) -> String {
        name.split('_')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                let first = chars.next().expect("parts are non-empty");
                first.to_uppercase().chain(chars).collect::<String>()
            })
            .collect()
    }

    fn is_word(name: &str) -> bool {
        name.len() > 1 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    fn anonymous_variant(name: &str) -> String {
        if is_word(name) {
            return format!("{}Kw", camel_case(name));
        }
        name.chars()
            .map(|c| match c {
                '!' => "Bang",
                '"' => "Quote",
                '#' => "Pound",
                '$' => "Dollar",
                '%' => "Percent",
                '&' => "Amp",
                '\'' => "Apos",
                '(' => "LParen",
                ')' => "RParen",
                '*' => "Star",
                '+' => "Plus",
                ',' => "Comma",
                '-' => "Minus",
                '.' => "Dot",
                '/' => "Slash",
                ':' => "Colon",
                ';' => "Semi",
                '<' => "Lt",
                '=' => "Eq",
                '>' => "Gt",
                '?' => "Question",
                '@' => "At",
                '[' => "LBrack",
                '\\' => "Backslash",
                ']' => "RBrack",
                '^' => "Caret",
                '_' => "Underscore",
                '{' => "LCurly",
                '|' => "Pipe",
                '}' => "RCurly",
                '~' => "Tilde",
                '←' => "UnicodeLeftArrow",
                other => panic!("no variant name for {other:?} in {name:?}; add one"),
            })
            .collect()
    }

    fn generate() -> String {
        let kinds = grammar_kinds();
        let mut variants = BTreeMap::new();
        for kind in &kinds {
            if let Some(other) = variants.insert(kind.variant.as_str(), kind.name) {
                panic!("{:?} and {other:?} both become {}", kind.name, kind.variant);
            }
        }
        for extra in ["Error", "Whitespace", "Text"] {
            assert!(
                !variants.contains_key(extra),
                "grammar kind collides with {extra}"
            );
        }

        let mut out = String::new();
        let w = &mut out;
        writeln!(
            w,
            "// Generated from tree-sitter-scala by the `syntax_kinds_are_up_to_date` test."
        )
        .unwrap();
        writeln!(w, "// Do not edit; regenerate with `UPDATE_GENERATED=1 cargo test syntax_kinds_are_up_to_date`.").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "/// The kind of a node or token. See the module docs for naming."
        )
        .unwrap();
        writeln!(
            w,
            "#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]"
        )
        .unwrap();
        writeln!(w, "#[repr(u16)]").unwrap();
        writeln!(w, "pub enum SyntaxKind {{").unwrap();
        for kind in &kinds {
            let doc = if kind.named {
                format!("`{}`", kind.name)
            } else {
                format!("`{:?}`", kind.name)
            };
            writeln!(w, "    /// {doc}").unwrap();
            writeln!(w, "    {},", kind.variant).unwrap();
        }
        writeln!(
            w,
            "    /// tree-sitter's error node: input it couldn't parse."
        )
        .unwrap();
        writeln!(w, "    Error,").unwrap();
        writeln!(w, "    /// Whitespace between tokens.").unwrap();
        writeln!(w, "    Whitespace,").unwrap();
        writeln!(
            w,
            "    /// Text inside a node that isn't in any child, such as the literal"
        )
        .unwrap();
        writeln!(w, "    /// parts of an interpolated string.").unwrap();
        writeln!(w, "    Text,").unwrap();
        writeln!(w, "}}").unwrap();
        writeln!(w).unwrap();

        writeln!(w, "impl SyntaxKind {{").unwrap();
        writeln!(
            w,
            "    /// The grammar's name for this kind, such as `class_definition` or `{{`."
        )
        .unwrap();
        writeln!(w, "    pub fn name(self) -> &'static str {{").unwrap();
        writeln!(w, "        match self {{").unwrap();
        for kind in &kinds {
            writeln!(
                w,
                "            SyntaxKind::{} => {:?},",
                kind.variant, kind.name
            )
            .unwrap();
        }
        writeln!(w, "            SyntaxKind::Error => \"ERROR\",").unwrap();
        writeln!(w, "            SyntaxKind::Whitespace => \"whitespace\",").unwrap();
        writeln!(w, "            SyntaxKind::Text => \"text\",").unwrap();
        writeln!(w, "        }}").unwrap();
        writeln!(w, "    }}").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "    /// Named kinds (`identifier`, `class_definition`) versus keywords and"
        )
        .unwrap();
        writeln!(w, "    /// punctuation (`\"class\"`, `\"{{\"`).").unwrap();
        writeln!(w, "    pub fn is_named(self) -> bool {{").unwrap();
        writeln!(w, "        !matches!(").unwrap();
        writeln!(w, "            self,").unwrap();
        let anonymous: Vec<_> = kinds.iter().filter(|k| !k.named).collect();
        for (i, kind) in anonymous.iter().enumerate() {
            let sep = if i + 1 == anonymous.len() { "" } else { " |" };
            writeln!(w, "            SyntaxKind::{}{sep}", kind.variant).unwrap();
        }
        writeln!(w, "        )").unwrap();
        writeln!(w, "    }}").unwrap();
        writeln!(w).unwrap();
        writeln!(
            w,
            "    /// Word-like keywords such as `class`, `match` and `given`."
        )
        .unwrap();
        writeln!(w, "    pub fn is_keyword(self) -> bool {{").unwrap();
        writeln!(w, "        matches!(").unwrap();
        writeln!(w, "            self,").unwrap();
        let keywords: Vec<_> = anonymous.iter().filter(|k| is_word(k.name)).collect();
        for (i, kind) in keywords.iter().enumerate() {
            let sep = if i + 1 == keywords.len() { "" } else { " |" };
            writeln!(w, "            SyntaxKind::{}{sep}", kind.variant).unwrap();
        }
        writeln!(w, "        )").unwrap();
        writeln!(w, "    }}").unwrap();
        writeln!(w).unwrap();
        writeln!(w, "    /// Looks up a grammar kind by name.").unwrap();
        writeln!(
            w,
            "    pub(crate) fn from_grammar(name: &str, named: bool) -> Option<SyntaxKind> {{"
        )
        .unwrap();
        writeln!(w, "        Some(match (name, named) {{").unwrap();
        for kind in &kinds {
            writeln!(
                w,
                "            ({:?}, {}) => SyntaxKind::{},",
                kind.name, kind.named, kind.variant
            )
            .unwrap();
        }
        writeln!(w, "            _ => return None,").unwrap();
        writeln!(w, "        }})").unwrap();
        writeln!(w, "    }}").unwrap();
        writeln!(w, "}}").unwrap();
        writeln!(w).unwrap();

        writeln!(
            w,
            "/// A keyword or punctuation kind: `T![class]`, `T![\"{{\"]`, `T![\"=>\"]`."
        )
        .unwrap();
        writeln!(w, "#[macro_export]").unwrap();
        writeln!(w, "macro_rules! T {{").unwrap();
        for kind in &anonymous {
            let path = format!("$crate::lossless_tree::SyntaxKind::{}", kind.variant);
            if is_word(kind.name) {
                writeln!(w, "    [{}] => {{ {path} }};", kind.name).unwrap();
            }
            writeln!(w, "    [{:?}] => {{ {path} }};", kind.name).unwrap();
        }
        writeln!(w, "}}").unwrap();
        rustfmt(&out)
    }

    /// Formats like `cargo fmt` so the file stays stable when it runs.
    fn rustfmt(source: &str) -> String {
        use std::io::Write as _;
        use std::process::{Command, Stdio};
        let mut child = Command::new("rustfmt")
            .args(["--edition", "2024", "--emit", "stdout"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("rustfmt is installed");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(source.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "rustfmt failed on generated code");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn syntax_kinds_are_up_to_date() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/lossless_tree/syntax_kind/generated.rs"
        );
        let expected = generate();
        // A Windows checkout may have turned its line endings into `\r\n`.
        let actual = std::fs::read_to_string(path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        if actual == expected {
            return;
        }
        if std::env::var_os("UPDATE_GENERATED").is_some() {
            std::fs::write(path, expected).unwrap();
            return;
        }
        panic!(
            "{path} is out of date with tree-sitter-scala; run \
             `UPDATE_GENERATED=1 cargo test syntax_kinds_are_up_to_date`"
        );
    }

    #[test]
    fn every_grammar_kind_maps_to_its_variant() {
        let lang = language();
        for id in 1..lang.node_kind_count() as u16 {
            if !lang.node_kind_is_visible(id) {
                continue;
            }
            let kind = SyntaxKind::from_ts(id);
            assert_ne!(
                kind,
                SyntaxKind::Error,
                "id {id} {:?}",
                lang.node_kind_for_id(id)
            );
            assert_eq!(kind.name(), lang.node_kind_for_id(id).unwrap());
            assert_eq!(kind.is_named(), lang.node_kind_is_named(id));
        }
        assert_eq!(SyntaxKind::from_ts(u16::MAX), SyntaxKind::Error);
    }

    #[test]
    fn macro_and_names_agree() {
        assert_eq!(crate::T![class], SyntaxKind::ClassKw);
        assert_eq!(crate::T!["class"], SyntaxKind::ClassKw);
        assert_eq!(crate::T!["{"], SyntaxKind::LCurly);
        assert_eq!(crate::T!["=>"].name(), "=>");
        assert_eq!(crate::T![forSome].name(), "forSome");
        assert!(crate::T![match].is_keyword());
        assert!(!crate::T!["=>"].is_keyword());
        assert!(SyntaxKind::Identifier.is_named());
        assert!(crate::T![")"].is_anonymous());
    }
}
