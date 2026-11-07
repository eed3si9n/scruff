//! Imports for the rules' fixes: whether a name is imported where code
//! is, and the edit adding an import of it to the file.

use super::organize_imports::{Direction, in_group, sorts_before};
use super::{Edit, line_end, line_start, name, newline};
use crate::lossless_tree::{SyntaxElement, SyntaxKind, SyntaxNode, TextSize};

/// What brings a path's last name in where code is.
#[derive(PartialEq)]
enum InScope {
    /// Nothing.
    Nothing,
    /// The path.
    Path,
    /// Something else under the same name.
    Other,
}

/// For `path`, as `["scala", "util", "Using"]`, the edit adding
/// `import scala.util.Using` to the file holding `node`, `Some(None)` if
/// an import in scope brings it in already, or `None` if one brings in
/// something else named `Using`.
pub(super) fn import(node: &SyntaxNode<'_>, path: &[&str]) -> Option<Option<Edit>> {
    let scopes: Vec<SyntaxNode<'_>> = node.ancestors().collect();
    let found = scopes
        .iter()
        .flat_map(|scope| scope.children().collect::<Vec<_>>())
        .filter(|child| child.kind() == SyntaxKind::ImportDeclaration)
        .map(|import| brings_in(&import, path))
        .fold(InScope::Nothing, |a, b| match (a, b) {
            (InScope::Other, _) | (_, InScope::Other) => InScope::Other,
            (InScope::Path, _) | (_, InScope::Path) => InScope::Path,
            _ => InScope::Nothing,
        });
    match found {
        InScope::Other => None,
        InScope::Path => Some(None),
        InScope::Nothing => Some(add_import(scopes.last()?, path)),
    }
}

/// What `import` brings in under `path`'s last name.
fn brings_in(import: &SyntaxNode<'_>, path: &[&str]) -> InScope {
    let Some((wanted, package)) = path.split_last() else {
        return InScope::Nothing;
    };
    let parts: Vec<SyntaxElement<'_>> = import
        .non_trivia_children()
        .filter(|c| c.kind() != SyntaxKind::ImportKw)
        .collect();
    let mut found = InScope::Nothing;
    for imported in parts.split(|p| p.kind() == SyntaxKind::Comma) {
        let Some((last, prefix)) = imported.split_last() else {
            continue;
        };
        let prefix: Vec<String> = prefix
            .iter()
            .filter(|p| p.kind() == SyntaxKind::Identifier)
            .map(|p| name(p.text()))
            .collect();
        let from_package = prefix == package;
        let selectors: Vec<SyntaxElement<'_>> = match last.kind() {
            SyntaxKind::NamespaceSelectors => last
                .as_node()
                .map(|n| n.non_trivia_children().collect())
                .unwrap_or_default(),
            _ => vec![*last],
        };
        for selector in selectors {
            let here = match selector.kind() {
                SyntaxKind::Identifier if name(selector.text()) == *wanted => {
                    if from_package {
                        InScope::Path
                    } else {
                        InScope::Other
                    }
                }
                SyntaxKind::NamespaceWildcard if from_package => InScope::Path,
                SyntaxKind::AsRenamedIdentifier | SyntaxKind::ArrowRenamedIdentifier => {
                    let alias = selector
                        .as_node()
                        .and_then(|n| n.child_by_field_name("alias"));
                    if alias.is_some_and(|a| name(a.text()) == *wanted) {
                        InScope::Other
                    } else {
                        InScope::Nothing
                    }
                }
                _ => InScope::Nothing,
            };
            if here == InScope::Other || found == InScope::Nothing {
                found = here;
            }
        }
    }
    found
}

/// An import of `path` put in the file's first group of imports, in
/// sorted order, or after its `package` clauses, or at its start.
fn add_import(root: &SyntaxNode<'_>, path: &[&str]) -> Option<Edit> {
    let source = root.text();
    let newline = newline(source);
    let text = format!("import {}", path.join("."));
    let offset = |o: usize| TextSize::try_from(o).ok();
    let top_level = |n: &SyntaxNode<'_>| {
        n.ancestors().skip(1).all(|a| {
            a.kind() == SyntaxKind::CompilationUnit
                || a.kind() == SyntaxKind::PackageClause
                || a.parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::PackageClause)
        })
    };
    let first = root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ImportDeclaration && top_level(n));
    if let Some(first) = first {
        let group: Vec<SyntaxNode<'_>> =
            std::iter::successors(Some(first), |n| in_group(n, Direction::Forward)).collect();
        if let Some(next) = group.iter().find(|i| sorts_before(path, i)) {
            // Before it and the comment lines above it, which go with it.
            let start = usize::from(next.text_range().start());
            let mut at = line_start(source, start);
            if !source[at..start].trim().is_empty() {
                return Some(Edit::insert(offset(start)?, &format!("{text}; ")));
            }
            let indent = &source[at..start];
            while at > 0 {
                let above = line_start(source, at - 1);
                let line = source[above..at].trim();
                if !(line.starts_with("//") || (line.starts_with("/*") && line.ends_with("*/"))) {
                    break;
                }
                at = above;
            }
            return Some(Edit::insert(
                offset(at)?,
                &format!("{indent}{text}{newline}"),
            ));
        }
        let last = group.last()?;
        let end = usize::from(last.text_range().end());
        let start = line_start(source, usize::from(last.text_range().start()));
        let indent: String = source[start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        return Some(Edit::insert(
            offset(line_end(source, end))?,
            &format!("{newline}{indent}{text}"),
        ));
    }
    let package = root
        .children()
        .filter(|c| {
            c.kind() == SyntaxKind::PackageClause && c.child_by_field_name("body").is_none()
        })
        .last();
    if let Some(package) = package {
        let end = line_end(source, usize::from(package.text_range().end()));
        return Some(Edit::insert(
            offset(end)?,
            &format!("{newline}{newline}{text}"),
        ));
    }
    let start = root.non_trivia_children().next()?.text_range().start();
    Some(Edit::insert(start, &format!("{text}{newline}{newline}")))
}
