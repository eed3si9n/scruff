# The Scruff style

Scruff aims for consistency, generality, readability, and reducing git diffs.

This document describes the current formatting style.

## How Scruff wraps lines

Scruff ignores previous formatting, and applies uniform horizontal whitespace to your code based on max-column (default `100`). Scruff uses `2` whitespaces for indentation, like most Scala styles.

### Short expressions

Scruff tries to print one full expression or a statement per line. If this fits the allotted max-column, it will flatten the lines together.

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

becomes:

```scala
// scruffed
object A:
  val a = List(1, 2)
  val b = List(1, 2)
  val c = List(1, 2)
```

### Expressions that can fold in half

When a definition doesn't fit on one line, Scruff first tries folding it in half at the `=`:

```scala
def compileIncremental(setup: Setup, previous: PreviousResult): CompileResult = compileWith(setup, previous, inputs, reporter)
```

becomes

```scala
// scruffed
def compileIncremental(setup: Setup, previous: PreviousResult): CompileResult =
  compileWith(setup, previous, inputs, reporter)
```

### Long expressions

If a function call still doesn't fit after folding, Scruff puts one argument per line, with a trailing comma:

```scala
val result = doSomething(compileInputs, logger, reporter, progress, cacheDirectory, extraOptions, previousResult)
```

becomes

```scala
// scruffed
val result = doSomething(
  compileInputs,
  logger,
  reporter,
  progress,
  cacheDirectory,
  extraOptions,
  previousResult,
)
```

```admonish note
Scruff will add trailing commas to long function calls.
```

### Named arguments

A function call with named arguments, or with a DSL containing `=` signs, puts one argument per line:

```scala
val conn = connect(host = "localhost", port = 8080)

lazy val action = project
  .settings(name := "action", Test / fork := true)
```

becomes

```scala
// scruffed
val conn = connect(
  host = "localhost",
  port = 8080,
)

lazy val action = project
  .settings(
    name := "action",
    Test / fork := true,
  )
```

### Call chain (fluent interface)

A chain of two or more method calls and field selections stays on one line if it fits. Otherwise, Scruff breaks before each `.`, starting from the first call. Field selection, like `xs.dependencies`, stay with the receiver:

```scala
val names = xs.dependencies.filter(_.isTransitive).map(_.moduleName).filterNot(excluded).distinct.sorted.take(limit)
```

becomes

```scala
// scruffed
val names = xs.dependencies
  .filter(_.isTransitive)
  .map(_.moduleName)
  .filterNot(excluded)
  .distinct
  .sorted
  .take(limit)
```

### Chained infix expression

A chain of infix expressions stays on one line if it fits. Otherwise, Scruff breaks before each `&&` or `||` operator, so the operators start the lines:

```scala
val skip = isCi && isPullRequest || offline && cache.isEmpty || dryRun && warningCount + errorCount == 0
```

becomes

```scala
// scruffed
val skip = isCi && isPullRequest
  || offline && cache.isEmpty
  || dryRun && warningCount + errorCount == 0
```

## Blank lines

Scruff adds 2 blank lines above a top-level class, trait, object, or enum, except 1 blank line between a class and its companion object, and 1 line above a nested one.

Scruff adds 1 blank line above and below a `val`, `var`, or `def` that takes more than one line

```scala
package app
import scala.util.Try
class Box(val value: Int):
  val doubled = value * 2
  def show = s"Box($value)"
  def merge(other: Box) =
    val sum = value + other.value
    Box(sum)
  val zero = 0
object Box:
  def empty = Box(0)
```

becomes

```scala
// scruffed
package app

import scala.util.Try


class Box(val value: Int):
  val doubled = value * 2
  def show = s"Box($value)"

  def merge(other: Box) =
    val sum = value + other.value
    Box(sum)

  val zero = 0

object Box:
  def empty = Box(0)
```

## Scala 3 dialect

Scala 3 is the default dialect of Scruff formatter:

```toml
dialect = "scala3"
```

### Scala 3: Indentation syntax

`"scala3"` diealect replaces braces with indentation where Scala 3 allows it. This covers the bodies of classes, objects, and definitions:

```scala
object A {
  def foo(x: Int): String =
    x match {
      case 1 => "one"
      case _ => "many"
    }
}
```

becomes

```scala
// scruffed
object A:
  def foo(x: Int): String = x match
    case 1 => "one"
    case _ => "many"
```

### Scala 3: Fewer braces (colon argument) syntax

`"scala3"` dialect replaces a braced lambda argument with a colon argument (SIP-44):

```scala
val sizes = files.map { file =>
  val bytes = Files.size(file)
  bytes / 1024
}
```

becomes

```scala
// scruffed
val sizes = files.map: file =>
  val bytes = Files.size(file)
  bytes / 1024
```

### Scala 3: Control syntax

`"scala3"` dialect rewrites to Scala 3 control syntax:

```scala
val label = if (count > 1) "many" else "one"
val names = for (file <- files) yield file.getName
```

becomes

```scala
// scruffed
val label = if count > 1 then "many" else "one"
val names = for file <- files yield file.getName
```

### Scala 3: Miscellaneous rewrites

- `*` for wildcard imports, and `as` for renames in imports and exports
- `?` for type wildcards
- `xs*` for vararg splices
- `using` for `implicit` parameter clauses
- `&` for compound types, and commas between parents after `extends`
- End markers added to definitions spanning 20 or more lines, and removed from shorter ones

## Scala 2 dialects

Scruff also supports the following Scala 2 dialects:

- `scala213source3`, `scala212source3`, and `sbt1` (Scala 2 with `-Xsource:3`): Scruff rewrites imports, type wildcards, and vararg splices to the Scala 3 syntax, and keeps braces, parentheses, `implicit`, and `with`.
- `scala213`: Scruff rewrites Scala 3 syntax back to Scala 2 syntax, such as braces, parentheses, `_` wildcard imports, `=>` renames, and `xs: _*` splices.

In both, a Boolean chain broken across lines is wrapped in parentheses, since a Scala 2 line can't start with an operator:

```scala
// scruffed
val shouldPublish = (isRelease
  && !isSnapshot
  && credentials.nonEmpty
  && repository.isDefined
  && testsPassed)
```

## Comments

Scruff does not format comment contents.

### Scaladoc

Scruff will, however, unify the Scaladoc comments with _gutter asterisks_ aligned in column two:

```scala
// scruffed
/** Compiles the sources.
 *
 *  @param sources the files to compile
 */
def compile(sources: Seq[File]): Unit = ()
```

### format: off

Code in between `// format: off` and `// format: on` are not formatted:

```scala
// format: off

object AnythingGoes {}

// format: on
```
