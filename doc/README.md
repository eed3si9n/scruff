# Scruff

Scruff is an experimental, lightweight linter and formatter for Scala, built on top of Tree-Sitter Scala, and written in Rust.

## Scruff formatter

Scruff formatter is a fast and normalizing Scala code formatter.
Inspired by Python's code formatters like [Black](https://pypi.org/project/black/) and [Ruff](https://docs.astral.sh/ruff/formatter/),
Scruff removes all configurations related to styling. See [scruff fmt](scruff-fmt.md) for more details.

## Scruff linter

Scruff linter is an experimental syntactic Scala linter, that can process 400k sloc/s. See [scruff check](scruff-check.md) for more details.

## License

Scruff is provided under MIT License.
