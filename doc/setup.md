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

For convenience, We've also created an sbt plugin that adds `scruffFmt`, `scruffFmtCheck`, and `scruffFmtOnCompile`.

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
