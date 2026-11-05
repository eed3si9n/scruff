# scruff fmt

## Synopsis

`scruff` `fmt` \[ _options_ \] \[ _file_ | _dir..._ | `-` \]

Formats the given files and directories in place, or the current directory when none are given. `-` reads standard input and prints the result. `scruff format` is the same command.

| Option                        | Effect                                                                                      |
| ----------------------------- | ------------------------------------------------------------------------------------------- |
| `--check`                     | Write nothing; show how each file would change, and exit with 1 if any would                |
| `--diff`                      | Write nothing; print a unified diff of each change, and exit with 1 if any would            |
| `-q`, `--quiet`               | Print only the diffs: no file count, no warnings, errors only with `--check`                |
| `-s`, `--silent`              | Print nothing but the formatted text or the diff; errors show only in the exit code         |
| <nobr>`--color` _when_</nobr> | `auto` (default), `always` or `never`; `auto` colors on a terminal unless `NO_COLOR` is set |

## Scruff formatter

Scruff formatter is a fast and normalizing Scala code formatter, written in Rust.
Inspired by Python's code formatters like [Black](https://pypi.org/project/black/) and [Ruff](https://docs.astral.sh/ruff/formatter/),
Scruff removes all configurations related to styling.

Scruff formatter is intended to be normalizing, which means that the _scruffed_ code
looks the same regardless of how it started.

## Philosophy

### Philosophy: normalize

Inspired by Python code formatters [Black](https://pypi.org/project/black/) and [Ruff](https://docs.astral.sh/ruff/formatter/), Scruff is a normalizing code formatter. This means that the _scruffed up_ code looks the same regardless of how it started. Here's an example:

```scala
// original input
object A:
  val a = List(1, 2)
  val b = List(
    1,
    2
  )
  val c =
    List(1, 2)
```

Using Scalafmt 3.11.5, which is the defacto standard formatter for Scala, the above code is kept as-is after formatting. Scruff ignores previous formatting, and applies uniform whitespace as follows:

```scala
// scruffed
object A:
  val a = List(1, 2)
  val b = List(1, 2)
  val c = List(1, 2)
```

### Philosophy: fast

Built on top of [Tree-Sitter Scala](https://eed3si9n.com/fast-scala3-parsing-with-tree-sitter/) using Rust, Scruff parses and formats the Scala code in parallel, in native code. When formatting unformatted 207k sloc scala/scala3 repo, Scruff runs in **1.50s**:

- Scruff 0.1.0 (with 12 core): 1.50s (**138.6** ksloc/s)

So out-of-box, Scruff is about 6x faster than Scalafmt 3.11.5 to format code mostly due to the default parallelism. Interestingly, on code that's already formatted, Scruff finishes in **0.60s**. This is because when a file's output matches its input, Scruff skips re-parsing it and the equivalence check, so each file is parsed only once:

- Scruff 0.1.0 (with 12 core): 0.60s (**339.0** ksloc/s)
- Scalafmt 3.11.5 (with 1 core): 8.09s (**25.2** ksloc/s)
- Scalafmt 3.11.5 (`scalafmtParallelism := 12`): 1.81s (**112.7** ksloc/s)

This means that Scruff would be 3 ~ 10x faster on CI, depending on how Scalafmt is configured.

### Philosophy: lunch A or lunch B

Scruff 0.1.0 offers minimal configurations:

```toml
# .scruff.toml

max-column = 100
dialect = "scala3"

exclude-paths = [
  "glob:**/src/sbt-test/**",
]
```

- `dialect = "scala3"` rewrites to maximalist Scala 3, with significant spaces, fewer braces, and `then` control.
- `dialect = "scala213"` or `dialect = "scala213source3"` rewrites to curly-braces.

There are no other knobs.
