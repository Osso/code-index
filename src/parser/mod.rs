pub mod php;
pub mod python;
pub mod rust_lang;
pub mod typescript;

use crate::model::{Language, ParseResult, Symbol};
use anyhow::Result;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor};

/// Parse a source file and extract symbols, references, and imports.
pub fn parse_file(source: &str, language: Language) -> Result<ParseResult> {
    match language {
        Language::Php => php::parse(source),
        Language::Rust => rust_lang::parse(source),
        Language::Python => python::parse(source),
        Language::TypeScript => typescript::parse(source),
    }
}

/// Extract text from a tree-sitter node.
pub fn node_text<'a>(node: Node<'a>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

/// Run a tree-sitter query and call a closure for each match.
/// Handles the StreamingIterator from tree-sitter 0.25.
pub fn for_each_match<F>(query: &Query, node: Node, source: &[u8], mut f: F)
where
    F: FnMut(&tree_sitter::QueryMatch, &Query, &[u8]),
{
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, node, source);
    while let Some(m) = matches.next() {
        f(m, query, source);
    }
}

/// Find the enclosing symbol name for a given line position.
/// Prefers the most specific (smallest range) enclosing symbol.
pub fn find_enclosing_symbol(symbols: &[Symbol], line: usize) -> Option<String> {
    symbols
        .iter()
        .filter(|s| s.line_start <= line && s.line_end >= line)
        .min_by_key(|s| s.line_end - s.line_start)
        .map(|s| s.name.clone())
}
