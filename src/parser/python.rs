use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{Import, ParseResult, RefKind, Reference, Symbol, SymbolKind};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

const SYMBOL_QUERY: &str = r#"
(function_definition
    name: (identifier) @fn_name
    parameters: (parameters) @fn_params
) @fn_node

(class_definition
    name: (identifier) @class_name
    superclasses: (argument_list)? @superclasses
) @class_node
"#;

const CALL_QUERY: &str = r#"
(call
    function: (identifier) @call_name
) @call_node

(call
    function: (attribute
        attribute: (identifier) @method_call_name
    )
) @method_call_node
"#;

const IMPORT_QUERY: &str = r#"
(import_statement
    name: (dotted_name) @import_name
) @import_node

(import_from_statement
    module_name: (dotted_name)? @module_name
    name: (dotted_name) @from_name
) @from_node

(import_from_statement
    module_name: (dotted_name)? @module_name2
    name: (aliased_import
        name: (dotted_name) @aliased_name
        alias: (identifier) @alias
    )
) @aliased_node
"#;

pub fn parse(source: &str) -> Result<ParseResult> {
    let lang: tree_sitter::Language = tree_sitter_python::LANGUAGE.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).context("Failed to set Python language")?;

    let tree = parser.parse(source, None).context("Failed to parse Python source")?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();

    parse_symbols(root, src, &lang, &mut symbols, &mut references)?;
    parse_calls(root, src, &lang, &symbols, &mut references)?;
    parse_imports(root, src, &lang, &mut imports)?;

    Ok(ParseResult { symbols, references, imports })
}

fn cap_node<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32) -> Option<tree_sitter::Node<'a>> {
    m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
}

fn cap_text<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32, src: &'a [u8]) -> Option<&'a str> {
    cap_node(m, idx).map(|n| node_text(n, src))
}

fn python_visibility(name: &str) -> Option<String> {
    if name.starts_with("__") && !name.ends_with("__") {
        Some("private".to_string())
    } else if name.starts_with('_') {
        Some("protected".to_string())
    } else {
        None
    }
}

fn build_fn_sym(m: &tree_sitter::QueryMatch, name_node: tree_sitter::Node, src: &[u8], fn_node_idx: u32, fn_params_idx: u32) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let fn_node = cap_node(m, fn_node_idx).unwrap_or(name_node);
    let params = cap_text(m, fn_params_idx, src).unwrap_or("");
    let parent_name = find_parent_class_py(name_node, src);
    let is_method = parent_name.is_some();
    let is_test = name.starts_with("test_") || name.starts_with("test");
    Symbol {
        name: name.clone(), kind: if is_method { SymbolKind::Method } else { SymbolKind::Function },
        line_start: fn_node.start_position().row, line_end: fn_node.end_position().row,
        parent_name, visibility: python_visibility(&name),
        signature: Some(format!("def {}", params)),
        is_test,
    }
}

fn extract_superclasses(node: tree_sitter::Node, src: &[u8], class_name: &str, references: &mut Vec<Reference>) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "identifier" => {
                    references.push(Reference {
                        kind: RefKind::Inherit, target_name: node_text(child, src).to_string(),
                        target_qualifier: None, line: child.start_position().row,
                        source_symbol_name: Some(class_name.to_string()),
                    });
                }
                "attribute" => {
                    let full = node_text(child, src);
                    let name = full.rsplit('.').next().unwrap_or(full);
                    references.push(Reference {
                        kind: RefKind::Inherit, target_name: name.to_string(),
                        target_qualifier: Some(full.to_string()), line: child.start_position().row,
                        source_symbol_name: Some(class_name.to_string()),
                    });
                }
                _ => {}
            }
        }
    }
}

fn parse_symbols(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, symbols: &mut Vec<Symbol>, references: &mut Vec<Reference>) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid Python symbol query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let fn_name_idx = q.capture_index_for_name("fn_name").unwrap();
        let fn_params_idx = q.capture_index_for_name("fn_params").unwrap();
        let fn_node_idx = q.capture_index_for_name("fn_node").unwrap();
        let class_name_idx = q.capture_index_for_name("class_name").unwrap();
        let class_node_idx = q.capture_index_for_name("class_node").unwrap();
        let sc_idx = q.capture_index_for_name("superclasses");

        for cap in m.captures {
            if cap.index == fn_name_idx {
                symbols.push(build_fn_sym(m, cap.node, src, fn_node_idx, fn_params_idx));
            } else if cap.index == class_name_idx {
                let name = node_text(cap.node, src).to_string();
                let class_node = cap_node(m, class_node_idx).unwrap_or(cap.node);
                if let Some(si) = sc_idx {
                    if let Some(sc_node) = cap_node(m, si) {
                        extract_superclasses(sc_node, src, &name, references);
                    }
                }
                symbols.push(Symbol {
                    name, kind: SymbolKind::Class,
                    line_start: class_node.start_position().row, line_end: class_node.end_position().row,
                    parent_name: None, visibility: None, signature: None,
                    is_test: false,
                });
            }
        }
    });
    Ok(())
}

fn parse_calls(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, symbols: &[Symbol], references: &mut Vec<Reference>) -> Result<()> {
    let query = Query::new(lang, CALL_QUERY).context("Invalid Python call query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let call_idx = q.capture_index_for_name("call_name").unwrap();
        let method_idx = q.capture_index_for_name("method_call_name").unwrap();

        for cap in m.captures {
            if cap.index == call_idx || cap.index == method_idx {
                let line = cap.node.start_position().row;
                references.push(Reference {
                    kind: RefKind::Call, target_name: node_text(cap.node, src).to_string(),
                    target_qualifier: None, line,
                    source_symbol_name: find_enclosing_symbol(symbols, line),
                });
            }
        }
    });
    Ok(())
}

fn process_simple_import(node: tree_sitter::Node, src: &[u8], imports: &mut Vec<Import>) {
    let full_path = node_text(node, src).to_string();
    let local_name = full_path.rsplit('.').next().unwrap_or(&full_path).to_string();
    imports.push(Import { local_name, full_path, alias: None, line: node.start_position().row });
}

fn process_from_import(m: &tree_sitter::QueryMatch, cap_node: tree_sitter::Node, src: &[u8], mod_idx: Option<u32>, imports: &mut Vec<Import>) {
    let name = node_text(cap_node, src).to_string();
    let module = mod_idx.and_then(|idx| cap_text(m, idx, src)).unwrap_or("");
    let full_path = if module.is_empty() { name.clone() } else { format!("{}.{}", module, name) };
    imports.push(Import { local_name: name, full_path, alias: None, line: cap_node.start_position().row });
}

fn process_aliased_import(m: &tree_sitter::QueryMatch, src: &[u8], an_idx: u32, al_idx: u32, mod_idx: Option<u32>, imports: &mut Vec<Import>) {
    if let (Some(name_node), Some(alias_node)) = (cap_node(m, an_idx), cap_node(m, al_idx)) {
        let name = node_text(name_node, src).to_string();
        let alias = node_text(alias_node, src).to_string();
        let module = mod_idx.and_then(|idx| cap_text(m, idx, src)).unwrap_or("");
        let full_path = if module.is_empty() { name } else { format!("{}.{}", module, name) };
        imports.push(Import { local_name: alias.clone(), full_path, alias: Some(alias), line: name_node.start_position().row });
    }
}

fn parse_imports(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, imports: &mut Vec<Import>) -> Result<()> {
    let query = Query::new(lang, IMPORT_QUERY).context("Invalid Python import query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let import_idx = q.capture_index_for_name("import_name").unwrap();
        let mod_idx = q.capture_index_for_name("module_name");
        let from_idx = q.capture_index_for_name("from_name");
        let mod2_idx = q.capture_index_for_name("module_name2");
        let an_idx = q.capture_index_for_name("aliased_name");
        let al_idx = q.capture_index_for_name("alias");

        let has_from = from_idx.is_some_and(|idx| m.captures.iter().any(|c| c.index == idx));
        let has_aliased = an_idx.is_some_and(|idx| m.captures.iter().any(|c| c.index == idx));

        if let Some(cap) = m.captures.iter().find(|c| c.index == import_idx) {
            if !has_from && !has_aliased {
                process_simple_import(cap.node, src, imports);
            }
        }
        if let Some(fi) = from_idx {
            if let Some(cap) = m.captures.iter().find(|c| c.index == fi) {
                process_from_import(m, cap.node, src, mod_idx, imports);
            }
        }
        if let (Some(ai), Some(ali)) = (an_idx, al_idx) {
            process_aliased_import(m, src, ai, ali, mod2_idx, imports);
        }
    });
    Ok(())
}

fn find_parent_class_py<'a>(node: tree_sitter::Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_definition" {
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i) {
                    if child.kind() == "identifier" { return Some(node_text(child, src).to_string()); }
                }
            }
        }
        current = parent.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_python_class() {
        let src = "class Animal:\n    def __init__(self, name):\n        self.name = name\n\nclass Dog(Animal):\n    def speak(self):\n        return 'Woof'\n";
        let result = parse(src).unwrap();
        assert!(result.symbols.iter().any(|s| s.name == "Animal" && s.kind == SymbolKind::Class));
        assert!(result.symbols.iter().any(|s| s.name == "__init__" && s.kind == SymbolKind::Method));
        assert!(result.references.iter().any(|r| r.target_name == "Animal" && r.kind == RefKind::Inherit));
    }

    #[test]
    fn test_parse_python_imports() {
        let src = "import os\nfrom collections import OrderedDict\nfrom typing import List as TypingList\n";
        let result = parse(src).unwrap();
        assert!(result.imports.iter().any(|i| i.full_path == "os"));
        assert!(result.imports.iter().any(|i| i.local_name == "OrderedDict"));
        assert!(result.imports.iter().any(|i| i.local_name == "TypingList"));
    }

    #[test]
    fn test_parse_python_calls() {
        let src = "def process():\n    result = calculate(10)\n    data.transform()\n    print('hello')\n";
        let result = parse(src).unwrap();
        let names: Vec<&str> = result.references.iter().filter(|r| r.kind == RefKind::Call).map(|r| r.target_name.as_str()).collect();
        assert!(names.contains(&"calculate"));
        assert!(names.contains(&"transform"));
        assert!(names.contains(&"print"));
    }
}
