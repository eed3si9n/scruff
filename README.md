# Scruff

Scruff is an experimental, lightweight linter and formatter for Scala, built on top of Tree-Sitter Scala, and written in Rust. See [the Scruff documentation](https://eed3si9n.com/scruff/) for the full info.

## Scruff formatter

Scruff formatter is a fast and normalizing Scala code formatter. It's about 6x faster than Scalafmt out of the box.

## Scruff linter

Scruff linter is a fast and syntactic Scala linter. It flags things like `null`, `return`, and unsorted imports, and `--fix` fixes what the rules can.

## License

Scruff is provided under MIT License.
