//! Runs the `scruff` binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const UNFORMATTED: &str = "object A:\n  def f(x:Int)=x+1\n";
const FORMATTED: &str = "object A:\n  def f(x: Int) = x + 1\n";

/// A file with `contents` in the test's own temporary directory.
fn file(name: &str, contents: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::write(&path, contents).unwrap();
    path
}

fn scruff(args: &[&str], stdin: &str) -> Output {
    scruff_in(Path::new(env!("CARGO_TARGET_TMPDIR")), args, stdin)
}

/// A new, empty directory for a test's project, outside this repository,
/// whose `.scruff.toml` would win over the project's `.scalafmt.conf`.
fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("scruff-cli-{}", std::process::id()))
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs scruff in `dir`.
fn scruff_in(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_scruff"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The files `--check` says would change, from its standard output, in
/// order, with `/` as the path separator.
fn reformatted(out: &Output) -> Vec<String> {
    paths(text(&out.stdout))
        .lines()
        .filter_map(|line| line.strip_suffix(": would be reformatted"))
        .map(|place| {
            // Drop the line and column.
            let mut parts = place.rsplitn(3, ':');
            parts.nth(2).unwrap_or(place).to_string()
        })
        .collect()
}

#[test]
fn format_overwrites_a_file() {
    let path = file("format_overwrites.scala", UNFORMATTED);
    let out = scruff(&["format", path.to_str().unwrap()], "");
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), "1 file reformatted\n");
    assert_eq!(text(&out.stderr), "");
    // Exactly the formatted text: no extra line break at the end.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FORMATTED);
}

#[test]
fn format_leaves_a_formatted_file_unwritten() {
    let path = file("format_unwritten.scala", FORMATTED);
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let out = scruff(&["format", path.to_str().unwrap()], "");
    assert!(out.status.success());
    let after = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert_eq!(before, after);
}

#[test]
fn format_reads_standard_input_and_prints() {
    let out = scruff(&["format", "-"], UNFORMATTED);
    assert!(out.status.success());
    // Exactly the formatted text: no extra line break at the end.
    assert_eq!(text(&out.stdout), FORMATTED);
    // Formatting its own output again changes nothing.
    let again = scruff(&["format", "-"], &text(&out.stdout));
    assert_eq!(text(&again.stdout), FORMATTED);
}

#[test]
fn fmt_is_an_alias_for_format() {
    let out = scruff(&["fmt", "-"], UNFORMATTED);
    assert!(out.status.success());
    assert_eq!(text(&out.stdout), FORMATTED);
}

#[test]
fn check_passes_on_a_formatted_file() {
    let path = file("check_formatted.scala", FORMATTED);
    let out = scruff(&["format", "--check", path.to_str().unwrap()], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "1 file already formatted\n");
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn check_shows_how_a_file_would_change() {
    let path = file("check_unformatted.scala", UNFORMATTED);
    let out = scruff(&["format", "--check", path.to_str().unwrap()], "");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        format!(
            "{}:2:11: would be reformatted\n  |\n1 | object A:\n  -   def f(x:Int)=x+1\n2 +   def f(x: Int) = x + 1\n  |\n\n1 file would be reformatted\n",
            path.display()
        )
    );
    assert_eq!(text(&out.stderr), "");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), UNFORMATTED);
}

#[test]
fn diff_prints_a_unified_diff_and_writes_nothing() {
    let dir = project("diff");
    std::fs::write(dir.join("A.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("B.scala"), FORMATTED).unwrap();
    let out = scruff_in(&dir, &["format", "--diff", "A.scala", "B.scala"], "");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        "--- A.scala\n+++ A.scala\n@@ -1,2 +1,2 @@\n object A:\n-  def f(x:Int)=x+1\n+  def f(x: Int) = x + 1\n\n"
    );
    // The count goes to standard error, so the diff can be piped.
    assert_eq!(
        text(&out.stderr),
        "1 file would be reformatted, 1 file already formatted\n"
    );
    assert_eq!(read(&dir, "A.scala"), UNFORMATTED);
    // Nothing to change: no diff, and exit with 0.
    let out = scruff_in(&dir, &["format", "--diff", "B.scala"], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "");
    // From standard input too.
    let out = scruff_in(&dir, &["format", "--diff", "-"], UNFORMATTED);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).starts_with("--- <stdin>\n"));
}

#[test]
fn the_count_says_what_changed() {
    let dir = project("count");
    std::fs::write(dir.join("A.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("B.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("C.scala"), FORMATTED).unwrap();
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert!(
        text(&out.stdout).ends_with("\n2 files would be reformatted, 1 file already formatted\n"),
        "{}",
        text(&out.stdout)
    );
    let out = scruff_in(&dir, &["format", "."], "");
    assert_eq!(
        text(&out.stdout),
        "2 files reformatted, 1 file left unchanged\n"
    );
    let out = scruff_in(&dir, &["format", "."], "");
    assert_eq!(text(&out.stdout), "3 files left unchanged\n");
}

#[test]
fn without_files_it_formats_the_current_directory() {
    let dir = repository("no_files", &[("src/A.scala", UNFORMATTED)]);
    let out = scruff_in(&dir, &["format"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "1 file reformatted\n");
    assert_eq!(read(&dir, "src/A.scala"), FORMATTED);
    // A directory with nothing to format says so, and isn't an error.
    let empty = project("no_files_empty");
    let out = scruff_in(&empty, &["format"], "");
    assert!(out.status.success());
    assert_eq!(
        text(&out.stderr),
        "warning: no files to format under the given paths\n"
    );
}

#[test]
fn quiet_leaves_out_the_count_and_silent_everything() {
    let dir = project("quiet");
    std::fs::write(dir.join("A.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("B.scala"), "object B {\n  def f( = 1\n").unwrap();
    let out = scruff_in(&dir, &["format", "--check", "-q", "."], "");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(reformatted(&out), ["A.scala"]);
    assert!(!text(&out.stdout).contains("1 file"));
    // Errors are part of `--check`'s report, so they still show.
    assert!(text(&out.stderr).starts_with("B.scala:2:"));
    // As in Ruff, they don't otherwise: only the exit code tells.
    let out = scruff_in(&dir, &["format", "--diff", "-q", "."], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stdout).starts_with("--- A.scala\n"));
    assert_eq!(text(&out.stderr), "");
    let out = scruff_in(&dir, &["format", "--check", "-q", "-"], "object B {\n");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stderr), "");
    let empty = project("quiet_empty");
    let out = scruff_in(&empty, &["format", "-q"], "");
    assert!(out.status.success());
    assert_eq!(text(&out.stderr), "");
    let out = scruff_in(&dir, &["format", "--check", "-s", "."], "");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    // A diff is the output asked for, so it shows even when silent.
    let out = scruff_in(&dir, &["format", "--diff", "-s", "A.scala"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).starts_with("--- A.scala\n"));
    assert_eq!(text(&out.stderr), "");
    assert_eq!(read(&dir, "A.scala"), UNFORMATTED);
    // Writing, nothing shows; A.scala is written all the same.
    let out = scruff_in(&dir, &["format", "-q", "."], "");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(text(&out.stderr), "");
    assert_eq!(read(&dir, "A.scala"), FORMATTED);
}

#[test]
fn syntax_errors_exit_with_2_and_say_where() {
    let source = "object A {\n  def f( = 1\n";
    let path = file("syntax_error.scala", source);
    let out = scruff(&["format", path.to_str().unwrap()], "");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    // The file is left as it was.
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with(&format!("{}:2:", path.display())),
        "{stderr}"
    );
    // With --check too: an error isn't "would be reformatted".
    let check = scruff(&["format", "--check", path.to_str().unwrap()], "");
    assert_eq!(check.status.code(), Some(2));
}

#[test]
fn a_missing_file_exits_with_2() {
    let out = scruff(&["format", "no-such-file.scala"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("can't read no-such-file.scala"));
}

#[test]
fn several_files_report_in_order_and_errors_win() {
    let dir = project("several_files");
    std::fs::write(dir.join("a.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("b.scala"), "object B {\n  def f( = 1\n").unwrap();
    std::fs::write(dir.join("c.scala"), UNFORMATTED).unwrap();
    let out = scruff_in(
        &dir,
        &["format", "--check", "a.scala", "b.scala", "c.scala"],
        "",
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(reformatted(&out), ["a.scala", "c.scala"]);
    let stdout = text(&out.stdout);
    assert!(
        stdout.ends_with("\n2 files would be reformatted\n"),
        "{stdout}"
    );
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("b.scala:2:"), "{stderr}");
}

#[test]
fn a_dash_with_files_is_an_error() {
    let path = file("dash_with_files.scala", FORMATTED);
    let out = scruff(&["format", "-", path.to_str().unwrap()], "");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn files_the_config_excludes_are_skipped() {
    let dir = project("excluded");
    std::fs::write(
        dir.join(".scalafmt.conf"),
        "maxColumn = 100\nproject.excludePaths = [\"glob:**/gen/**\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("gen")).unwrap();
    std::fs::write(dir.join("gen/A.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("B.scala"), UNFORMATTED).unwrap();
    let out = scruff_in(&dir, &["format", "--check", "gen/A.scala"], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stderr),
        "warning: no files to format under the given paths\n"
    );
    let out = scruff_in(&dir, &["format", "gen/A.scala", "B.scala"], "");
    assert!(out.status.success());
    let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap();
    assert_eq!(read("gen/A.scala"), UNFORMATTED);
    assert_eq!(read("B.scala"), FORMATTED);
}

#[test]
fn max_column_comes_from_the_config() {
    let dir = project("max_column");
    std::fs::write(dir.join(".scalafmt.conf"), "maxColumn = 20\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/A.scala"), "val x = f(alpha, beta)\n").unwrap();
    let out = scruff_in(&dir, &["format", "src/A.scala"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(dir.join("src/A.scala")).unwrap(),
        "val x = f(\n  alpha,\n  beta,\n)\n"
    );
    // Standard input follows the config for the current directory.
    let out = scruff_in(&dir, &["format", "-"], "val x = f(alpha, beta)\n");
    assert_eq!(text(&out.stdout), "val x = f(\n  alpha,\n  beta,\n)\n");
}

#[test]
fn a_broken_config_exits_with_2_and_names_it() {
    let dir = project("broken_config");
    std::fs::write(dir.join(".scalafmt.conf"), "maxColumn = wide\n").unwrap();
    std::fs::write(dir.join("A.scala"), UNFORMATTED).unwrap();
    let out = scruff_in(&dir, &["format", "A.scala"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr).contains(".scalafmt.conf: maxColumn"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("A.scala")).unwrap(),
        UNFORMATTED
    );
}

#[test]
fn scruff_toml_comes_before_scalafmt_conf() {
    let dir = project("scruff_toml");
    std::fs::write(dir.join(".scalafmt.conf"), "maxColumn = 100\n").unwrap();
    std::fs::write(dir.join(".scruff.toml"), "max-column = 20\n").unwrap();
    std::fs::write(dir.join("A.scala"), "val x = f(alpha, beta)\n").unwrap();
    let out = scruff_in(&dir, &["format", "A.scala"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        std::fs::read_to_string(dir.join("A.scala")).unwrap(),
        "val x = f(\n  alpha,\n  beta,\n)\n"
    );
}

/// A project in its own git repository, with `files` at their paths.
fn repository(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = project(name);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    for (path, contents) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

/// `text` with Windows' `\\` path separators as `/`, for comparing paths
/// scruff prints on any platform.
fn paths(text: String) -> String {
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    }
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

#[test]
fn a_directory_stands_for_its_scala_files() {
    let dir = repository(
        "walk",
        &[
            ("build.sbt", UNFORMATTED),
            ("src/main/scala/A.scala", UNFORMATTED),
            ("src/main/java/B.java", UNFORMATTED),
            (".hidden/C.scala", UNFORMATTED),
            ("target/D.scala", UNFORMATTED),
            (".gitignore", "target/\n"),
        ],
    );
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(reformatted(&out), ["build.sbt", "src/main/scala/A.scala"]);
    let out = scruff_in(&dir, &["format", "src"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(read(&dir, "src/main/scala/A.scala"), FORMATTED);
    assert_eq!(read(&dir, "src/main/java/B.java"), UNFORMATTED);
    assert_eq!(read(&dir, "build.sbt"), UNFORMATTED);
}

#[test]
fn a_walk_follows_the_config_and_skips_excluded_directories() {
    let dir = repository(
        "walk_config",
        &[
            (
                ".scruff.toml",
                "include-paths = [\"glob:**.scala\", \"glob:**.txt\"]\nexclude-paths = [\"glob:**/gen/**\"]\n",
            ),
            ("A.scala", UNFORMATTED),
            ("notes.txt", UNFORMATTED),
            ("build.sbt", UNFORMATTED),
            ("gen/B.scala", UNFORMATTED),
        ],
    );
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert_eq!(reformatted(&out), ["A.scala", "notes.txt"]);
}

#[test]
fn a_walk_skips_an_excluded_directory_with_its_own_config() {
    let dir = repository(
        "walk_nested_config",
        &[
            (".scruff.toml", "exclude-paths = [\"glob:**/io/**\"]\n"),
            ("A.scala", UNFORMATTED),
            ("io/.scalafmt.conf", "maxColumn = 100\n"),
            ("io/B.scala", "object B { val b = null }\n"),
        ],
    );
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert_eq!(reformatted(&out), ["A.scala"]);
    let out = scruff_in(&dir, &["check", "."], "");
    assert!(!text(&out.stdout).contains("io/"), "{}", text(&out.stdout));
}

#[test]
fn a_file_named_twice_is_formatted_once() {
    let dir = repository("named_twice", &[("src/A.scala", UNFORMATTED)]);
    let out = scruff_in(
        &dir,
        &["format", "--check", "src", "src/A.scala", "./src"],
        "",
    );
    assert_eq!(reformatted(&out), ["src/A.scala"]);
}

#[test]
fn a_broken_config_is_reported_once() {
    let dir = repository(
        "broken_walk",
        &[
            (".scalafmt.conf", "maxColumn = wide\n"),
            ("A.scala", UNFORMATTED),
            ("B.scala", UNFORMATTED),
        ],
    );
    let out = scruff_in(&dir, &["format", "."], "");
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(".scalafmt.conf: maxColumn"), "{stderr}");
    assert_eq!(read(&dir, "A.scala"), UNFORMATTED);
}

#[cfg(unix)]
#[test]
fn a_file_and_a_link_to_it_are_formatted_once() {
    let dir = repository("linked", &[("src/A.scala", UNFORMATTED)]);
    std::os::unix::fs::symlink("src/A.scala", dir.join("B.scala")).unwrap();
    let out = scruff_in(&dir, &["format", "--check", "src/A.scala", "B.scala"], "");
    assert_eq!(reformatted(&out), ["src/A.scala"]);
    // A walk doesn't follow links.
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert_eq!(reformatted(&out), ["src/A.scala"]);
}

#[test]
fn imports_are_rewritten_to_what_the_dialect_allows() {
    let dir = repository(
        "rewrite",
        &[
            (
                ".scalafmt.conf",
                "runner.dialect = scala3\nfileOverride { \".sbt\" = scala213 }\n",
            ),
            ("A.scala", "import a._\nimport b.{c => d}\n"),
            ("build.sbt", "import a._\n"),
        ],
    );
    let out = scruff_in(&dir, &["format", "."], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(read(&dir, "A.scala"), "import a.*\nimport b.{ c as d }\n");
    assert_eq!(read(&dir, "build.sbt"), "import a._\n");
    // Standard input follows the dialect no pattern changes.
    let out = scruff_in(&dir, &["format", "-"], "import a._\n");
    assert_eq!(text(&out.stdout), "import a.*\n");
}

#[test]
fn without_a_dialect_it_is_scala_3() {
    let dir = repository("no_dialect", &[("A.scala", "import a._\n")]);
    let out = scruff_in(&dir, &["format", "A.scala"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(read(&dir, "A.scala"), "import a.*\n");
}

#[test]
fn a_scala_2_dialect_keeps_imports() {
    let dir = repository(
        "scala_2",
        &[
            (".scruff.toml", "dialect = \"scala213\"\n"),
            ("A.scala", "import a._\n"),
        ],
    );
    let out = scruff_in(&dir, &["format", "A.scala"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(read(&dir, "A.scala"), "import a._\n");
}

#[test]
fn color_is_on_only_when_asked_or_on_a_terminal() {
    let dir = project("color");
    std::fs::write(dir.join("A.scala"), UNFORMATTED).unwrap();
    std::fs::write(dir.join("B.scala"), "object B {\n  def f( = 1\n").unwrap();
    let has_color = |out: &Output| text(&out.stdout).contains('\x1b');
    let out = scruff_in(&dir, &["format", "--check", "--color", "always", "."], "");
    assert!(has_color(&out));
    assert!(text(&out.stderr).contains('\x1b'));
    // Piped, as here, it's off unless asked for.
    let out = scruff_in(&dir, &["format", "--check", "."], "");
    assert!(!has_color(&out));
    let out = scruff_in(&dir, &["format", "--diff", "--color", "never", "."], "");
    assert!(!has_color(&out));
    // The formatted text from standard input is never colored.
    let out = scruff_in(&dir, &["format", "--color", "always", "-"], UNFORMATTED);
    assert_eq!(text(&out.stdout), FORMATTED);
}

#[test]
fn check_reports_each_problem_in_order_and_exits_with_1() {
    let dir = repository(
        "check_returns",
        &[
            (
                "A.scala",
                "object A:\n  def f(x: Int): Int =\n    if x > 0 then return 1\n    return 0\n",
            ),
            ("B.scala", "object B:\n  var x = 1\n"),
            (".scruff.toml", "[lint]\nextend-select = [\"no_var\"]\n"),
        ],
    );
    let out = scruff_in(&dir, &["check", "--output-format", "concise"], "");
    assert_eq!(out.status.code(), Some(1));
    let no_return = "S001 no_return `return` is not allowed; make the result the last expression";
    let no_var = "S002 no_var `var` is not allowed; use a `val`";
    assert_eq!(
        text(&out.stdout),
        format!(
            "A.scala:3:19: {no_return}\nA.scala:4:5: {no_return}\nB.scala:2:3: {no_var}\nFound 3 problems.\n"
        )
    );
}

const SBT_IN: &str = "fork in run := true\na in Compile in core := 1\n";
const SBT_SLASH: &str = "run / fork := true\nCompile / a in core := 1\n";
const SBT_LINT: &str = "[lint]\nextend-select = [\"sbt_0_13_syntax\"]\n";

#[test]
fn check_counts_what_fix_would_fix() {
    let dir = repository(
        "check_fixable",
        &[("build.sbt", SBT_IN), (".scruff.toml", SBT_LINT)],
    );
    let out = scruff_in(&dir, &["check", "--output-format", "concise"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        text(&out.stdout).ends_with("Found 3 problems.\n[*] 2 fixable with the `--fix` option.\n"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(read(&dir, "build.sbt"), SBT_IN);
}

#[test]
fn check_fix_overwrites_files_and_reports_what_is_left() {
    let dir = repository(
        "check_fix",
        &[("build.sbt", SBT_IN), (".scruff.toml", SBT_LINT)],
    );
    let out = scruff_in(&dir, &["check", "--fix", "--output-format", "concise"], "");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(read(&dir, "build.sbt"), SBT_SLASH);
    let stdout = text(&out.stdout);
    assert!(
        stdout.starts_with("build.sbt:2:13: S009 sbt_0_13_syntax"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("Found 3 problems (2 fixed, 1 remaining).\n"),
        "{stdout}"
    );

    // With nothing left, it passes.
    std::fs::write(dir.join("build.sbt"), "fork in run := true\n").unwrap();
    let out = scruff_in(&dir, &["check", "--fix"], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(read(&dir, "build.sbt"), "run / fork := true\n");
    assert_eq!(
        text(&out.stdout),
        "Found 1 problem (1 fixed, 0 remaining).\n"
    );
}

#[test]
fn check_fix_on_standard_input_prints_the_fixed_text() {
    let dir = repository("check_fix_stdin", &[(".scruff.toml", SBT_LINT)]);
    let out = scruff_in(
        &dir,
        &["check", "--fix", "--output-format", "concise", "-"],
        SBT_IN,
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), SBT_SLASH);
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("<stdin>:2:13: S009"), "{stderr}");
    assert!(
        stderr.ends_with("Found 3 problems (2 fixed, 1 remaining).\n"),
        "{stderr}"
    );
}

#[test]
fn check_passes_without_problems() {
    let out = scruff(&["check", "-"], "object A:\n  def f(x: Int) = x + 1\n");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "All checks passed!\n");
}

#[test]
fn check_refuses_syntax_errors_with_2() {
    let out = scruff(&["check", "-"], "object A {\n  def f( = return 1\n");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(text(&out.stdout), "");
    assert!(text(&out.stderr).contains("<stdin>: not checked because of syntax errors"));
}

#[test]
fn check_runs_only_default_rules_without_a_lint_table() {
    let dir = repository("check_defaults", &[("A.scala", "object A:\n  var x = 1\n")]);
    let out = scruff_in(&dir, &["check"], "");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "All checks passed!\n");
}

#[test]
fn check_follows_select_and_ignore() {
    let source = "object A:\n  var x = 1\n  def f: Int = return x\n";
    let dir = repository(
        "check_select",
        &[
            ("A.scala", source),
            (
                ".scruff.toml",
                "[lint]\nselect = [\"S\"]\nignore = [\"S001\"]\n",
            ),
        ],
    );
    let out = scruff_in(&dir, &["check", "--output-format", "concise"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stdout).starts_with("A.scala:2:3: S002 no_var"));
    assert!(!text(&out.stdout).contains("S001"));
    // Standard input follows the current directory's configuration.
    let out = scruff_in(&dir, &["check", "--output-format", "concise", "-"], source);
    assert!(text(&out.stdout).starts_with("<stdin>:2:3: S002 no_var"));
    assert!(!text(&out.stdout).contains("S001"));
}

#[test]
fn select_on_the_command_line_replaces_the_configs() {
    let source = "object A:\n  var x = 1\n  def f: Int = return x\n  val y = null\n";
    let dir = repository(
        "check_select_flag",
        &[
            ("A.scala", source),
            (
                ".scruff.toml",
                "[lint]\nextend-select = [\"no_var\"]\nignore = [\"S001\"]\n",
            ),
        ],
    );
    let codes = |out: &Output| -> Vec<String> {
        text(&out.stdout)
            .lines()
            .filter_map(|line| Some(line.split(' ').nth(1)?.to_string()))
            .filter(|code| code.len() == 4)
            .collect()
    };
    let concise = ["check", "--output-format", "concise"];
    let out = scruff_in(&dir, &concise, "");
    assert_eq!(codes(&out), ["S002", "S004"]);
    // The config's ignore goes too, as in Ruff.
    let out = scruff_in(
        &dir,
        &[&concise[..], &["--select", "S001,no_null"]].concat(),
        "",
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(codes(&out), ["S001", "S004"]);
    let out = scruff_in(
        &dir,
        &[
            &concise[..],
            &["--select", "no_return", "--select", "S002", "-"],
        ]
        .concat(),
        source,
    );
    assert_eq!(codes(&out), ["S002", "S001"]);
    let out = scruff_in(&dir, &["check", "--select", "C"], "");
    assert_eq!(out.status.code(), Some(0));
    // `--ignore` turns rules off on top of the config's selection.
    let out = scruff_in(&dir, &[&concise[..], &["--ignore", "no_var"]].concat(), "");
    assert_eq!(codes(&out), ["S004"]);
    // And on top of `--select`.
    let out = scruff_in(
        &dir,
        &[&concise[..], &["--select", "S", "--ignore", "S002,S004"]].concat(),
        "",
    );
    assert_eq!(codes(&out), ["S001"]);
    let out = scruff_in(
        &dir,
        &[&concise[..], &["--ignore", "S002", "--ignore", "S004", "-"]].concat(),
        source,
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(codes(&out), Vec::<String>::new());
}

#[test]
fn an_unknown_rule_in_select_or_ignore_exits_with_2() {
    let dir = repository("check_select_unknown", &[("A.scala", "object A\n")]);
    for (flag, entries) in [("--select", "no_var,S9"), ("--ignore", "S9")] {
        let out = scruff_in(&dir, &["check", flag, entries], "");
        assert_eq!(out.status.code(), Some(2));
        assert!(
            text(&out.stderr).contains(&format!(r#"{flag}: no rule is named or coded "S9""#)),
            "{}",
            text(&out.stderr)
        );
    }
}

#[test]
fn an_unknown_rule_in_the_config_exits_with_2() {
    let dir = repository(
        "check_unknown_rule",
        &[
            ("A.scala", "object A\n"),
            (".scruff.toml", "[lint]\nignore = [\"no_such_rule\"]\n"),
        ],
    );
    let out = scruff_in(&dir, &["check"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        text(&out.stderr)
            .contains(r#".scruff.toml: lint: no rule is named or coded "no_such_rule""#),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn check_help_lists_each_rule_on_its_own_line() {
    let out = scruff(&["check", "--help"], "");
    let help = text(&out.stdout);
    assert!(
        help.contains("\n  S001  no_return ") && help.contains("\n  S002  no_var "),
        "{help}"
    );
    assert!(
        help.lines()
            .any(|line| line.starts_with("  S001") && line.ends_with("(default)")),
        "{help}"
    );
}

#[test]
fn a_scruff_ignore_comment_hides_a_problem() {
    let source = "\
object A:
  val a: String = null // scruff: ignore(S004)
  // scruff: ignore(no_null)
  val b: String = null
  val c: String = null
";
    let out = scruff(&["check", "--output-format", "concise", "-"], source);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        "<stdin>:5:19: S004 no_null `null` is not allowed; use an `Option`\nFound 1 problem.\n"
    );
}

#[test]
fn check_shows_each_problem_in_full_by_default() {
    let out = scruff(&["check", "-"], "val s: String = null\n");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        "S004 no_null `null` is not allowed; use an `Option`\n --> <stdin>:1:17\n  |\n1 | val s: String = null\n  |                 ^^^^\n\nFound 1 problem.\n"
    );
}
