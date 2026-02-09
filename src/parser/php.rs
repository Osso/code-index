use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{Import, ParseResult, RefKind, Reference, Symbol, SymbolKind};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

const SYMBOL_QUERY: &str = r#"
(function_definition
    name: (name) @fn_name
    parameters: (formal_parameters) @fn_params
) @fn_node

(method_declaration
    name: (name) @method_name
    parameters: (formal_parameters) @method_params
) @method_node

(class_declaration
    name: (name) @class_name
) @class_node

(interface_declaration
    name: (name) @iface_name
) @iface_node

(trait_declaration
    name: (name) @trait_name
) @trait_node
"#;

const CALL_QUERY: &str = r#"
(function_call_expression
    function: (name) @call_name
) @call_node

(function_call_expression
    function: (qualified_name) @qualified_call
) @qualified_call_node

(member_call_expression
    name: (name) @member_call_name
) @member_call_node

(scoped_call_expression
    scope: (_) @scope
    name: (name) @scoped_call_name
) @scoped_call_node

(object_creation_expression
    (name) @new_class_name
) @new_node

(object_creation_expression
    (qualified_name) @new_qualified_name
) @new_qualified_node
"#;

const USE_QUERY: &str = r#"
(namespace_use_declaration
    (namespace_use_clause
        (qualified_name) @use_path
        alias: (name)? @alias
    )
) @use_node
"#;

const INHERITANCE_QUERY: &str = r#"
(class_declaration
    name: (name) @class_name
    (base_clause (name) @base_name)?
    (base_clause (qualified_name) @base_qualified)?
    (class_interface_clause
        (name) @iface_name
    )*
    (class_interface_clause
        (qualified_name) @iface_qualified
    )*
) @class_node
"#;

pub fn parse(source: &str) -> Result<ParseResult> {
    let lang: tree_sitter::Language = tree_sitter_php::LANGUAGE_PHP.into();
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).context("Failed to set PHP language")?;

    let tree = parser.parse(source, None).context("Failed to parse PHP source")?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();

    parse_symbols(root, src, &lang, &mut symbols)?;
    parse_calls(root, src, &lang, &symbols, &mut references)?;
    parse_uses(root, src, &lang, &mut imports)?;
    parse_inheritance(root, src, &lang, &mut references)?;

    Ok(ParseResult { symbols, references, imports })
}

fn cap_node<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32) -> Option<tree_sitter::Node<'a>> {
    m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
}

fn cap_text<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32, src: &'a [u8]) -> Option<&'a str> {
    cap_node(m, idx).map(|n| node_text(n, src))
}

fn make_call_ref(name: &str, qualifier: Option<String>, line: usize, source: Option<String>) -> Reference {
    Reference { kind: RefKind::Call, target_name: name.to_string(), target_qualifier: qualifier, line, source_symbol_name: source }
}

fn extract_qualified(text: &str) -> (String, Option<String>) {
    let name = text.rsplit('\\').next().unwrap_or(text).to_string();
    (name, Some(text.to_string()))
}

fn build_fn_sym(m: &tree_sitter::QueryMatch, name_node: tree_sitter::Node, src: &[u8], node_idx: u32, params_idx: u32) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let container = cap_node(m, node_idx).unwrap_or(name_node);
    let params = cap_text(m, params_idx, src).unwrap_or("");
    Symbol {
        name, kind: SymbolKind::Function,
        line_start: container.start_position().row, line_end: container.end_position().row,
        parent_name: None, visibility: None, signature: Some(format!("function {}", params)),
        is_test: false,
    }
}

fn build_method_sym(m: &tree_sitter::QueryMatch, name_node: tree_sitter::Node, src: &[u8], node_idx: u32, params_idx: u32) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let container = cap_node(m, node_idx).unwrap_or(name_node);
    let params = cap_text(m, params_idx, src).unwrap_or("");
    let parent = find_parent_class(name_node, src);
    let is_test = is_php_test(&name, parent.as_deref());
    Symbol {
        name, kind: SymbolKind::Method,
        line_start: container.start_position().row, line_end: container.end_position().row,
        parent_name: parent,
        visibility: extract_php_visibility(container, src),
        signature: Some(format!("function {}", params)),
        is_test,
    }
}

fn build_type_sym(m: &tree_sitter::QueryMatch, name_node: tree_sitter::Node, src: &[u8], node_idx: u32, kind: SymbolKind) -> Symbol {
    let name = node_text(name_node, src).to_string();
    let container = cap_node(m, node_idx).unwrap_or(name_node);
    Symbol {
        name, kind,
        line_start: container.start_position().row, line_end: container.end_position().row,
        parent_name: None, visibility: None, signature: None,
        is_test: false,
    }
}

/// PHPUnit: testFoo(), Codeception: class FooCest / FooTest with test* methods
fn is_php_test(method_name: &str, parent_class: Option<&str>) -> bool {
    if !method_name.starts_with("test") {
        return false;
    }
    match parent_class {
        Some(cls) => cls.ends_with("Test") || cls.ends_with("Cest"),
        None => false,
    }
}

fn parse_symbols(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, symbols: &mut Vec<Symbol>) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid PHP symbol query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let fn_n = q.capture_index_for_name("fn_name").unwrap();
        let fn_p = q.capture_index_for_name("fn_params").unwrap();
        let fn_nd = q.capture_index_for_name("fn_node").unwrap();
        let mn = q.capture_index_for_name("method_name").unwrap();
        let mp = q.capture_index_for_name("method_params").unwrap();
        let mnd = q.capture_index_for_name("method_node").unwrap();
        let cn = q.capture_index_for_name("class_name").unwrap();
        let cnd = q.capture_index_for_name("class_node").unwrap();
        let iface_n = q.capture_index_for_name("iface_name").unwrap();
        let iface_nd = q.capture_index_for_name("iface_node").unwrap();
        let tn = q.capture_index_for_name("trait_name").unwrap();
        let tnd = q.capture_index_for_name("trait_node").unwrap();

        for cap in m.captures {
            let sym = match cap.index {
                i if i == fn_n => Some(build_fn_sym(m, cap.node, src, fn_nd, fn_p)),
                i if i == mn => Some(build_method_sym(m, cap.node, src, mnd, mp)),
                i if i == cn => Some(build_type_sym(m, cap.node, src, cnd, SymbolKind::Class)),
                i if i == iface_n => Some(build_type_sym(m, cap.node, src, iface_nd, SymbolKind::Interface)),
                i if i == tn => Some(build_type_sym(m, cap.node, src, tnd, SymbolKind::Trait)),
                _ => None,
            };
            if let Some(s) = sym { symbols.push(s); }
        }
    });
    Ok(())
}

fn parse_calls(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, symbols: &[Symbol], references: &mut Vec<Reference>) -> Result<()> {
    let query = Query::new(lang, CALL_QUERY).context("Invalid PHP call query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let call_idx = q.capture_index_for_name("call_name").unwrap();
        let qual_idx = q.capture_index_for_name("qualified_call").unwrap();
        let member_idx = q.capture_index_for_name("member_call_name").unwrap();
        let scoped_idx = q.capture_index_for_name("scoped_call_name").unwrap();
        let scope_idx = q.capture_index_for_name("scope").unwrap();
        let new_idx = q.capture_index_for_name("new_class_name").unwrap();
        let new_q_idx = q.capture_index_for_name("new_qualified_name").unwrap();

        for cap in m.captures {
            let line = cap.node.start_position().row;
            let src_sym = find_enclosing_symbol(symbols, line);
            let text = node_text(cap.node, src);

            match cap.index {
                i if i == call_idx || i == member_idx || i == new_idx => {
                    references.push(make_call_ref(text, None, line, src_sym));
                }
                i if i == qual_idx || i == new_q_idx => {
                    let (name, qualifier) = extract_qualified(text);
                    references.push(make_call_ref(&name, qualifier, line, src_sym));
                }
                i if i == scoped_idx => {
                    let qualifier = cap_text(m, scope_idx, src).map(|s| s.to_string());
                    references.push(make_call_ref(text, qualifier, line, src_sym));
                }
                _ => {}
            }
        }
    });
    Ok(())
}

fn parse_uses(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, imports: &mut Vec<Import>) -> Result<()> {
    let query = Query::new(lang, USE_QUERY).context("Invalid PHP use query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let path_idx = q.capture_index_for_name("use_path").unwrap();
        let alias_idx = q.capture_index_for_name("alias");

        if let Some(cap) = m.captures.iter().find(|c| c.index == path_idx) {
            let full_path = node_text(cap.node, src).to_string();
            let alias = alias_idx.and_then(|idx| cap_text(m, idx, src).map(|s| s.to_string()));
            let local_name = alias.clone().unwrap_or_else(|| {
                full_path.rsplit('\\').next().unwrap_or(&full_path).to_string()
            });
            imports.push(Import { local_name, full_path, alias, line: cap.node.start_position().row });
        }
    });
    Ok(())
}

fn emit_inh_ref(cap: &tree_sitter::QueryCapture, src: &[u8], kind: RefKind, cls: &Option<String>, qualified: bool, refs: &mut Vec<Reference>) {
    let text = node_text(cap.node, src);
    let (name, qualifier) = if qualified { extract_qualified(text) } else { (text.to_string(), None) };
    refs.push(Reference { kind, target_name: name, target_qualifier: qualifier, line: cap.node.start_position().row, source_symbol_name: cls.clone() });
}

fn parse_inheritance(root: tree_sitter::Node, src: &[u8], lang: &tree_sitter::Language, references: &mut Vec<Reference>) -> Result<()> {
    let query = Query::new(lang, INHERITANCE_QUERY).context("Invalid PHP inheritance query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let cn = q.capture_index_for_name("class_name").unwrap();
        let bn = q.capture_index_for_name("base_name");
        let bq = q.capture_index_for_name("base_qualified");
        let iface_n = q.capture_index_for_name("iface_name");
        let iface_q = q.capture_index_for_name("iface_qualified");

        let cls = cap_text(m, cn, src).map(|s| s.to_string());

        for cap in m.captures {
            if bn.is_some_and(|idx| cap.index == idx) {
                emit_inh_ref(cap, src, RefKind::Inherit, &cls, false, references);
            } else if bq.is_some_and(|idx| cap.index == idx) {
                emit_inh_ref(cap, src, RefKind::Inherit, &cls, true, references);
            } else if iface_n.is_some_and(|idx| cap.index == idx) {
                emit_inh_ref(cap, src, RefKind::Implement, &cls, false, references);
            } else if iface_q.is_some_and(|idx| cap.index == idx) {
                emit_inh_ref(cap, src, RefKind::Implement, &cls, true, references);
            }
        }
    });
    Ok(())
}

fn find_parent_class<'a>(node: tree_sitter::Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        let kind = parent.kind();
        if kind == "class_declaration" || kind == "trait_declaration" || kind == "interface_declaration" {
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i) {
                    if child.kind() == "name" { return Some(node_text(child, src).to_string()); }
                }
            }
        }
        current = parent.parent();
    }
    None
}

fn extract_php_visibility(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            let kind = child.kind();
            if kind == "visibility_modifier" || kind == "static_modifier" || kind == "abstract_modifier" {
                return Some(node_text(child, src).to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_php_class() {
        let src = "<?php\nclass Foo extends Bar implements Baz {\n    public function hello() { return 1; }\n}\n";
        let result = parse(src).unwrap();
        assert!(result.symbols.iter().any(|s| s.name == "Foo" && s.kind == SymbolKind::Class));
        assert!(result.symbols.iter().any(|s| s.name == "hello" && s.kind == SymbolKind::Method));
        assert!(result.references.iter().any(|r| r.target_name == "Bar" && r.kind == RefKind::Inherit));
        assert!(result.references.iter().any(|r| r.target_name == "Baz" && r.kind == RefKind::Implement));
    }

    #[test]
    fn test_parse_php_use_statements() {
        let src = "<?php\nuse App\\Models\\User;\nuse Illuminate\\Http\\Request as HttpRequest;\n";
        let result = parse(src).unwrap();
        assert_eq!(result.imports.len(), 2);
        assert_eq!(result.imports[0].local_name, "User");
        assert_eq!(result.imports[1].local_name, "HttpRequest");
    }

    #[test]
    fn test_parse_php_calls() {
        let src = "<?php\nfunction process() {\n    $x = calculate(10);\n    $user->save();\n    User::find(1);\n}\n";
        let result = parse(src).unwrap();
        let names: Vec<&str> = result.references.iter().filter(|r| r.kind == RefKind::Call).map(|r| r.target_name.as_str()).collect();
        assert!(names.contains(&"calculate"));
        assert!(names.contains(&"save"));
        assert!(names.contains(&"find"));
    }
}
