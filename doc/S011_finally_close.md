# S011 finally_close

Off by default. Fixable with `--fix`.

```bash
$ scruff check --select finally_close --fix

# or, alternatively
$ scruff check --select S011 --fix
```

`--select` runs this rule, and only it, without changing the configuration. To keep it on, add it to `extend-select` under `[lint]`.

```admonish note
Credit to Kenji Yoshida (xuwei-k)'s [ScalaUtilUsingResource rule](https://github.com/xuwei-k/scalafix-rules/blob/main/rules/src/main/scala/fix/ScalaUtilUsingResource.scala).
```

## Description

Flags a `try` without `catch` whose `finally` only closes a resource, as in `try { ... } finally a.close()`. It points at `try`.

## Why is this bad?

[scala.util.Using](https://www.scala-lang.org/api/current/scala/util/Using$.html)'s `resource` function can express this.

## Fix

`--fix` rewrites `val a = ...` and the `try` right after it to `Using.resource(...): a =>`, with braces outside Scala 3, and imports `scala.util.Using`.

## Example

Given this `A.scala`, in the `scala3` dialect:

```scala
package example

import java.io.FileInputStream

object A:
  def f1: Int =
    val a = new FileInputStream("a")
    try
      val b = a.read()
      val c = a.read()
      b + c
    finally a.close()
```

`scruff check --select finally_close --fix` rewrites it to:

```scala
package example

import java.io.FileInputStream
import scala.util.Using

object A:
  def f1: Int =
    Using.resource(new FileInputStream("a")): a =>
      val b = a.read()
      val c = a.read()
      b + c
```

and prints `Found 1 problem (1 fixed, 0 remaining).`, exiting with 0.
