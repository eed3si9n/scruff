# S009 sbt_0_13_syntax

Off by default. Fixable with `--fix`.

```bash
$ scruff check --select sbt_0_13_syntax --fix

# or, alternatively
$ scruff check --select S009 --fix
```

`--select` runs this rule, and only it, without changing the configuration. To keep it on, add it to `extend-select` under `[lint]`.

## Description

Flags sbt 0.13's scoping with `in`, as in `scalacOptions in Compile` or `key.in(Compile)`. It points at `in`. Any `.sbt`, `.scala` or `.sc` file scruff finds is checked, `project/*.scala` included.

## Why is this bad?

sbt 1.5 deprecated `in` for the slash syntax, as [Migrating from sbt 0.13.x](https://www.scala-sbt.org/1.x/docs/Migrating-from-sbt-013x.html) describes, and sbt 2 removes it, as [Migrating from sbt 1.x](https://www.scala-sbt.org/2.x/docs/en/changes/migrating-from-sbt-1.x.html) describes.

## Fix

`--fix` writes the slash syntax, with a tuple's parts in its order: `key in (Compile, console)` becomes `Compile / console / key`.

Where one `in` follows another, as in `sources in Compile in core`, syntax alone can't tell the project from the configuration. `--fix` rewrites the first `in` and reports the second as remaining, exiting with 1:

```scala
Compile / sources in core := Nil
```

Write `core / Compile / sources` there yourself.

## Example: migrating from sbt 0.13 syntax

Given this `build.sbt`:

```scala
lazy val core = project
  .settings(
    scalacOptions in Compile += "-deprecation",
    scalacOptions in (Compile, console) := Nil,
    fork in Test := true,
  )
```

`scruff check --select sbt_0_13_syntax --fix` rewrites it to the slash syntax that sbt 2 requires:

```scala
lazy val core = project
  .settings(
    Compile / scalacOptions += "-deprecation",
    Compile / console / scalacOptions := Nil,
    Test / fork := true,
  )
```

and prints `Found 3 problems (3 fixed, 0 remaining).`, exiting with 0.
