# scruff check

## Synopsis

`scruff` `check` \[ _options_ \] \[ _file_ | _dir..._ | `-` \]

Scruff linter (`check`) is an experimental syntactic Scala linter. It checks the given files and directories against the lint rules, or the current directory when none are given. `-` reads standard input. Files are found as `scruff fmt` finds them, following the same include and exclude paths. Exits with 1 if a rule flags anything, or with `--fix` if anything is left, and with 2 if a file can't be checked because it can't be read or written or has syntax errors.

| Option                               | Effect                                                                                                                              |
| ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| <nobr>`--output-format` _how_</nobr> | `full` (default) shows each problem with the lines around it; `concise` prints one line each, as `file:line:col: code rule message` |
| `--fix`                              | fixes what the rules can, overwriting each file, and reports what's left; on `-`, prints the fixed text and reports to stderr       |
| `--select` _rules_                   | runs these rules in place of what `[lint]` selects, `ignore` included; names, codes or starts of codes, separated by commas         |
| `--ignore` _rules_                   | turns these rules off, on top of `--select` or what `[lint]` selects; separated by commas                                           |
| `--color` _when_                     | `auto` (default), `always` or `never`; `auto` colors on a terminal unless `NO_COLOR` is set                                         |

### Micro benchmark

Using Scalafix 0.14.9 as a reference, Scruff linter runs about 10 to 25x faster, depending on the codebase. For example, here's wall-clock time to check for `null` and `return`, including startup, on a 12-core arm64 machine:

| Codebase                 | Scruff 0.2.2         | Scalafix 0.14.9    |
| ------------------------ | -------------------- | ------------------ |
| Cats (52k sloc)          | 0.12 s (433k sloc/s) | 3.1 s (17k sloc/s) |
| scala/scala3 (207k sloc) | 0.37 s (555k sloc/s) | 6.2 s (34k sloc/s) |

Here's wall-clock time to organize imports, also on a 12-core arm64 machine:

| Codebase                 | Scruff 0.2.2 `--fix` | Scalafix 0.14.9    |
| ------------------------ | -------------------- | ------------------ |
| Cats (52k sloc)          | 0.12 s (433k sloc/s) | 2.7 s (19k sloc/s) |
| scala/scala3 (207k sloc) | 0.54 s (383k sloc/s) | 5.4 s (38k sloc/s) |

Scalafix's time doesn't include compiling with SemanticDB first, which its `OrganizeImports` needs. Similar to Scalafmt, the majority of the difference might be due to parallelism.

## Configuration

The set of enabled rules is controlled via the `lint.select`, `lint.extend-select`, and `lint.ignore` settings.

```toml
# .scruff.toml

version = "0.2.2"

[lint]
# default rules
# select = [
#   "untyped_implicit_val",
#   "no_return",
#   "no_null",
#   "no_xml",
#   "procedure_syntax",
# ]
```

Scruff currently selects `untyped_implicit_val`, `no_return`, `no_null`, `no_xml`, and `procedure_syntax` rules by default; see [Rules](#rules).

To extend the default, use `exend-select`, and to opt out from defaults use `ignore` as follows:

```toml
# .scruff.toml

version = "0.2.2"

[lint]
extend-select = ["no_default_param"]

ignore = ["no_null"]
```

To try other rules without editing the configuration, `--select` on the command line replaces what `[lint]` selects, as in Ruff. It takes the same entries, separated by commas, and the configuration's `ignore` doesn't apply. `--ignore` then turns rules off, on top of `--select` or of the configuration:

```bash
$ scruff check --select no_var,S004
$ scruff check --select ALL --ignore S003,S006
$ scruff check --ignore no_null
```

## Usage

The main usage pattern is to run the `scruff` CLI either from CI or during the build. See the [Setup](setup.md) page.

Because it typically takes 1 ~ 2 second to run the GitHub Action, including the time it takes to download the CLI, we recommend running that ahead of the normal build process.

In the local environment, sbt-scruff provides `scruff` task, which can call `scruff check`:

```bash
sbt:vimquit-root> scruff check
[info] All checks passed!
[success] elapsed time: 0 s
```

Here's what an error looks like:

```scala
sbt:sbt-vimquit-root> scruff check
[info] S004 no_null `null` is not allowed; use an `Option`
[info]   --> plugin/src/main/scala/sbtvimquit/VimquitPlugin.scala:11:11
[info]    |
[info]  9 |   override def trigger = allRequirements
[info] 10 |
[info] 11 |   var x = null
[info]    |           ^^^^
[info] 12 |
[info] 13 |   lazy val quitParser: Parser[Unit] = (":q": Parser[String]) ^^^ (())
[info]    |
[info] Found 1 problem.
[error] scruff exited with 1
[error] (ThisBuild / scruff) scruff exited with 1
[error] elapsed time: 0 s
```

## Fixes

Some rules can fix what they flag. Without `--fix`, the count says how many:

```bash
$ scruff check
...
Found 3 problems.
[*] 2 fixable with the `--fix` option.
```

`--fix` makes those edits, writes the files, and reports the problems left:

```bash
$ scruff check --fix
...
Found 3 problems (2 fixed, 1 remaining).
```

To opt out of rules for a line or a whole file, see [Error suppression](#error-suppression).

To migrate a build from sbt 0.13's `key in Scope` to the slash syntax, see [S009 sbt_0_13_syntax](S009_sbt_0_13_syntax.md).

## Error suppression

### Line-level

To suppress the lint error, write a comment next to or above the use site in the format of `scruff: ignore(no_null)`:

```scala
object A:
  val a: String = null // scruff: ignore(no_null)
  val b: String = null // scruff: ignore(S004)
  var c: String = null // scruff: ignore(no_var, no_null)

  // scruff: ignore(ALL)
  var d: String = null
```

### File-level

To suppress lint errors in the whole file, write `scruff: file_ignore(...)` with the rules to turn off, or `scruff: noqa` to turn off every rule, preferably towards the top of the file:

```scala
// scruff: file_ignore(no_null, S002)
package example

object A:
  var a: String = null
```

```scala
// scruff: noqa
package example.generated
```

## Rules

The initial of a rule's code is its category: `C` correctness, `S` style.

| Code                             | Rule                   | Flags                                                 | Default | Fix |
| -------------------------------- | ---------------------- | ----------------------------------------------------- | ------- | --- |
| C001                             | `untyped_implicit_val` | `implicit val` members without a type                 | ✓       |     |
| S001                             | `no_return`            | `return` statements                                   | ✓       |     |
| S002                             | `no_var`               | `var` definitions, declarations and class parameters  |         |     |
| S003                             | `no_default_param`     | default values of parameters                          |         |     |
| S004                             | `no_null`              | `null`                                                | ✓       |     |
| S005                             | `no_while`             | `while` and `do … while` loops                        |         |     |
| S006                             | `no_asinstanceof`      | casts with `asInstanceOf`                             |         |     |
| S007                             | `no_xml`               | XML literals and patterns                             | ✓       |     |
| [S008](S008_procedure_syntax.md) | `procedure_syntax`     | `def f() { ... }` and `def f()` without a result type | ✓       | ✓   |
| [S009](S009_sbt_0_13_syntax.md)  | `sbt_0_13_syntax`      | sbt 0.13's `key in Scope`                             |         | ✓   |
| [S010](S010_organize_imports.md) | `organize_imports`     | imports out of order                                  |         | ✓   |
| [S011](S011_finally_close.md)    | `finally_close`        | `try { ... } finally x.close()`                       |         | ✓   |
