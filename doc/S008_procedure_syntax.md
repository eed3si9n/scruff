# S008 procedure_syntax

On by default. Fixable with `--fix`.

```bash
$ scruff check --select procedure_syntax --fix

# or, alternatively
$ scruff check --select S008 --fix
```

## Description

Flags Scala 2.x procedure syntax: `def f() { ... }` without `=`, and `def f()` without a result type.

## Why is this bad?

The Scala 3 reference lists it under dropped features, in [Dropped: Procedure Syntax](https://docs.scala-lang.org/scala3/reference/dropped-features/procedure-syntax.html): "Procedure syntax `def f() { ... }` has been dropped." Scala 3 accepts it only under `-source:3.0-migration`.

## Fix

`--fix` writes the result type, `: Unit`, after the last parameter clause, with `=` before the body: `def run() { ... }` becomes `def run(): Unit = { ... }`, and `def stop()` becomes `def stop(): Unit`.

## Example

```scala
object Main {
  def run() { println("running") }
  def stop()
}
```

`scruff check` reports both methods:

```
S008 procedure_syntax procedure syntax is dropped in Scala 3; write the result type, `: Unit =`
 --> Main.scala:2:7
  |
1 | object Main {
2 |   def run() { println("running") }
  |       ^^^
3 |   def stop()
4 | }
  |
```

Use instead:

```scala
object Main {
  def run(): Unit = { println("running") }
  def stop(): Unit
}
```
