# Scruff

## Scruff formatter

Scruff is a fast and normalizing Scala code formatter, written in Rust.
Inspired by Python's code formatters like [Black](https://pypi.org/project/black/) and [Ruff](https://docs.astral.sh/ruff/formatter/),
Scruff removes all configurations related to styling.

Scruff is intended to be normalizing, which means that the _scruffed_ code
looks the same regardless of how it started.

## Usage

The native binaries are published to GitHub Releases <https://github.com/eed3si9n/scruff/releases/tag/v0.1.0>. For convenience, We've also created an sbt plugin that adds `scruffFmt`, `scruffFmtCheck`, and `scruffFmtOnCompile`.

```scala
// project/plugins.sbt
addSbtPlugin("com.eed3si9n" % "sbt-scruff" % "0.1.0")
```

The first time you call `scruffFmt`, it will download the binary:

```bash
sbt:scruff-root> scruffFmt
[info] downloading scruff 0.1.0 for universal-apple-darwin
[info] 4 files left unchanged
[success] elapsed time: 1 s
```

To enable formatting on-compile:

```scala
ThisBuild / scruffFmtOnCompile := true
```

### GitHub Actions

Instead of calling `scruffFmtCheck`, you can run the scruff CLI on GitHub Actions as follows:

```yaml
    - uses: actions/checkout@v7
    # add this immediately after actions/checkout
    - uses: eed3si9n/scruff@b3030eb89b4bf1a60a130966c971d6d49beed7b8
    - uses: actions/setup-java@v6
      with:
        distribution: "${{ matrix.distribution }}"
        java-version: "${{ matrix.java }}"
        cache: "sbt"
```

This action finishes in 1 to 2 seconds, faster than setting up JDK.

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

## License

Scruff is provided under MIT License.
