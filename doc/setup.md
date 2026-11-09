# Setup

The native binaries are published to GitHub Releases <https://github.com/eed3si9n/scruff/releases/tag/v{{scruff_version}}>.

## `.scruff.toml`

`.scruff.toml` is used by the `scruff` CLI for its configuration.

```toml
version = "{{scruff_version}}"
dialect = "scala3"

exclude-paths = [
  "glob:**/src/sbt-test/**",
]

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

Both GitHub Action and sbt-scruff also use `.scruff.toml` for the scruff version. In other words, it would follow the version specified in `.scruff.toml` ({{scruff_version}}) even if the sbt-scruff version is 0.2.0.

## Setting up GitHub Actions

Instead of calling `scruffFmtCheck`, you can run the `scruff` CLI on GitHub Actions as follows:

```yaml
- uses: actions/checkout@v7
# add this immediately after actions/checkout
- uses: eed3si9n/scruff@v{{scruff_version}}
  with:
    format: true
    lint: true
- uses: actions/setup-java@v6
  with:
    distribution: "${{ matrix.distribution }}"
    java-version: "${{ matrix.java }}"
    cache: "sbt"
```

By default the action runs both `scruff fmt --check` and `scruff check`. This action typically finishes in 1 to 2 seconds, faster than setting up JDK.

## sbt-scruff

For convenience, We've also created an sbt plugin that adds `scruff`, `scruffFmt`, `scruffFmtCheck`, `scruffFmtOnCompile`, and `scruffCheckOnCompile`.

```scala
// project/plugins.sbt
addSbtPlugin("com.eed3si9n" % "sbt-scruff" % "0.3.0")
```

The first time you call `scruffFmt`, it will download the binary:

```bash
sbt:scruff-root> scruffFmt
[info] downloading scruff {{scruff_version}} for universal-apple-darwin
[info] 4 files left unchanged
[success] elapsed time: 1 s
```

`scruff` runs the scruff CLI in the build's root with the arguments you give it, and fails if scruff exits with an error. For example, to check the build against the lint rules (scruff 0.2.2 or later):

```bash
sbt:scruff-root> scruff check
```

To enable formatting on-compile:

```scala
ThisBuild / scruffFmtOnCompile := true
```

To check the lint rules on-compile, after formatting, fixing what they can (as `scruff check --fix`) and failing the compile if a rule flags anything left:

```scala
ThisBuild / scruffCheckOnCompile := true
```
