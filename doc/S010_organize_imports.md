# S010 organize_imports

Off by default. Fixable with `--fix`.

```bash
$ scruff check --select organize_imports --fix

# or, alternatively
$ scruff check --select S010 --fix
```

`--select` runs this rule, and only it, without changing the configuration. To keep it on, add it to `extend-select` under `[lint]`.

```admonish warning title="Unsafe fix"
`--fix` can change what the code means, or stop it from compiling. Scruff reads only syntax, so it can't tell which names a wildcard import such as `import scala.concurrent.*` brings in, and may move an import that uses one, such as `import duration.*`, above it. See [Example: opting out](#example-opting-out) for how to keep such an import below.
```

## Description

Flags imports that aren't sorted. Imports up to the next statement sort together, blank lines or not.

Capitals sort after lowercase, and wildcards first: `cats.*`, `cats.data.Ior`, `cats.Eq`. An import of an imported name, as `a.foo` after `import c.b.a`, stays after it.

## Fix

`--fix` sorts the lines and drops blank ones between imports; comments move with the import they're on or above.

## Example

Given this `Main.scala`:

```scala
package example

import scala.util.Try
import scala.concurrent.duration.*
import cats.syntax.all.*
import cats.Eq

import example.internal.Config
import Config.{given, *}
import example.internal.Codec

object Main
```

`scruff check --select organize_imports` reports the first import out of place:

```
S010 organize_imports imports are not sorted alphabetically
 --> Main.scala:3:1
  |
1 | package example
2 |
3 | import scala.util.Try
  | ^^^^^^^^^^^^^^^^^^^^^
4 | import scala.concurrent.duration.*
5 | import cats.syntax.all.*
  |

Found 1 problem.
[*] 1 fixable with the `--fix` option.
```

`scruff check --select organize_imports --fix` sorts the imports as one block, keeping `Config.{given, *}` after the import of `Config`:

```scala
package example

import cats.syntax.all.*
import cats.Eq
import example.internal.Codec
import example.internal.Config
import scala.concurrent.duration.*
import scala.util.Try
import Config.{given, *}

object Main
```

and prints `Found 1 problem (1 fixed, 0 remaining).`, exiting with 0.

## Example: opting out

Names a wildcard brings in aren't known, so an import using one sorts as any other. Here `duration` comes from `scala.concurrent.*`, but `--fix` would move `import duration.*` above it, which doesn't compile:

```scala
package example

import scala.concurrent.*
import duration.*
import cats.Eq

object Main:
  val timeout = 5.seconds
```

`// scruff: ignore(organize_imports)` at the end of the import's line, or on the line above, puts that import after the rest, since what it uses may come from any import above it. `--fix` then sorts the others:

```scala
package example

import cats.Eq
import scala.concurrent.*
import duration.* // scruff: ignore(organize_imports)

object Main:
  val timeout = 5.seconds
```

To keep a file's imports as written, turn the rule off for the file with `// scruff: file_ignore(organize_imports)`. Writing the import in full, `import scala.concurrent.duration.*`, works too. See [Error suppression](scruff-check.md#error-suppression).
