use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{Import, ParseResult, RefKind, Reference, Symbol, SymbolKind};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

const SYMBOL_QUERY: &str = r#"
(function_item
    name: (identifier) @fn_name
    parameters: (parameters) @fn_params
) @fn_node

(struct_item
    name: (type_identifier) @struct_name
) @struct_node

(enum_item
    name: (type_identifier) @enum_name
) @enum_node

(trait_item
    name: (type_identifier) @trait_name
) @trait_node

(impl_item
    trait: (type_identifier)? @impl_trait
    type: (type_identifier) @impl_type
) @impl_node
"#;

const CALL_QUERY: &str = r#"
(call_expression
    function: (identifier) @call_name
) @call_node

(call_expression
    function: (field_expression
        field: (field_identifier) @method_name
    )
) @method_call_node

(call_expression
    function: (scoped_identifier
        name: (identifier) @scoped_name
        path: (identifier)? @scoped_path
    )
) @scoped_call_node

(macro_invocation
    macro: (identifier) @macro_name
) @macro_node
"#;

const USE_QUERY: &str = r#"
(use_declaration
    argument: (_) @use_path
) @use_node
"#;

pub fn parse(source: &str) -> Result<ParseResult> {
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).context("Failed to set Rust language")?;

    let tree = parser.parse(source, None).context("Failed to parse Rust source")?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();

    parse_symbols(root, src, &lang, &mut symbols)?;
    parse_calls(root, src, &lang, &symbols, &mut references)?;
    parse_uses(root, src, &lang, &mut imports)?;
    parse_impl_blocks(root, src, &lang, &symbols, &mut references)?;

    Ok(ParseResult { symbols, references, imports })
}

fn capture_node_by_idx<'a>(
    m: &tree_sitter::QueryMatch<'_, 'a>,
    idx: u32,
) -> Option<tree_sitter::Node<'a>> {
    m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
}

fn capture_text_by_idx<'a>(
    m: &tree_sitter::QueryMatch<'_, 'a>,
    idx: u32,
    src: &'a [u8],
) -> Option<&'a str> {
    capture_node_by_idx(m, idx).map(|n| node_text(n, src))
}

fn make_call_ref(name: &str, qualifier: Option<String>, line: usize, source: Option<String>) -> Reference {
    Reference {
        kind: RefKind::Call,
        target_name: name.to_string(),
        target_qualifier: qualifier,
        line,
        source_symbol_name: source,
    }
}

fn parse_symbols(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid symbol query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let fn_name_idx = q.capture_index_for_name("fn_name").unwrap();
        let fn_params_idx = q.capture_index_for_name("fn_params").unwrap();
        let fn_node_idx = q.capture_index_for_name("fn_node").unwrap();
        let struct_name_idx = q.capture_index_for_name("struct_name").unwrap();
        let struct_node_idx = q.capture_index_for_name("struct_node").unwrap();
        let enum_name_idx = q.capture_index_for_name("enum_name").unwrap();
        let enum_node_idx = q.capture_index_for_name("enum_node").unwrap();
        let trait_name_idx = q.capture_index_for_name("trait_name").unwrap();
        let trait_node_idx = q.capture_index_for_name("trait_node").unwrap();

        for cap in m.captures {
            if cap.index == fn_name_idx {
                symbols.push(build_fn_symbol(m, cap.node, src, fn_node_idx, fn_params_idx));
            } else if cap.index == struct_name_idx {
                symbols.push(build_type_sym(m, cap.node, src, struct_node_idx, SymbolKind::Struct));
            } else if cap.index == enum_name_idx {
                symbols.push(build_type_sym(m, cap.node, src, enum_node_idx, SymbolKind::Enum));
            } else if cap.index == trait_name_idx {
                symbols.push(build_type_sym(m, cap.node, src, trait_node_idx, SymbolKind::Trait));
            }
        }
    });
    Ok(())
}

fn build_fn_symbol(
    m: &tree_sitter::QueryMatch,
    name_node: tree_sitter::Node,
    src: &[u8],
    fn_node_idx: u32,
    fn_params_idx: u32,
) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let fn_node = capture_node_by_idx(m, fn_node_idx);
    let params = capture_text_by_idx(m, fn_params_idx, src).unwrap_or("");
    let (line_start, line_end) = match fn_node {
        Some(n) => (n.start_position().row, n.end_position().row),
        None => (name_node.start_position().row, name_node.end_position().row),
    };
    let is_method = is_inside_impl(name_node);
    let parent_name = find_parent_impl_type(name_node, src);
    let vis = extract_visibility(fn_node.unwrap_or(name_node), src);

    Symbol {
        name,
        kind: if is_method { SymbolKind::Method } else { SymbolKind::Function },
        line_start,
        line_end,
        parent_name,
        visibility: vis,
        signature: Some(format!("fn {}", params)),
    }
}

fn build_type_sym(
    m: &tree_sitter::QueryMatch,
    name_node: tree_sitter::Node,
    src: &[u8],
    node_idx: u32,
    kind: SymbolKind,
) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let container = capture_node_by_idx(m, node_idx).unwrap_or(name_node);
    Symbol {
        name,
        kind,
        line_start: container.start_position().row,
        line_end: container.end_position().row,
        parent_name: None,
        visibility: extract_visibility(container, src),
        signature: None,
    }
}

fn parse_calls(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &[Symbol],
    references: &mut Vec<Reference>,
) -> Result<()> {
    let query = Query::new(lang, CALL_QUERY).context("Invalid call query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let call_name_idx = q.capture_index_for_name("call_name").unwrap();
        let method_name_idx = q.capture_index_for_name("method_name").unwrap();
        let scoped_name_idx = q.capture_index_for_name("scoped_name").unwrap();
        let scoped_path_idx = q.capture_index_for_name("scoped_path").unwrap();
        let macro_name_idx = q.capture_index_for_name("macro_name").unwrap();

        for cap in m.captures {
            let line = cap.node.start_position().row;
            let source_sym = find_enclosing_symbol(symbols, line);

            if cap.index == call_name_idx || cap.index == method_name_idx {
                references.push(make_call_ref(node_text(cap.node, src), None, line, source_sym));
            } else if cap.index == scoped_name_idx {
                let qualifier = capture_text_by_idx(m, scoped_path_idx, src).map(|s| s.to_string());
                references.push(make_call_ref(node_text(cap.node, src), qualifier, line, source_sym));
            } else if cap.index == macro_name_idx {
                let name = format!("{}!", node_text(cap.node, src));
                references.push(make_call_ref(&name, None, line, source_sym));
            }
        }
    });
    Ok(())
}

fn parse_uses(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    imports: &mut Vec<Import>,
) -> Result<()> {
    let query = Query::new(lang, USE_QUERY).context("Invalid use query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let use_path_idx = q.capture_index_for_name("use_path").unwrap();
        for cap in m.captures {
            if cap.index == use_path_idx {
                let full_path = node_text(cap.node, src).to_string();
                let local_name = extract_rust_import_name(&full_path);
                imports.push(Import {
                    local_name,
                    full_path,
                    alias: None,
                    line: cap.node.start_position().row,
                });
            }
        }
    });
    Ok(())
}

fn parse_impl_blocks(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    existing_symbols: &[Symbol],
    references: &mut Vec<Reference>,
) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid symbol query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let impl_node_idx = q.capture_index_for_name("impl_node").unwrap();
        let impl_type_idx = q.capture_index_for_name("impl_type").unwrap();
        let impl_trait_idx = q.capture_index_for_name("impl_trait");

        if !m.captures.iter().any(|c| c.index == impl_node_idx) {
            return;
        }
        let type_name: Option<&str> = capture_text_by_idx(m, impl_type_idx, src);
        let trait_name: Option<&str> = impl_trait_idx.and_then(|idx| capture_text_by_idx(m, idx, src));

        if let (Some(tn), Some(tr)) = (type_name, trait_name) {
            let line = capture_node_by_idx(m, impl_node_idx).unwrap().start_position().row;
            references.push(Reference {
                kind: RefKind::TraitImpl,
                target_name: tr.to_string(),
                target_qualifier: Some(tn.to_string()),
                line,
                source_symbol_name: find_enclosing_symbol(existing_symbols, line),
            });
        }
    });
    Ok(())
}

fn is_inside_impl(node: tree_sitter::Node) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            return true;
        }
        current = parent.parent();
    }
    false
}

fn find_parent_impl_type<'a>(node: tree_sitter::Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i) {
                    if child.kind() == "type_identifier" {
                        return Some(node_text(child, src).to_string());
                    }
                }
            }
        }
        current = parent.parent();
    }
    None
}

fn extract_visibility(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "visibility_modifier" {
                return Some(node_text(child, src).to_string());
            }
        }
    }
    None
}

fn extract_rust_import_name(path: &str) -> String {
    if let Some(alias_pos) = path.find(" as ") {
        return path[alias_pos + 4..].trim().to_string();
    }
    path.rsplit("::").next().unwrap_or(path).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_function() {
        let src = "pub fn hello(x: i32) -> String {\n    format!(\"hello {}\", x)\n}\n";
        let result = parse(src).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "hello");
        assert_eq!(result.symbols[0].kind, SymbolKind::Function);
        assert_eq!(result.symbols[0].visibility.as_deref(), Some("pub"));
    }

    #[test]
    fn test_parse_struct_and_impl() {
        let src = "pub struct Foo { value: i32 }\nimpl Foo {\n    pub fn new(v: i32) -> Self { Self { value: v } }\n}\n";
        let result = parse(src).unwrap();
        let struct_sym = result.symbols.iter().find(|s| s.name == "Foo").unwrap();
        assert_eq!(struct_sym.kind, SymbolKind::Struct);
        let new_sym = result.symbols.iter().find(|s| s.name == "new").unwrap();
        assert_eq!(new_sym.kind, SymbolKind::Method);
        assert_eq!(new_sym.parent_name.as_deref(), Some("Foo"));
    }

    #[test]
    fn test_parse_use_statements() {
        let src = "use std::collections::HashMap;\nuse crate::model::Symbol;\nuse anyhow::Result;\n";
        let result = parse(src).unwrap();
        assert_eq!(result.imports.len(), 3);
        assert_eq!(result.imports[0].local_name, "HashMap");
        assert_eq!(result.imports[1].local_name, "Symbol");
        assert_eq!(result.imports[2].local_name, "Result");
    }

    #[test]
    fn test_parse_calls() {
        let src = "fn main() {\n    let x = foo();\n    bar.baz();\n    println!(\"hello\");\n}\n";
        let result = parse(src).unwrap();
        let call_names: Vec<&str> = result.references.iter().map(|r| r.target_name.as_str()).collect();
        assert!(call_names.contains(&"foo"));
        assert!(call_names.contains(&"baz"));
        assert!(call_names.contains(&"println!"));
    }
}
