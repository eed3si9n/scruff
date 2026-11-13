//! A project's configuration: which files to format, and the line width.
//!
//! scruff uses the nearest `.scruff.toml` in a file's directory or one
//! above it, or else the nearest `.scalafmt.conf`, so a `.scruff.toml`
//! wins over a nearer `.scalafmt.conf`. Only that one file counts.
//!
//! `.scruff.toml` has these keys:
//!
//! - `max-column`, 100 by default.
//! - `dialect`, the Scala dialect, by scalafmt's names: `scala3`,
//!   `scala213source3`, `scala212source3`, `scala213` or `sbt1`; `scala3`
//!   by default. scruff rewrites code to the new
//!   syntax it allows.
//! - `include-paths` and `exclude-paths`: `glob:` or `regex:` patterns, in
//!   the syntax of Java's `PathMatcher`, which must match a file's whole
//!   absolute path. Include paths default to `.scala`, `.sbt`, `.sc` and
//!   `.mill` files.
//! - `[lint]`, the rules `scruff check` runs: `select`, `extend-select` and
//!   `ignore`, as [`Selection`] describes. Without it, the default rules.
//!
//! From `.scalafmt.conf`, scruff reads only these settings, with scalafmt's
//! meaning:
//!
//! - `maxColumn`, or scruff's own 100 when the file doesn't set it, where
//!   scalafmt would use 80.
//! - `project.includePaths` and `project.excludePaths`, the same patterns.
//! - `project.includeFilters` and `project.excludeFilters`, their older
//!   form: regexes that must match somewhere in the path. Include filters
//!   replace the default include paths unless those are set as well.
//! - `runner.dialect`, also under `fileOverride`, whose patterns are
//!   tried longest first, as in scalafmt. The dialect is `scala3` when
//!   none is named. `runner.dialectOverride` isn't read: what scruff
//!   rewrites goes with the dialect's name alone.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::lint::{Rule, Selection};

/// scruff's own configuration file.
pub const SCRUFF_FILE: &str = ".scruff.toml";

/// scalafmt's configuration file, which scruff reads a few settings from.
pub const SCALAFMT_FILE: &str = ".scalafmt.conf";

/// scalafmt's `project.includePaths` when a configuration doesn't set it.
const DEFAULT_INCLUDE_PATHS: [&str; 4] =
    ["glob:**.scala", "glob:**.sbt", "glob:**.sc", "glob:**.mill"];

/// The settings scruff takes from a configuration file.
#[derive(Debug)]
pub struct Config {
    /// The configuration file.
    pub path: PathBuf,
    pub max_column: usize,
    include: Vec<Matcher>,
    exclude: Vec<Matcher>,
    /// Directories whose every file is excluded, from `glob:.../X/**`
    /// exclude paths, matched as `glob:.../X`.
    exclude_dirs: Vec<Matcher>,
    /// The dialect of files no override matches.
    dialect: Dialect,
    /// Dialects for the files a pattern matches, the first match winning.
    dialect_overrides: Vec<(Matcher, Dialect)>,
    /// The lint rules `scruff check` runs, from `[lint]`.
    pub lint_rules: Vec<Rule>,
}

/// What new syntax a Scala dialect allows, which decides what scruff
/// rewrites to it. Named after scalameta's dialect flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dialect {
    /// `import a.*` for `import a._`.
    pub star_wildcard_import: bool,
    /// `import a.{b as c}` for `import a.{b => c}`.
    pub as_for_import_rename: bool,
    /// `List[?]` for `List[_]`. Without it, `?` is a type's name, as in
    /// kind-projector's `Either[A, ?]`, so it isn't rewritten back.
    pub question_mark_type_wildcard: bool,
    /// `f(xs*)` for `f(xs: _*)`, and `case Seq(xs*)` for `case Seq(xs @ _*)`.
    pub postfix_star_vararg_splices: bool,
    /// `f(a ++ b*)` splicing the whole infix expression, `a ++ b`. Only
    /// Scala 3's: Scala 2 with `-Xsource:3` reads it as `a ++ (b*)`, so
    /// there `f(a ++ b: _*)` becomes `f((a ++ b)*)`.
    pub infix_postfix_star_vararg_splices: bool,
    /// `if c then a` for `if (c) a`, `while c do x` for `while (c) x`, and
    /// `for x <- xs do f(x)` for `for (x <- xs) f(x)`. Only Scala 3's.
    pub quiet_syntax: bool,
    /// Indentation for braces: `class A:` for `class A {`, and `def f =`
    /// with its body indented below for `def f = {`. Only Scala 3's.
    pub significant_indentation: bool,
    /// A colon argument for a braced one: `xs.map: x =>` with the body
    /// indented below for `xs.map { x =>`. Only Scala 3's, since 3.3, and
    /// only with significant indentation, which a colon argument is.
    pub fewer_braces: bool,
    /// A `using` clause for an `implicit` one: `def f(using ec: EC)` for
    /// `def f(implicit ec: EC)`. Only Scala 3's.
    pub given_using: bool,
    /// An intersection type for a compound one: `A & B` for `A with B`.
    /// Only Scala 3's.
    pub and_types: bool,
    /// Commas between the parents a class extends: `extends A, B` for
    /// `extends A with B`. Only Scala 3's.
    pub comma_separated_extends: bool,
    /// A line may start with an infix operator, continuing the line
    /// before, as in `a` and then `&& b` on the next line. Only Scala 3's.
    /// Without it, a boolean chain broken before its operators is wrapped
    /// in parentheses, inside which line breaks don't end a statement.
    pub leading_infix_operators: bool,
}

/// The dialects scruff knows, by scalafmt's names.
const DIALECTS: [&str; 5] = [
    "scala3",
    "scala213source3",
    "scala212source3",
    "scala213",
    "sbt1",
];

impl Dialect {
    /// Scala 3, scruff's dialect when none is named.
    pub const SCALA3: Dialect = Dialect {
        star_wildcard_import: true,
        as_for_import_rename: true,
        question_mark_type_wildcard: true,
        postfix_star_vararg_splices: true,
        infix_postfix_star_vararg_splices: true,
        quiet_syntax: true,
        significant_indentation: true,
        fewer_braces: true,
        given_using: true,
        and_types: true,
        comma_separated_extends: true,
        leading_infix_operators: true,
    };

    /// The dialect scalafmt calls `name`, ignoring case: `scala3`,
    /// `scala213source3` or `scala212source3` (Scala 2 with `-Xsource:3`),
    /// `sbt1` (sbt 1's build files, which allow the same), or `scala213`.
    pub fn named(name: &str) -> Option<Dialect> {
        // Scala 3's imports, wildcards and splices are in Scala 2 with
        // `-Xsource:3` too, but not its control syntax.
        let (new_syntax, scala3) = match name.to_ascii_lowercase().as_str() {
            "scala3" => (true, true),
            "scala213source3" | "scala212source3" | "sbt1" => (true, false),
            "scala213" => (false, false),
            _ => return None,
        };
        Some(Dialect {
            star_wildcard_import: new_syntax,
            as_for_import_rename: new_syntax,
            question_mark_type_wildcard: new_syntax,
            postfix_star_vararg_splices: new_syntax,
            infix_postfix_star_vararg_splices: scala3,
            quiet_syntax: scala3,
            significant_indentation: scala3,
            fewer_braces: scala3,
            given_using: scala3,
            and_types: scala3,
            comma_separated_extends: scala3,
            leading_infix_operators: scala3,
        })
    }
}

/// A dialect as a configuration gives it, before it's resolved: its name.
/// scalafmt's `dialectOverride` flags aren't read; the name alone decides.
#[derive(Clone, Debug, Default)]
struct DialectSetting {
    name: Option<String>,
}

impl DialectSetting {
    /// The dialect: Scala 3 without a name.
    fn resolve(&self) -> Result<Dialect, String> {
        let name = self.name.as_deref().unwrap_or("scala3");
        Dialect::named(name).ok_or_else(|| {
            format!(
                "unknown dialect {name:?}; the dialects are {}",
                DIALECTS.join(", ")
            )
        })
    }

    /// This setting with what `runner` sets, an object holding `dialect`.
    fn with_runner(&self, runner: &hocon::HoconValue) -> Result<DialectSetting, String> {
        let mut setting = self.clone();
        let Some(runner) = runner.as_object() else {
            return Err("runner must be an object".into());
        };
        if let Some(name) = runner.get("dialect") {
            setting.name = Some(
                name.as_str()
                    .ok_or("runner.dialect must be a dialect's name")?
                    .to_string(),
            );
        }
        Ok(setting)
    }
}

#[derive(Debug)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for ConfigError {}

/// The `[lint]` table: `select`, `extend-select` and `ignore`, each a list
/// of rules' names or codes.
fn lint_selection(value: &toml::Value) -> Result<Selection, String> {
    let table = value.as_table().ok_or("lint must be a table")?;
    let mut selection = Selection::default();
    for (key, value) in table {
        let message = || format!("lint.{key} must be a list of strings");
        let strings = value
            .as_array()
            .ok_or_else(message)?
            .iter()
            .map(|item| item.as_str().map(str::to_string).ok_or_else(message))
            .collect::<Result<Vec<_>, _>>();
        match key.as_str() {
            "select" => selection.select = Some(strings?),
            "extend-select" => selection.extend_select = strings?,
            "ignore" => selection.ignore = strings?,
            _ => {
                return Err(format!(
                    "unknown key lint.{key}; the keys are select, extend-select and ignore"
                ));
            }
        }
    }
    Ok(selection)
}

/// A pattern a path must match, as scalafmt matches it.
#[derive(Debug)]
struct Matcher(Regex);

/// The patterns a configuration gives, before they're compiled.
#[derive(Default)]
struct Patterns {
    /// `None` for the default include paths.
    include_paths: Option<Vec<String>>,
    include_filters: Vec<String>,
    exclude_paths: Vec<String>,
    exclude_filters: Vec<String>,
}

impl Config {
    /// The configuration in the nearest `.scruff.toml` or `.scalafmt.conf` in
    /// `dir` or a directory above it, if there is one.
    pub fn find(dir: &Path) -> Result<Option<Config>, ConfigError> {
        Config::locate(dir)
            .map(|path| Config::load(&path))
            .transpose()
    }

    /// The nearest `.scruff.toml` or `.scalafmt.conf` in `dir` or a
    /// directory above it, without reading it.
    pub fn locate(dir: &Path) -> Option<PathBuf> {
        Locator::default().locate(dir)
    }

    /// Reads a `.scruff.toml`, or else a `.scalafmt.conf`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let error = |message: String| ConfigError {
            path: path.to_path_buf(),
            message,
        };
        if is_toml(path) {
            let text = std::fs::read_to_string(path).map_err(|e| error(e.to_string()))?;
            return Config::parse(path, &text);
        }
        let config = hocon::parse_file(path).map_err(|e| error(e.to_string()))?;
        Config::from_hocon(path, &config).map_err(error)
    }

    /// Reads a configuration's text, as if from the file at `path`: TOML for
    /// a `.toml` file, HOCON otherwise.
    pub fn parse(path: &Path, text: &str) -> Result<Config, ConfigError> {
        let error = |message: String| ConfigError {
            path: path.to_path_buf(),
            message,
        };
        if is_toml(path) {
            let table = text
                .parse::<toml::Table>()
                .map_err(|e| error(e.to_string().trim_end().to_string()))?;
            return Config::from_toml(path, &table).map_err(error);
        }
        let config = hocon::parse(text).map_err(|e| error(e.to_string()))?;
        Config::from_hocon(path, &config).map_err(error)
    }

    fn from_toml(path: &Path, table: &toml::Table) -> Result<Config, String> {
        let mut max_column = crate::fmt::FormatOptions::default().max_width;
        let mut patterns = Patterns::default();
        let mut dialect = DialectSetting::default();
        let mut lint = Selection::default();
        for (key, value) in table {
            let strings = || -> Result<Vec<String>, String> {
                let message = || format!("{key} must be a list of strings");
                value
                    .as_array()
                    .ok_or_else(message)?
                    .iter()
                    .map(|item| item.as_str().map(str::to_string).ok_or_else(message))
                    .collect()
            };
            match key.as_str() {
                "max-column" => {
                    max_column = value
                        .as_integer()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|&n| n > 0)
                        .ok_or("max-column must be a positive number")?;
                }
                // Read by tools that install scruff (the GitHub action); the
                // formatter only checks that it is a string.
                "version" => {
                    value
                        .as_str()
                        .ok_or("version must be a string, such as \"0.1.0\"")?;
                }
                "include-paths" => patterns.include_paths = Some(strings()?),
                "exclude-paths" => patterns.exclude_paths = strings()?,
                "dialect" => {
                    dialect.name = Some(
                        value
                            .as_str()
                            .ok_or("dialect must be a dialect's name")?
                            .to_string(),
                    );
                }
                "lint" => lint = lint_selection(value)?,
                _ => {
                    return Err(format!(
                        "unknown key {key:?}; the keys are version, max-column, dialect, \
                         include-paths, exclude-paths and lint"
                    ));
                }
            }
        }
        let mut config = Config::new(path, max_column, patterns)?;
        config.dialect = dialect.resolve()?;
        config.lint_rules = lint.rules().map_err(|e| format!("lint: {e}"))?;
        Ok(config)
    }

    fn from_hocon(path: &Path, config: &hocon::Config) -> Result<Config, String> {
        let max_column = match config.get("maxColumn") {
            None => crate::fmt::FormatOptions::default().max_width,
            Some(value) => value
                .as_i64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|&n| n > 0)
                .ok_or("maxColumn must be a positive number")?,
        };
        let strings = |key: &str| -> Result<Vec<String>, String> {
            let Some(value) = config.get(key) else {
                return Ok(Vec::new());
            };
            let items = value
                .as_array()
                .ok_or_else(|| format!("{key} must be a list of strings"))?;
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{key} must be a list of strings"))
                })
                .collect()
        };
        let patterns = Patterns {
            include_paths: match config.has("project.includePaths") {
                true => Some(strings("project.includePaths")?),
                false => None,
            },
            include_filters: strings("project.includeFilters")?,
            exclude_paths: strings("project.excludePaths")?,
            exclude_filters: strings("project.excludeFilters")?,
        };
        let mut result = Config::new(path, max_column, patterns)?;

        let base = match config.get("runner") {
            Some(runner) => DialectSetting::default().with_runner(runner)?,
            None => DialectSetting::default(),
        };
        result.dialect = base.resolve()?;
        // `fileOverride`: each pattern's settings over the file's, the
        // longest pattern first, as in scalafmt.
        if let Some(overrides) = config.get("fileOverride") {
            let overrides = overrides
                .as_object()
                .ok_or("fileOverride must be an object")?;
            let mut keys: Vec<_> = overrides.keys().filter(|k| !k.is_empty()).collect();
            keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
            for key in keys {
                // Languages need scalafmt's project layout, which scruff
                // doesn't read.
                if key.starts_with("lang:") {
                    continue;
                }
                let setting = match &overrides[key.as_str()] {
                    value if value.as_str().is_some() => DialectSetting {
                        name: value.as_str().map(str::to_string),
                    },
                    value => match value.as_object().and_then(|o| o.get("runner")) {
                        Some(runner) => base.with_runner(runner)?,
                        None => base.clone(),
                    },
                };
                // `.sbt` stands for every file with that extension.
                let pattern = match key.strip_prefix('.') {
                    Some(_) => format!("glob:**{key}"),
                    None => key.clone(),
                };
                result
                    .dialect_overrides
                    .push((Matcher::path(&pattern)?, setting.resolve()?));
            }
        }
        Ok(result)
    }

    fn new(path: &Path, max_column: usize, patterns: Patterns) -> Result<Config, String> {
        let mut include = Vec::new();
        match patterns.include_paths {
            Some(paths) => {
                for pattern in paths {
                    include.push(Matcher::path(&pattern)?);
                }
            }
            // Include filters replace the default include paths, as in
            // scalafmt.
            None if patterns.include_filters.is_empty() => {
                for pattern in DEFAULT_INCLUDE_PATHS {
                    include.push(Matcher::path(pattern)?);
                }
            }
            None => {}
        }
        for pattern in patterns.include_filters {
            include.push(Matcher::filter(&pattern)?);
        }
        let mut exclude = Vec::new();
        let mut exclude_dirs = Vec::new();
        for pattern in patterns.exclude_paths {
            exclude.push(Matcher::path(&pattern)?);
            if pattern.starts_with("glob:")
                && let Some(dir) = pattern.strip_suffix("/**")
            {
                exclude_dirs.push(Matcher::path(dir)?);
            }
        }
        for pattern in patterns.exclude_filters {
            exclude.push(Matcher::filter(&pattern)?);
        }
        Ok(Config {
            path: path.to_path_buf(),
            max_column,
            include,
            exclude,
            exclude_dirs,
            dialect: Dialect::SCALA3,
            dialect_overrides: Vec::new(),
            lint_rules: Rule::defaults(),
        })
    }

    /// The dialect of `file`, or of files no `fileOverride` pattern matches
    /// when there's none, as for standard input.
    pub fn dialect_for(&self, file: Option<&Path>) -> Dialect {
        if let Some(file) = file {
            let path = match_text(file);
            if let Some((_, dialect)) = self
                .dialect_overrides
                .iter()
                .find(|(m, _)| m.0.is_match(&path))
            {
                return *dialect;
            }
        }
        self.dialect
    }

    /// Whether every file under `dir` is excluded, so a walk can skip it: a
    /// `glob:.../X/**` exclude path matches it as `glob:.../X`, as scalafmt
    /// prunes its walks.
    pub fn excludes_dir(&self, dir: &Path) -> bool {
        let path = match_text(dir);
        self.exclude_dirs.iter().any(|m| m.0.is_match(&path))
    }

    /// Whether a file with no configuration is one scruff formats when it
    /// finds it in a directory: a `.scala`, `.sbt`, `.sc` or `.mill` file.
    pub fn included_by_default(file: &Path) -> bool {
        file.extension()
            .is_some_and(|e| ["scala", "sbt", "sc", "mill"].iter().any(|x| e == *x))
    }

    /// Whether the project formats `file`: an include pattern matches its
    /// absolute path, and no exclude pattern does.
    pub fn includes(&self, file: &Path) -> bool {
        let path = match_text(file);
        self.include.iter().any(|m| m.0.is_match(&path))
            && !self.exclude.iter().any(|m| m.0.is_match(&path))
    }
}

/// Finds directories' configuration files, remembering what it found for
/// each directory on the way up, so directories that share parents look
/// at each one once.
#[derive(Default)]
pub struct Locator {
    found: HashMap<PathBuf, Option<PathBuf>>,
}

impl Locator {
    /// The nearest `.scruff.toml` in `dir` or a directory above it, or
    /// else the nearest `.scalafmt.conf`. A `.scruff.toml` above wins over
    /// a nearer `.scalafmt.conf`, such as one in a checked-out project.
    pub fn locate(&mut self, dir: &Path) -> Option<PathBuf> {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let mut visited = Vec::new();
        let mut scruff = None;
        let mut scalafmt = None;
        let mut known = None;
        for dir in dir.ancestors() {
            if let Some(found) = self.found.get(dir) {
                known = found.clone();
                break;
            }
            visited.push(dir.to_path_buf());
            let path = dir.join(SCRUFF_FILE);
            if path.is_file() {
                scruff = Some(path);
                break;
            }
            let path = dir.join(SCALAFMT_FILE);
            if scalafmt.is_none() && path.is_file() {
                scalafmt = Some(path);
            }
        }
        // What a directory above found is a `.scalafmt.conf` only when
        // there's no `.scruff.toml` above it.
        let known_is_scruff = known.as_ref().is_some_and(|p| p.ends_with(SCRUFF_FILE));
        let result = match (scruff, known) {
            (Some(path), _) => Some(path),
            (None, Some(path)) if known_is_scruff => Some(path),
            (None, known) => scalafmt.or(known),
        };
        for dir in visited {
            self.found.insert(dir, result.clone());
        }
        result
    }
}

/// The absolute path that patterns match against, with `/` separators on
/// every platform, since patterns are written with `/`.
fn match_text(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

/// Whether the file is a `.scruff.toml`, or another TOML file.
fn is_toml(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "toml")
}

impl Matcher {
    /// A `glob:` or `regex:` pattern, which must match the whole path, as
    /// Java's `PathMatcher` does.
    fn path(pattern: &str) -> Result<Matcher, String> {
        let regex = if let Some(glob) = pattern.strip_prefix("glob:") {
            glob_to_regex(glob).map_err(|e| format!("invalid glob {pattern:?}: {e}"))?
        } else if let Some(regex) = pattern.strip_prefix("regex:") {
            format!("^(?:{regex})$")
        } else {
            return Err(format!("{pattern:?} needs a `glob:` or `regex:` prefix"));
        };
        Regex::new(&regex)
            .map(Matcher)
            .map_err(|e| format!("invalid pattern {pattern:?}: {e}"))
    }

    /// A regex that must match somewhere in the path, as scalafmt's
    /// include and exclude filters do.
    fn filter(pattern: &str) -> Result<Matcher, String> {
        Regex::new(pattern)
            .map(Matcher)
            .map_err(|e| format!("invalid regex {pattern:?}: {e}"))
    }
}

/// A Java `PathMatcher` glob as a regex for the whole path, translated as
/// the JDK does: `*` and `?` stay within a directory, `**` crosses them,
/// `{a,b}` is either, `[...]` is a character class, with `!` to negate, and
/// `\` escapes.
fn glob_to_regex(glob: &str) -> Result<String, String> {
    let is_regex_meta = |c: char| ".^$+{[]|()\\*?&-~#".contains(c);
    let chars: Vec<char> = glob.chars().collect();
    let mut regex = String::from("^");
    let mut in_group = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let Some(&next) = chars.get(i) else {
                    return Err("ends with an escape".into());
                };
                i += 1;
                if is_regex_meta(next) {
                    regex.push('\\');
                }
                regex.push(next);
            }
            '[' => {
                // Any of these characters, but never a `/`.
                regex.push_str("[[^/]&&[");
                if chars.get(i) == Some(&'^') {
                    regex.push_str("\\^");
                    i += 1;
                } else {
                    if chars.get(i) == Some(&'!') {
                        regex.push('^');
                        i += 1;
                    }
                    if chars.get(i) == Some(&'-') {
                        regex.push('-');
                        i += 1;
                    }
                }
                let mut closed = false;
                while i < chars.len() {
                    let c = chars[i];
                    i += 1;
                    match c {
                        ']' => {
                            closed = true;
                            break;
                        }
                        '/' => return Err("a `/` in a character class".into()),
                        '\\' | '[' | '&' | '~' => {
                            regex.push('\\');
                            regex.push(c);
                        }
                        _ => regex.push(c),
                    }
                }
                if !closed {
                    return Err("a `[` without its `]`".into());
                }
                regex.push_str("]]");
            }
            '{' if in_group => return Err("nested `{`".into()),
            '{' => {
                regex.push_str("(?:(?:");
                in_group = true;
            }
            '}' if in_group => {
                regex.push_str("))");
                in_group = false;
            }
            ',' if in_group => regex.push_str(")|(?:"),
            '*' if chars.get(i) == Some(&'*') => {
                regex.push_str(".*");
                i += 1;
            }
            '*' => regex.push_str("[^/]*"),
            '?' => regex.push_str("[^/]"),
            c => {
                if is_regex_meta(c) {
                    regex.push('\\');
                }
                regex.push(c);
            }
        }
    }
    if in_group {
        return Err("a `{` without its `}`".into());
    }
    regex.push('$');
    Ok(regex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(Path::new("/p/.scalafmt.conf"), text).unwrap()
    }

    fn glob(pattern: &str, path: &str) -> bool {
        Regex::new(&glob_to_regex(pattern).unwrap())
            .unwrap()
            .is_match(path)
    }

    #[test]
    fn globs_follow_java() {
        // `**` crosses directories, `*` doesn't.
        assert!(glob("**.scala", "/p/src/A.scala"));
        assert!(glob("**/*.scala", "/p/src/A.scala"));
        assert!(!glob("*.scala", "/p/src/A.scala"));
        assert!(glob("/p/*.scala", "/p/A.scala"));
        assert!(!glob("/p/*.scala", "/p/src/A.scala"));
        // The whole path must match.
        assert!(!glob("**/src", "/p/src/A.scala"));
        assert!(glob("**/src/**", "/p/src/A.scala"));
        // Groups, classes and escapes.
        assert!(glob(
            "**/src/{main,test}/scala-3/**",
            "/p/src/test/scala-3/A.scala"
        ));
        assert!(!glob(
            "**/src/{main,test}/scala-3/**",
            "/p/src/it/scala-3/A.scala"
        ));
        assert!(glob("**/scala-[23]/*", "/p/scala-2/A.scala"));
        assert!(!glob("**/scala-[!23]/*", "/p/scala-2/A.scala"));
        assert!(glob("**/a?c", "/p/abc"));
        assert!(!glob("**/a?c", "/p/a/c"));
        assert!(glob("**/a\\*c", "/p/a*c"));
        assert!(!glob("**/a\\*c", "/p/abc"));
        // Regex characters are literal.
        assert!(glob("**/a+b.(c)", "/p/a+b.(c)"));
        assert!(!glob("**/a.c", "/p/abc"));
    }

    #[test]
    fn bad_globs_are_errors() {
        assert!(glob_to_regex("{a,{b}}").is_err());
        assert!(glob_to_regex("{a").is_err());
        assert!(glob_to_regex("[a").is_err());
        assert!(glob_to_regex("a\\").is_err());
    }

    #[test]
    fn defaults() {
        let c = config("");
        assert_eq!(c.max_column, 100);
        assert!(c.includes(Path::new("/p/src/A.scala")));
        assert!(c.includes(Path::new("/p/build.sbt")));
        assert!(!c.includes(Path::new("/p/src/A.java")));
    }

    #[test]
    fn max_column() {
        assert_eq!(config("maxColumn = 120").max_column, 120);
        assert!(Config::parse(Path::new("x"), "maxColumn = wide").is_err());
        assert!(Config::parse(Path::new("x"), "maxColumn = 0").is_err());
    }

    #[test]
    fn exclude_paths() {
        let c = config(
            r#"project.excludePaths = ["glob:**/contraband-scala/**", "regex:.*/gen/[^/]*\\.scala"]"#,
        );
        assert!(!c.includes(Path::new("/p/a/contraband-scala/A.scala")));
        assert!(!c.includes(Path::new("/p/gen/A.scala")));
        assert!(c.includes(Path::new("/p/gen/sub/A.scala")));
        assert!(c.includes(Path::new("/p/src/A.scala")));
    }

    #[test]
    fn excluded_directories() {
        let c = config(
            r#"project.excludePaths = ["glob:**/gen/**", "glob:**/*.java", "regex:.*/old/.*"]"#,
        );
        assert!(c.excludes_dir(Path::new("/p/a/gen")));
        assert!(!c.excludes_dir(Path::new("/p/a/gen2")));
        assert!(!c.excludes_dir(Path::new("/p/a")));
        // Only `glob:.../X/**` patterns name whole directories.
        assert!(!c.excludes_dir(Path::new("/p/old")));
    }

    #[test]
    fn include_paths_replace_the_default() {
        let c = config(r#"project.includePaths = ["glob:**/presentation-compiler/**/*.scala"]"#);
        assert!(c.includes(Path::new("/p/presentation-compiler/src/A.scala")));
        assert!(!c.includes(Path::new("/p/compiler/src/A.scala")));
    }

    #[test]
    fn filters_match_anywhere() {
        let c = config(r#"project.excludeFilters = ["\\Wsbt-test\\W"]"#);
        assert!(!c.includes(Path::new("/p/src/sbt-test/A.scala")));
        assert!(c.includes(Path::new("/p/src/sbt-tests/A.scala")));
        // Include filters replace the default include paths...
        let c = config(r#"project.includeFilters = ["/core/"]"#);
        assert!(c.includes(Path::new("/p/core/A.java")));
        assert!(!c.includes(Path::new("/p/other/A.scala")));
        // ...unless those are set as well.
        let c = config(
            r#"project.includeFilters = ["/core/"]
               project.includePaths = ["glob:**.scala"]"#,
        );
        assert!(c.includes(Path::new("/p/other/A.scala")));
    }

    fn scruff_config(text: &str) -> Result<Config, ConfigError> {
        Config::parse(Path::new("/p/.scruff.toml"), text)
    }

    #[test]
    fn scruff_toml_lint_table() {
        assert_eq!(scruff_config("").unwrap().lint_rules, Rule::defaults());
        let c =
            scruff_config("[lint]\nextend-select = [\"S002\"]\nignore = [\"no_return\"]").unwrap();
        let expected: Vec<Rule> = Rule::ALL
            .iter()
            .copied()
            .filter(|&r| (r.is_default() || r == Rule::NoVar) && r != Rule::NoReturn)
            .collect();
        assert_eq!(c.lint_rules, expected);
        let message = |text: &str| scruff_config(text).unwrap_err().message;
        assert_eq!(message("lint = 1"), "lint must be a table");
        assert_eq!(
            message("[lint]\nselect = \"S\""),
            "lint.select must be a list of strings"
        );
        assert!(message("[lint]\nenable = []").starts_with("unknown key lint.enable"));
        assert_eq!(
            message("[lint]\nselect = [\"S3\"]"),
            "lint: no rule is named or coded \"S3\""
        );
    }

    #[test]
    fn scruff_toml() {
        let c = scruff_config(
            r#"
            max-column = 120
            exclude-paths = ["glob:**/gen/**"]
            "#,
        )
        .unwrap();
        assert_eq!(c.max_column, 120);
        assert!(c.includes(Path::new("/p/src/A.scala")));
        assert!(!c.includes(Path::new("/p/gen/A.scala")));
        let c = scruff_config(r#"include-paths = ["glob:**/core/**.scala"]"#).unwrap();
        assert!(c.includes(Path::new("/p/core/src/A.scala")));
        assert!(!c.includes(Path::new("/p/other/A.scala")));
    }

    #[test]
    fn scruff_toml_defaults() {
        assert!(scruff_config("version = \"0.1.0\"").is_ok());
        let c = scruff_config("").unwrap();
        assert_eq!(c.max_column, 100);
        assert!(c.includes(Path::new("/p/build.sbt")));
        assert!(!c.includes(Path::new("/p/A.java")));
    }

    #[test]
    fn scruff_toml_errors() {
        let message = |text| scruff_config(text).unwrap_err().message;
        assert!(message("maxColumn = 120").contains("unknown key \"maxColumn\""));
        assert!(message("max-column = \"wide\"").contains("positive number"));
        assert!(message("version = 1").contains("must be a string"));
        assert!(message("exclude-paths = \"glob:**\"").contains("list of strings"));
        assert!(message("exclude-paths = [\"**/gen/**\"]").contains("prefix"));
        assert!(!message("max-column = ").is_empty());
    }

    #[test]
    fn scruff_toml_comes_first() {
        let dir = std::env::temp_dir().join(format!("scruff-config-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join(SCALAFMT_FILE), "maxColumn = 80").unwrap();
        // Only `.scalafmt.conf`.
        let found = Config::find(&sub).unwrap().unwrap();
        assert_eq!(found.max_column, 80);
        // Both: `.scruff.toml` alone counts, so its default width applies.
        std::fs::write(dir.join(SCRUFF_FILE), "").unwrap();
        let found = Config::find(&sub).unwrap().unwrap();
        assert_eq!(found.path, dir.join(SCRUFF_FILE));
        assert_eq!(found.max_column, 100);
        // A farther `.scruff.toml` wins over a nearer `.scalafmt.conf`.
        std::fs::write(sub.join(SCALAFMT_FILE), "maxColumn = 60").unwrap();
        assert_eq!(Config::find(&sub).unwrap().unwrap().max_column, 100);
        // A locator shared between directories finds the same files.
        let deeper = sub.join("a/b");
        std::fs::create_dir_all(&deeper).unwrap();
        let mut locator = Locator::default();
        assert_eq!(locator.locate(&deeper), Some(dir.join(SCRUFF_FILE)));
        assert_eq!(locator.locate(&sub.join("a")), Some(dir.join(SCRUFF_FILE)));
        assert_eq!(locator.locate(&dir), Some(dir.join(SCRUFF_FILE)));
        // The same, looking at the top directory first.
        let mut locator = Locator::default();
        assert_eq!(locator.locate(&dir), Some(dir.join(SCRUFF_FILE)));
        assert_eq!(locator.locate(&deeper), Some(dir.join(SCRUFF_FILE)));
        // With no `.scruff.toml`, the nearest `.scalafmt.conf`.
        std::fs::remove_file(dir.join(SCRUFF_FILE)).unwrap();
        let mut locator = Locator::default();
        assert_eq!(locator.locate(&dir), Some(dir.join(SCALAFMT_FILE)));
        assert_eq!(locator.locate(&deeper), Some(sub.join(SCALAFMT_FILE)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dialects() {
        let new = Dialect {
            star_wildcard_import: true,
            as_for_import_rename: true,
            question_mark_type_wildcard: true,
            postfix_star_vararg_splices: true,
            infix_postfix_star_vararg_splices: true,
            quiet_syntax: true,
            significant_indentation: true,
            fewer_braces: true,
            given_using: true,
            and_types: true,
            comma_separated_extends: true,
            leading_infix_operators: true,
        };
        let old = Dialect {
            star_wildcard_import: false,
            as_for_import_rename: false,
            question_mark_type_wildcard: false,
            postfix_star_vararg_splices: false,
            infix_postfix_star_vararg_splices: false,
            quiet_syntax: false,
            significant_indentation: false,
            fewer_braces: false,
            given_using: false,
            and_types: false,
            comma_separated_extends: false,
            leading_infix_operators: false,
        };
        let source3 = Dialect {
            infix_postfix_star_vararg_splices: false,
            quiet_syntax: false,
            significant_indentation: false,
            fewer_braces: false,
            given_using: false,
            and_types: false,
            comma_separated_extends: false,
            leading_infix_operators: false,
            ..new
        };
        assert_eq!(Dialect::named("scala3"), Some(new));
        assert_eq!(Dialect::named("Scala213Source3"), Some(source3));
        assert_eq!(Dialect::named("scala212source3"), Some(source3));
        assert_eq!(Dialect::named("scala213"), Some(old));
        assert_eq!(Dialect::named("sbt1"), Some(source3));
        // Only these five.
        for other in ["scala33", "scala212", "sbt0137", "scala4"] {
            assert_eq!(Dialect::named(other), None, "{other}");
        }
    }

    #[test]
    fn scalafmt_dialects() {
        let file = |p: &str| Some(PathBuf::from(p));
        // Scala 3 without a dialect.
        assert_eq!(
            config("").dialect_for(file("/p/A.scala").as_deref()),
            Dialect::SCALA3
        );
        // `dialectOverride` isn't read; the dialect goes with its name.
        for flag in [
            "allowStarWildcardImport",
            "allowAsForImportRename",
            "allowQuestionMarkAsTypeWildcard",
            "allowPostfixStarVarargSplices",
            "allowQuietSyntax",
            "allowSignificantIndentation",
            "allowFewerBraces",
            "allowGivenUsing",
        ] {
            let flipped = |name: &str, allow: bool| {
                config(&format!(
                    "runner.dialect = {name}\nrunner.dialectOverride.{flag} = {allow}"
                ))
                .dialect_for(None)
            };
            let named = |name| Dialect::named(name).unwrap();
            assert_eq!(flipped("scala3", false), named("scala3"), "{flag}");
            assert_eq!(flipped("scala213", true), named("scala213"), "{flag}");
        }
        let c = config(
            r#"
            runner.dialect = scala3
            fileOverride {
              "glob:**/scala-2/**" { runner.dialect = scala213 }
              "glob:**/src/main/scala-2/**" = scala213source3
              ".sbt" { runner.dialectOverride.allowQuietSyntax = false }
            }
            "#,
        );
        let dialect = |p: &str| c.dialect_for(file(p).as_deref());
        let named = |name| Dialect::named(name).unwrap();
        let base = dialect("/p/src/A.scala");
        assert_eq!(base, Dialect::SCALA3);
        // The longest matching pattern wins.
        assert_eq!(
            dialect("/p/src/main/scala-2/A.scala"),
            named("scala213source3")
        );
        assert_eq!(dialect("/p/src/test/scala-2/A.scala"), named("scala213"));
        // An override without a dialect keeps the one around it.
        assert_eq!(dialect("/p/build.sbt"), Dialect::SCALA3);
        // With no file, the dialect no pattern changes.
        assert_eq!(c.dialect_for(None), base);
    }

    #[test]
    fn dialect_errors() {
        let message = |text| Config::parse(Path::new("x"), text).unwrap_err().message;
        assert!(message("runner.dialect = scala4").contains("unknown dialect"));
        assert!(message("fileOverride { \".sbt\" = scala4 }").contains("unknown dialect"));
    }

    #[test]
    fn scruff_toml_dialect() {
        let c = scruff_config("dialect = \"scala213\"").unwrap();
        assert!(!c.dialect_for(None).star_wildcard_import);
        assert_eq!(
            scruff_config("").unwrap().dialect_for(None),
            Dialect::SCALA3
        );
        assert!(scruff_config("dialect = \"scala4\"").is_err());
    }

    #[test]
    fn patterns_need_a_kind() {
        let err = Config::parse(Path::new("x"), r#"project.excludePaths = ["**/gen/**"]"#);
        assert!(err.unwrap_err().message.contains("prefix"));
    }

    /// Every `.scalafmt.conf` in the corpus loads.
    #[test]
    fn corpus_configs_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("corpora");
        let mut found = 0;
        for project in ["sbt", "cats", "scala3"] {
            let path = root.join(project).join(SCALAFMT_FILE);
            if !path.is_file() {
                continue;
            }
            found += 1;
            let config = Config::load(&path).unwrap();
            let expected = if project == "sbt" { 100 } else { 120 };
            assert_eq!(config.max_column, expected, "{project}");
        }
        let cats = root.join("cats");
        if cats.join(SCALAFMT_FILE).is_file() {
            let config = Config::load(&cats.join(SCALAFMT_FILE)).unwrap();
            let dialect = |p: &str| config.dialect_for(Some(&cats.join(p)));
            // cats' `dialectOverride` flags aren't read.
            let named = |name| Dialect::named(name).unwrap();
            let main = dialect("core/src/main/scala/cats/Functor.scala");
            assert_eq!(main, named("scala213source3"));
            assert_eq!(
                dialect("core/src/main/scala-2.12/cats/A.scala"),
                named("scala212source3")
            );
            assert_eq!(
                dialect("core/src/test/scala-3/cats/A.scala"),
                Dialect::SCALA3
            );
            assert_eq!(dialect("build.sbt"), named("sbt1"));
        }
        if found > 0 {
            let sbt = Config::load(&root.join("sbt").join(SCALAFMT_FILE)).unwrap();
            let base = root.join("sbt");
            assert!(sbt.includes(&base.join("main/src/main/scala/sbt/Defaults.scala")));
            assert!(!sbt.includes(&base.join("sbt-app/src/sbt-test/a/b/A.scala")));
            assert!(!sbt.includes(&base.join("launcher-package/citest2/A.scala")));
        }
    }
}
