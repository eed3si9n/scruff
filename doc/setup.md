# Setup


The native binaries are published to GitHub Releases <https://github.com/eed3si9n/scruff/releases/tag/v0.1.0>.

## Setting up GitHub Actions

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

## sbt-scruff

For convenience, We've also created an sbt plugin that adds `scruffFmt`, `scruffFmtCheck`, `scruff`, `scruffFmtOnCompile`, and `scruffCheckOnCompile`.

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

`scruff` runs the scruff CLI in the build's root with the arguments you give it, and fails if scruff exits with an error. For example, to check the build against the lint rules (scruff 0.2.0 or later):

```bash
sbt:scruff-root> scruff check
```

To enable formatting on-compile:

```scala
ThisBuild / scruffFmtOnCompile := true
```

To check the lint rules on-compile, after formatting, failing the compile if a rule flags anything:

```scala
ThisBuild / scruffCheckOnCompile := true
```
