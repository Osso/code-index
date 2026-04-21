use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{Import, ParseResult, RefKind, Reference, Symbol, SymbolKind};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

const SYMBOL_QUERY: &str = r#"
(function_declaration
    name: (identifier) @fn_name
    parameters: (formal_parameters) @fn_params
) @fn_node

(class_declaration
    name: (type_identifier) @class_name
) @class_node

(abstract_class_declaration
    name: (type_identifier) @abstract_class_name
) @abstract_class_node

(interface_declaration
    name: (type_identifier) @iface_name
) @iface_node

(method_definition
    name: (property_identifier) @method_name
    parameters: (formal_parameters) @method_params
) @method_node

(enum_declaration
    name: (identifier) @enum_name
) @enum_node
"#;

const ARROW_FN_QUERY: &str = r#"
(variable_declarator
    name: (identifier) @arrow_name
    value: (arrow_function
        parameters: (formal_parameters) @arrow_params
    ) @arrow_node
)

(variable_declarator
    name: (identifier) @fn_expr_name
    value: (function_expression
        parameters: (formal_parameters) @fn_expr_params
    ) @fn_expr_node
)
"#;

const CALL_QUERY: &str = r#"
(call_expression
    function: (identifier) @call_name
) @call_node

(call_expression
    function: (member_expression
        property: (property_identifier) @method_call_name
    )
) @method_call_node

(new_expression
    constructor: (identifier) @new_name
) @new_node

(new_expression
    constructor: (member_expression
        property: (property_identifier) @new_member_name
    )
) @new_member_node

(jsx_self_closing_element
    name: (identifier) @jsx_component_name
) @jsx_self_closing_node

(jsx_opening_element
    name: (identifier) @jsx_component_name
) @jsx_opening_node
"#;

const IMPORT_QUERY: &str = r#"
(import_statement
    (import_clause
        (named_imports
            (import_specifier
                name: (identifier) @import_name
                alias: (identifier)? @import_alias
            )
        )
    )?
    (import_clause
        (identifier) @default_import
    )?
    source: (string) @import_source
) @import_node
"#;

const TEST_CALLBACK_QUERY: &str = r#"
(call_expression
    function: (identifier) @test_fn
    arguments: (arguments
        [
            (arrow_function) @test_cb
            (function_expression) @test_cb
        ]
    )
) @test_call

(call_expression
    function: (member_expression
        object: (identifier) @test_obj
        property: (property_identifier) @test_prop
    )
    arguments: (arguments
        [
            (arrow_function) @test_member_cb
            (function_expression) @test_member_cb
        ]
    )
) @test_member_call
"#;

pub fn parse(source: &str) -> Result<ParseResult> {
    let lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&lang)
        .context("Failed to set TypeScript language")?;

    let tree = parser
        .parse(source, None)
        .context("Failed to parse TypeScript source")?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();

    parse_symbols(root, src, &lang, &mut symbols)?;
    parse_arrow_functions(root, src, &lang, &mut symbols)?;
    parse_test_callbacks(root, src, &lang, &mut symbols)?;
    parse_calls(root, src, &lang, &symbols, &mut references)?;
    parse_imports(root, src, &lang, &mut imports)?;
    parse_inheritance(root, src, &mut references)?;

    Ok(ParseResult {
        symbols,
        references,
        imports,
    })
}

fn cap_node<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32) -> Option<tree_sitter::Node<'a>> {
    m.captures.iter().find(|c| c.index == idx).map(|c| c.node)
}

fn cap_text<'a>(m: &tree_sitter::QueryMatch<'_, 'a>, idx: u32, src: &'a [u8]) -> Option<&'a str> {
    cap_node(m, idx).map(|n| node_text(n, src))
}

fn build_fn_sym(
    m: &tree_sitter::QueryMatch,
    nn: tree_sitter::Node,
    src: &[u8],
    ni: u32,
    pi: u32,
) -> Symbol {
    let fn_node = cap_node(m, ni).unwrap_or(nn);
    let vis = extract_ts_export(fn_node);
    build_arrow_sym(m, nn, src, ni, pi, "function", vis)
}

fn build_class_sym(
    m: &tree_sitter::QueryMatch,
    nn: tree_sitter::Node,
    src: &[u8],
    ni: u32,
    vis: Option<String>,
) -> Symbol {
    let name = node_text(nn, src).to_string();
    let container = cap_node(m, ni).unwrap_or(nn);
    Symbol {
        name,
        kind: SymbolKind::Class,
        line_start: container.start_position().row,
        line_end: container.end_position().row,
        parent_name: None,
        visibility: vis,
        signature: None,
        is_test: false,
    }
}

fn build_method_sym(
    m: &tree_sitter::QueryMatch,
    nn: tree_sitter::Node,
    src: &[u8],
    ni: u32,
    pi: u32,
) -> Symbol {
    let name = node_text(nn, src).to_string();
    let method_node = cap_node(m, ni).unwrap_or(nn);
    let params = cap_text(m, pi, src).unwrap_or("");
    Symbol {
        name,
        kind: SymbolKind::Method,
        line_start: method_node.start_position().row,
        line_end: method_node.end_position().row,
        parent_name: find_parent_class_ts(nn, src),
        visibility: extract_method_vis(method_node, src),
        signature: Some(format!("method {}", params)),
        is_test: false,
    }
}

fn build_type_sym(
    m: &tree_sitter::QueryMatch,
    nn: tree_sitter::Node,
    src: &[u8],
    ni: u32,
    kind: SymbolKind,
) -> Symbol {
    let name = node_text(nn, src).to_string();
    let container = cap_node(m, ni).unwrap_or(nn);
    Symbol {
        name,
        kind,
        line_start: container.start_position().row,
        line_end: container.end_position().row,
        parent_name: None,
        visibility: extract_ts_export(container),
        signature: None,
        is_test: false,
    }
}

#[derive(Clone, Copy)]
struct TsSymbolCaptureIndices {
    fn_name: u32,
    fn_params: u32,
    fn_node: u32,
    class_name: u32,
    class_node: u32,
    abstract_class_name: u32,
    abstract_class_node: u32,
    iface_name: u32,
    iface_node: u32,
    method_name: u32,
    method_params: u32,
    method_node: u32,
    enum_name: u32,
    enum_node: u32,
}

fn ts_symbol_capture_indices(query: &Query) -> TsSymbolCaptureIndices {
    TsSymbolCaptureIndices {
        fn_name: query.capture_index_for_name("fn_name").unwrap(),
        fn_params: query.capture_index_for_name("fn_params").unwrap(),
        fn_node: query.capture_index_for_name("fn_node").unwrap(),
        class_name: query.capture_index_for_name("class_name").unwrap(),
        class_node: query.capture_index_for_name("class_node").unwrap(),
        abstract_class_name: query.capture_index_for_name("abstract_class_name").unwrap(),
        abstract_class_node: query.capture_index_for_name("abstract_class_node").unwrap(),
        iface_name: query.capture_index_for_name("iface_name").unwrap(),
        iface_node: query.capture_index_for_name("iface_node").unwrap(),
        method_name: query.capture_index_for_name("method_name").unwrap(),
        method_params: query.capture_index_for_name("method_params").unwrap(),
        method_node: query.capture_index_for_name("method_node").unwrap(),
        enum_name: query.capture_index_for_name("enum_name").unwrap(),
        enum_node: query.capture_index_for_name("enum_node").unwrap(),
    }
}

fn build_ts_symbol_for_capture<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    capture_index: u32,
    capture_node: tree_sitter::Node<'a>,
    src: &[u8],
    indices: TsSymbolCaptureIndices,
) -> Option<Symbol> {
    build_ts_callable_or_class_symbol(query_match, capture_index, capture_node, src, indices)
        .or_else(|| build_ts_type_symbol(query_match, capture_index, capture_node, src, indices))
}

fn build_ts_callable_or_class_symbol<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    capture_index: u32,
    capture_node: tree_sitter::Node<'a>,
    src: &[u8],
    indices: TsSymbolCaptureIndices,
) -> Option<Symbol> {
    match capture_index {
        i if i == indices.fn_name => Some(build_fn_sym(
            query_match,
            capture_node,
            src,
            indices.fn_node,
            indices.fn_params,
        )),
        i if i == indices.class_name => Some(build_class_sym(
            query_match,
            capture_node,
            src,
            indices.class_node,
            extract_ts_export(capture_node),
        )),
        i if i == indices.abstract_class_name => Some(build_class_sym(
            query_match,
            capture_node,
            src,
            indices.abstract_class_node,
            Some("abstract".into()),
        )),
        i if i == indices.method_name => Some(build_method_sym(
            query_match,
            capture_node,
            src,
            indices.method_node,
            indices.method_params,
        )),
        _ => None,
    }
}

fn build_ts_type_symbol<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    capture_index: u32,
    capture_node: tree_sitter::Node<'a>,
    src: &[u8],
    indices: TsSymbolCaptureIndices,
) -> Option<Symbol> {
    match capture_index {
        i if i == indices.iface_name => Some(build_type_sym(
            query_match,
            capture_node,
            src,
            indices.iface_node,
            SymbolKind::Interface,
        )),
        i if i == indices.enum_name => Some(build_type_sym(
            query_match,
            capture_node,
            src,
            indices.enum_node,
            SymbolKind::Enum,
        )),
        _ => None,
    }
}

fn parse_symbols(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid TS symbol query")?;
    let indices = ts_symbol_capture_indices(&query);

    for_each_match(&query, root, src, |m, _, _| {
        for cap in m.captures {
            if let Some(symbol) = build_ts_symbol_for_capture(m, cap.index, cap.node, src, indices)
            {
                symbols.push(symbol);
            }
        }
    });
    Ok(())
}

fn parse_arrow_functions(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(lang, ARROW_FN_QUERY).context("Invalid TS arrow fn query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let an = q.capture_index_for_name("arrow_name").unwrap();
        let ap = q.capture_index_for_name("arrow_params").unwrap();
        let and = q.capture_index_for_name("arrow_node").unwrap();
        let fen = q.capture_index_for_name("fn_expr_name").unwrap();
        let fep = q.capture_index_for_name("fn_expr_params").unwrap();
        let fend = q.capture_index_for_name("fn_expr_node").unwrap();

        for cap in m.captures {
            if cap.index == an {
                symbols.push(build_arrow_sym(m, cap.node, src, and, ap, "const =>", None));
            } else if cap.index == fen {
                symbols.push(build_arrow_sym(
                    m,
                    cap.node,
                    src,
                    fend,
                    fep,
                    "const function",
                    None,
                ));
            }
        }
    });
    Ok(())
}

fn find_test_callback_node<'a>(
    m: &tree_sitter::QueryMatch<'_, 'a>,
    q: &Query,
    src: &[u8],
) -> Option<tree_sitter::Node<'a>> {
    if let (Some(fn_idx), Some(cb_idx)) = (
        q.capture_index_for_name("test_fn"),
        q.capture_index_for_name("test_cb"),
    ) {
        if cap_text(m, fn_idx, src).is_some_and(is_ts_test_fn) {
            return cap_node(m, cb_idx);
        }
    }

    if let (Some(obj_idx), Some(prop_idx), Some(cb_idx)) = (
        q.capture_index_for_name("test_obj"),
        q.capture_index_for_name("test_prop"),
        q.capture_index_for_name("test_member_cb"),
    ) {
        let obj = cap_text(m, obj_idx, src).unwrap_or("");
        let prop = cap_text(m, prop_idx, src).unwrap_or("");
        if is_ts_test_fn(obj) && is_ts_test_modifier(prop) {
            return cap_node(m, cb_idx);
        }
    }

    None
}

fn build_test_callback_sym(cb: tree_sitter::Node, sequence: usize) -> Symbol {
    let line = cb.start_position().row;
    Symbol {
        name: format!("__ts_test_{}_{}", line, sequence),
        kind: SymbolKind::Function,
        line_start: line,
        line_end: cb.end_position().row,
        parent_name: None,
        visibility: None,
        signature: Some("test callback".to_string()),
        is_test: true,
    }
}

fn parse_test_callbacks(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(lang, TEST_CALLBACK_QUERY).context("Invalid TS test callback query")?;
    let mut sequence = 0usize;

    for_each_match(&query, root, src, |m, q, _| {
        if let Some(cb) = find_test_callback_node(m, q, src) {
            sequence += 1;
            symbols.push(build_test_callback_sym(cb, sequence));
        }
    });

    Ok(())
}

fn is_ts_test_fn(name: &str) -> bool {
    matches!(name, "test" | "it")
}

fn is_ts_test_modifier(name: &str) -> bool {
    matches!(name, "only" | "skip" | "concurrent" | "fails")
}

fn build_arrow_sym(
    m: &tree_sitter::QueryMatch,
    nn: tree_sitter::Node,
    src: &[u8],
    ni: u32,
    pi: u32,
    prefix: &str,
    visibility: Option<String>,
) -> Symbol {
    let name = node_text(nn, src).to_string();
    let fn_node = cap_node(m, ni).unwrap_or(nn);
    let params = cap_text(m, pi, src).unwrap_or("");
    Symbol {
        name,
        kind: SymbolKind::Function,
        line_start: fn_node.start_position().row,
        line_end: fn_node.end_position().row,
        parent_name: None,
        visibility,
        signature: Some(format!("{} {}", prefix, params)),
        is_test: false,
    }
}

fn parse_calls(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &[Symbol],
    references: &mut Vec<Reference>,
) -> Result<()> {
    let query = Query::new(lang, CALL_QUERY).context("Invalid TS call query")?;
    let indices = ts_call_capture_indices(&query);

    for_each_match(&query, root, src, |m, _, _| {
        for cap in m.captures {
            if let Some(reference) = ts_call_reference_for_capture(cap, src, symbols, indices) {
                references.push(reference);
            }
        }
    });
    Ok(())
}

#[derive(Clone, Copy)]
struct TsCallCaptureIndices {
    call_name: u32,
    method_call_name: u32,
    new_name: u32,
    new_member_name: u32,
    jsx_component_name: u32,
}

fn ts_call_capture_indices(query: &Query) -> TsCallCaptureIndices {
    TsCallCaptureIndices {
        call_name: query.capture_index_for_name("call_name").unwrap(),
        method_call_name: query.capture_index_for_name("method_call_name").unwrap(),
        new_name: query.capture_index_for_name("new_name").unwrap(),
        new_member_name: query.capture_index_for_name("new_member_name").unwrap(),
        jsx_component_name: query.capture_index_for_name("jsx_component_name").unwrap(),
    }
}

fn ts_call_reference_for_capture(
    capture: &tree_sitter::QueryCapture<'_>,
    src: &[u8],
    symbols: &[Symbol],
    indices: TsCallCaptureIndices,
) -> Option<Reference> {
    if is_direct_ts_call_capture(capture.index, indices) {
        return Some(build_ts_call_reference(
            node_text(capture.node, src),
            capture.node,
            symbols,
        ));
    }
    if capture.index == indices.jsx_component_name
        && is_jsx_component_name(node_text(capture.node, src))
    {
        return Some(build_ts_call_reference(
            node_text(capture.node, src),
            capture.node,
            symbols,
        ));
    }
    None
}

fn is_direct_ts_call_capture(capture_index: u32, indices: TsCallCaptureIndices) -> bool {
    capture_index == indices.call_name
        || capture_index == indices.method_call_name
        || capture_index == indices.new_name
        || capture_index == indices.new_member_name
}

fn is_jsx_component_name(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

fn build_ts_call_reference(name: &str, node: tree_sitter::Node, symbols: &[Symbol]) -> Reference {
    let line = node.start_position().row;
    Reference {
        kind: RefKind::Call,
        target_name: name.to_string(),
        target_qualifier: None,
        line,
        source_symbol_name: find_enclosing_symbol(symbols, line),
    }
}

fn collect_named_imports(
    m: &tree_sitter::QueryMatch,
    src: &[u8],
    name_idx: u32,
    alias_idx: Option<u32>,
    source_path: &str,
    imports: &mut Vec<Import>,
) {
    for cap in m.captures.iter().filter(|c| c.index == name_idx) {
        let name = node_text(cap.node, src).to_string();
        let alias = alias_idx.and_then(|idx| cap_text(m, idx, src).map(|s| s.to_string()));
        let local = alias.clone().unwrap_or_else(|| name.clone());
        imports.push(Import {
            local_name: local,
            full_path: format!("{}.{}", source_path, name),
            alias,
            line: cap.node.start_position().row,
        });
    }
}

fn collect_default_import(
    m: &tree_sitter::QueryMatch,
    src: &[u8],
    def_idx: u32,
    source_path: &str,
    imports: &mut Vec<Import>,
) {
    if let Some(cap) = m.captures.iter().find(|c| c.index == def_idx) {
        imports.push(Import {
            local_name: node_text(cap.node, src).to_string(),
            full_path: format!("{}.default", source_path),
            alias: None,
            line: cap.node.start_position().row,
        });
    }
}

fn parse_imports(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    imports: &mut Vec<Import>,
) -> Result<()> {
    let query = Query::new(lang, IMPORT_QUERY).context("Invalid TS import query")?;

    for_each_match(&query, root, src, |m, q, _| {
        let src_idx = q.capture_index_for_name("import_source").unwrap();
        let source_path = cap_text(m, src_idx, src)
            .map(|s| s.trim_matches(|c| c == '\'' || c == '"'))
            .unwrap_or("");

        if let Some(ni) = q.capture_index_for_name("import_name") {
            collect_named_imports(
                m,
                src,
                ni,
                q.capture_index_for_name("import_alias"),
                source_path,
                imports,
            );
        }
        if let Some(di) = q.capture_index_for_name("default_import") {
            collect_default_import(m, src, di, source_path, imports);
        }
    });
    Ok(())
}

fn parse_inheritance(
    root: tree_sitter::Node,
    src: &[u8],
    references: &mut Vec<Reference>,
) -> Result<()> {
    walk_for_inheritance(root, src, references);
    Ok(())
}

fn walk_for_inheritance(node: tree_sitter::Node, src: &[u8], refs: &mut Vec<Reference>) {
    let kind = node.kind();
    if kind == "class_declaration" || kind == "abstract_class_declaration" {
        let class_name = find_class_name(node, src);
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "class_heritage" {
                    parse_heritage(child, src, &class_name, refs);
                }
            }
        }
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            walk_for_inheritance(child, src, refs);
        }
    }
}

fn parse_heritage(
    node: tree_sitter::Node,
    src: &[u8],
    cls: &Option<String>,
    refs: &mut Vec<Reference>,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "extends_clause" => emit_heritage(child, src, RefKind::Inherit, cls, refs),
                "implements_clause" => emit_heritage(child, src, RefKind::Implement, cls, refs),
                _ => {}
            }
        }
    }
}

fn emit_heritage(
    node: tree_sitter::Node,
    src: &[u8],
    kind: RefKind,
    cls: &Option<String>,
    refs: &mut Vec<Reference>,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            let target = match child.kind() {
                "type_identifier" | "identifier" => Some(node_text(child, src).to_string()),
                "generic_type" => child.child(0).map(|c| node_text(c, src).to_string()),
                _ => None,
            };
            if let Some(name) = target {
                refs.push(Reference {
                    kind: kind.clone(),
                    target_name: name,
                    target_qualifier: None,
                    line: child.start_position().row,
                    source_symbol_name: cls.clone(),
                });
            }
        }
    }
}

fn find_class_name(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "type_identifier" {
                return Some(node_text(child, src).to_string());
            }
        }
    }
    None
}

fn find_parent_class_ts<'a>(node: tree_sitter::Node<'a>, src: &'a [u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_declaration" || parent.kind() == "abstract_class_declaration" {
            return find_class_name(parent, src);
        }
        current = parent.parent();
    }
    None
}

fn extract_ts_export(node: tree_sitter::Node) -> Option<String> {
    node.parent()
        .filter(|p| p.kind() == "export_statement")
        .map(|_| "export".to_string())
}

fn extract_method_vis(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "accessibility_modifier" {
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
    fn test_parse_ts_class() {
        let src = "class Svc extends Base implements Ser {\n    public getName(): string { return ''; }\n}\n";
        let result = parse(src).unwrap();
        assert!(
            result
                .symbols
                .iter()
                .any(|s| s.name == "Svc" && s.kind == SymbolKind::Class)
        );
        assert!(
            result
                .symbols
                .iter()
                .any(|s| s.name == "getName" && s.kind == SymbolKind::Method)
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.target_name == "Base" && r.kind == RefKind::Inherit)
        );
        assert!(
            result
                .references
                .iter()
                .any(|r| r.target_name == "Ser" && r.kind == RefKind::Implement)
        );
    }

    #[test]
    fn test_parse_ts_arrow() {
        let src = "const greet = (name: string) => { return name; };\nexport function add(a: number, b: number) { return a + b; }\n";
        let result = parse(src).unwrap();
        assert!(result.symbols.iter().any(|s| s.name == "greet"));
        assert!(result.symbols.iter().any(|s| s.name == "add"));
    }

    #[test]
    fn test_parse_ts_calls() {
        let src = "function main() {\n    const app = express();\n    app.listen(3000);\n    const user = new User('test');\n}\n";
        let result = parse(src).unwrap();
        let names: Vec<&str> = result
            .references
            .iter()
            .filter(|r| r.kind == RefKind::Call)
            .map(|r| r.target_name.as_str())
            .collect();
        assert!(names.contains(&"express"));
        assert!(names.contains(&"listen"));
        assert!(names.contains(&"User"));
    }

    #[test]
    fn test_parse_ts_vitest_callbacks_as_tests() {
        let src = "it('renders login', () => {\n    render(<Login />);\n    fireEvent.click(button);\n});\n\ntest.only('search flow', function () {\n    runSearch();\n});\n";
        let result = parse(src).unwrap();

        let test_symbols: Vec<&Symbol> = result.symbols.iter().filter(|s| s.is_test).collect();
        assert_eq!(test_symbols.len(), 2);

        let refs_from_tests: Vec<&Reference> = result
            .references
            .iter()
            .filter(|r| {
                r.kind == RefKind::Call
                    && r.source_symbol_name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("__ts_test_"))
            })
            .collect();

        assert!(refs_from_tests.iter().any(|r| r.target_name == "render"));
        assert!(refs_from_tests.iter().any(|r| r.target_name == "click"));
        assert!(refs_from_tests.iter().any(|r| r.target_name == "runSearch"));
    }

    #[test]
    fn test_parse_ts_jsx_component_refs_from_tests() {
        let src = "export function Login() {\n    return <div />;\n}\n\nit('renders login', () => {\n    render(<Login />);\n});\n";
        let result = parse(src).unwrap();

        let refs_from_tests: Vec<&Reference> = result
            .references
            .iter()
            .filter(|r| {
                r.kind == RefKind::Call
                    && r.source_symbol_name
                        .as_deref()
                        .is_some_and(|name| name.starts_with("__ts_test_"))
            })
            .collect();

        assert!(refs_from_tests.iter().any(|r| r.target_name == "render"));
        assert!(refs_from_tests.iter().any(|r| r.target_name == "Login"));
    }
}
