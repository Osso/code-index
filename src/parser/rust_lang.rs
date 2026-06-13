use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{Import, ParseResult, RefKind, Reference, Symbol, SymbolKind};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

mod macro_calls;
use macro_calls::{scan_bare_function_refs, scan_macro_calls};

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
    function: (scoped_identifier) @scoped_function
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
    parser
        .set_language(&lang)
        .context("Failed to set Rust language")?;

    let tree = parser
        .parse(source, None)
        .context("Failed to parse Rust source")?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = Vec::new();
    let mut references = Vec::new();
    let mut imports = Vec::new();

    parse_symbols(root, src, &lang, &mut symbols)?;
    parse_calls(root, src, &lang, &symbols, &mut references)?;
    parse_uses(root, src, &lang, &mut imports)?;
    parse_impl_blocks(root, src, &lang, &symbols, &mut references)?;

    Ok(ParseResult {
        symbols,
        references,
        imports,
    })
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

fn make_call_ref(
    name: &str,
    qualifier: Option<String>,
    line: usize,
    source: Option<String>,
) -> Reference {
    Reference {
        kind: RefKind::Call,
        target_name: name.to_string(),
        target_qualifier: qualifier,
        line,
        source_symbol_name: source,
    }
}

#[derive(Clone, Copy)]
struct SymbolCaptureIndices {
    fn_name: u32,
    fn_params: u32,
    fn_node: u32,
    struct_name: u32,
    struct_node: u32,
    enum_name: u32,
    enum_node: u32,
    trait_name: u32,
    trait_node: u32,
}

fn symbol_capture_indices(query: &Query) -> SymbolCaptureIndices {
    SymbolCaptureIndices {
        fn_name: query.capture_index_for_name("fn_name").unwrap(),
        fn_params: query.capture_index_for_name("fn_params").unwrap(),
        fn_node: query.capture_index_for_name("fn_node").unwrap(),
        struct_name: query.capture_index_for_name("struct_name").unwrap(),
        struct_node: query.capture_index_for_name("struct_node").unwrap(),
        enum_name: query.capture_index_for_name("enum_name").unwrap(),
        enum_node: query.capture_index_for_name("enum_node").unwrap(),
        trait_name: query.capture_index_for_name("trait_name").unwrap(),
        trait_node: query.capture_index_for_name("trait_node").unwrap(),
    }
}

fn build_symbol_for_capture<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    capture_index: u32,
    capture_node: tree_sitter::Node<'a>,
    src: &[u8],
    indices: SymbolCaptureIndices,
) -> Option<Symbol> {
    if capture_index == indices.fn_name {
        return Some(build_fn_symbol(
            query_match,
            capture_node,
            src,
            indices.fn_node,
            indices.fn_params,
        ));
    }
    build_type_symbol_for_capture(query_match, capture_index, capture_node, src, indices)
}

fn build_type_symbol_for_capture<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    capture_index: u32,
    capture_node: tree_sitter::Node<'a>,
    src: &[u8],
    indices: SymbolCaptureIndices,
) -> Option<Symbol> {
    let (node_idx, kind) = match capture_index {
        i if i == indices.struct_name => (indices.struct_node, SymbolKind::Struct),
        i if i == indices.enum_name => (indices.enum_node, SymbolKind::Enum),
        i if i == indices.trait_name => (indices.trait_node, SymbolKind::Trait),
        _ => return None,
    };
    Some(build_type_sym(
        query_match,
        capture_node,
        src,
        node_idx,
        kind,
    ))
}

fn parse_symbols(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &mut Vec<Symbol>,
) -> Result<()> {
    let query = Query::new(lang, SYMBOL_QUERY).context("Invalid symbol query")?;
    let indices = symbol_capture_indices(&query);

    for_each_match(&query, root, src, |m, _, _| {
        for cap in m.captures {
            if let Some(symbol) = build_symbol_for_capture(m, cap.index, cap.node, src, indices) {
                symbols.push(symbol);
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

    let is_test = fn_node.map_or(false, |n| has_test_attribute(n, src));
    Symbol {
        name,
        kind: if is_method {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        },
        line_start,
        line_end,
        parent_name,
        visibility: vis,
        signature: Some(format!("fn {}", params)),
        is_test,
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
        is_test: false,
    }
}

/// Check if a function_item node has a #[test] or #[tokio::test] attribute
fn has_test_attribute(node: tree_sitter::Node, src: &[u8]) -> bool {
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() == "attribute_item" {
            let text = node_text(sib, src);
            if text.contains("test") {
                return true;
            }
        }
        // Skip comments between attribute and function
        if sib.kind() != "attribute_item" && sib.kind() != "line_comment" {
            break;
        }
        sibling = sib.prev_sibling();
    }
    false
}

fn parse_calls(
    root: tree_sitter::Node,
    src: &[u8],
    lang: &tree_sitter::Language,
    symbols: &[Symbol],
    references: &mut Vec<Reference>,
) -> Result<()> {
    let query = Query::new(lang, CALL_QUERY).context("Invalid call query")?;
    let indices = call_capture_indices(&query);
    for_each_match(&query, root, src, |query_match, _, _| {
        parse_call_match(query_match, src, symbols, references, indices)
    });
    Ok(())
}

#[derive(Clone, Copy)]
struct CallCaptureIndices {
    call_name: u32,
    method_name: u32,
    method_call_node: u32,
    scoped_function: u32,
    macro_name: u32,
    macro_node: u32,
}

fn call_capture_indices(query: &Query) -> CallCaptureIndices {
    CallCaptureIndices {
        call_name: query.capture_index_for_name("call_name").unwrap(),
        method_name: query.capture_index_for_name("method_name").unwrap(),
        method_call_node: query.capture_index_for_name("method_call_node").unwrap(),
        scoped_function: query.capture_index_for_name("scoped_function").unwrap(),
        macro_name: query.capture_index_for_name("macro_name").unwrap(),
        macro_node: query.capture_index_for_name("macro_node").unwrap(),
    }
}

fn parse_call_match(
    query_match: &tree_sitter::QueryMatch,
    src: &[u8],
    symbols: &[Symbol],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) {
    for capture in query_match.captures {
        let call_capture = make_call_capture(capture, symbols);
        if handle_non_macro_call_capture(query_match, &call_capture, src, references, indices) {
            continue;
        }
        handle_macro_call_capture(query_match, &call_capture, src, references, indices);
    }
}

fn handle_non_macro_call_capture(
    query_match: &tree_sitter::QueryMatch,
    capture: &CallCapture<'_>,
    src: &[u8],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) -> bool {
    handle_direct_call_capture(capture, src, references, indices)
        || handle_method_call_capture(query_match, capture, src, references, indices)
        || handle_scoped_call_capture(capture, src, references, indices)
}

#[derive(Clone)]
struct CallCapture<'a> {
    index: u32,
    node: tree_sitter::Node<'a>,
    line: usize,
    source_symbol_name: Option<String>,
}

fn make_call_capture<'a>(
    capture: &tree_sitter::QueryCapture<'a>,
    symbols: &[Symbol],
) -> CallCapture<'a> {
    let line = capture.node.start_position().row;
    let source_symbol_name = find_enclosing_symbol(symbols, line);
    CallCapture {
        index: capture.index,
        node: capture.node,
        line,
        source_symbol_name,
    }
}

fn handle_macro_call_capture(
    query_match: &tree_sitter::QueryMatch,
    capture: &CallCapture<'_>,
    src: &[u8],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) {
    if capture.index != indices.macro_name {
        return;
    }
    push_macro_refs(
        query_match,
        src,
        references,
        indices.macro_name,
        indices.macro_node,
        capture.source_symbol_name.clone(),
    );
}

fn handle_direct_call_capture(
    capture: &CallCapture<'_>,
    src: &[u8],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) -> bool {
    if capture.index != indices.call_name {
        return false;
    }
    references.push(make_call_ref(
        node_text(capture.node, src),
        None,
        capture.line,
        capture.source_symbol_name.clone(),
    ));
    true
}

fn handle_method_call_capture(
    query_match: &tree_sitter::QueryMatch,
    capture: &CallCapture<'_>,
    src: &[u8],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) -> bool {
    if capture.index != indices.method_name {
        return false;
    }
    let method_name = node_text(capture.node, src);
    references.push(make_call_ref(
        method_name,
        None,
        capture.line,
        capture.source_symbol_name.clone(),
    ));
    references.extend(extract_registration_function_refs(
        query_match,
        method_name,
        indices.method_call_node,
        src,
        capture.source_symbol_name.clone(),
    ));
    true
}

fn handle_scoped_call_capture(
    capture: &CallCapture<'_>,
    src: &[u8],
    references: &mut Vec<Reference>,
    indices: CallCaptureIndices,
) -> bool {
    if capture.index != indices.scoped_function {
        return false;
    }
    let (name, qualifier) = split_scoped_call(node_text(capture.node, src));
    references.push(make_call_ref(
        name,
        qualifier,
        capture.line,
        capture.source_symbol_name.clone(),
    ));
    true
}

fn extract_registration_function_refs(
    query_match: &tree_sitter::QueryMatch,
    method_name: &str,
    method_call_node_idx: u32,
    src: &[u8],
    source_symbol_name: Option<String>,
) -> Vec<Reference> {
    let Some(arg_node) = registration_argument_node(query_match, method_name, method_call_node_idx)
    else {
        return Vec::new();
    };
    let base_line = arg_node.start_position().row;
    scan_bare_function_refs(node_text(arg_node, src))
        .into_iter()
        .map(|call| {
            make_call_ref(
                &call.name,
                call.qualifier,
                base_line + call.line_offset,
                source_symbol_name.clone(),
            )
        })
        .collect()
}

fn registration_argument_node<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    method_name: &str,
    method_call_node_idx: u32,
) -> Option<tree_sitter::Node<'a>> {
    let call_node = capture_node_by_idx(query_match, method_call_node_idx)?;
    let args_node = (0..call_node.child_count())
        .filter_map(|index| call_node.child(index as u32))
        .find(|child| child.kind() == "arguments")?;

    let arg_index = match method_name {
        "add_systems" => 1,
        "add_observer" | "observe" => 0,
        _ => return None,
    };
    args_node.named_child(arg_index)
}

fn split_scoped_call(path: &str) -> (&str, Option<String>) {
    match path.rsplit_once("::") {
        Some((qualifier, name)) => (name, Some(qualifier.to_string())),
        None => (path, None),
    }
}

fn extract_macro_body_calls(
    macro_node: tree_sitter::Node,
    src: &[u8],
    source_symbol_name: Option<String>,
) -> Vec<Reference> {
    let Some(token_tree) = find_macro_token_tree(macro_node) else {
        return Vec::new();
    };
    let macro_body = node_text(token_tree, src);
    let base_line = token_tree.start_position().row;
    scan_macro_calls(macro_body)
        .into_iter()
        .map(|call| {
            make_call_ref(
                &call.name,
                call.qualifier,
                base_line + call.line_offset,
                source_symbol_name.clone(),
            )
        })
        .collect()
}

struct MacroCallContext<'a> {
    macro_node: tree_sitter::Node<'a>,
    line: usize,
    target_name: String,
}

fn resolve_macro_call_context<'a>(
    query_match: &tree_sitter::QueryMatch<'_, 'a>,
    src: &[u8],
    macro_name_idx: u32,
    macro_node_idx: u32,
) -> Option<MacroCallContext<'a>> {
    let macro_name = capture_text_by_idx(query_match, macro_name_idx, src)?;
    let macro_node = capture_node_by_idx(query_match, macro_node_idx)?;
    Some(MacroCallContext {
        line: macro_node.start_position().row,
        target_name: format!("{macro_name}!"),
        macro_node,
    })
}

fn push_macro_invocation_ref(
    references: &mut Vec<Reference>,
    context: &MacroCallContext<'_>,
    source_symbol_name: Option<String>,
) {
    references.push(make_call_ref(
        &context.target_name,
        None,
        context.line,
        source_symbol_name,
    ));
}

fn extend_macro_body_refs(
    references: &mut Vec<Reference>,
    context: &MacroCallContext<'_>,
    src: &[u8],
    source_symbol_name: Option<String>,
) {
    references.extend(
        extract_macro_body_calls(context.macro_node, src, source_symbol_name)
            .into_iter()
            .filter(|call| call.target_name != context.target_name),
    );
}

fn push_macro_refs(
    query_match: &tree_sitter::QueryMatch,
    src: &[u8],
    references: &mut Vec<Reference>,
    macro_name_idx: u32,
    macro_node_idx: u32,
    source_symbol_name: Option<String>,
) {
    let Some(context) =
        resolve_macro_call_context(query_match, src, macro_name_idx, macro_node_idx)
    else {
        return;
    };

    push_macro_invocation_ref(references, &context, source_symbol_name.clone());
    extend_macro_body_refs(references, &context, src, source_symbol_name);
}

fn find_macro_token_tree(macro_node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    (0..macro_node.child_count())
        .filter_map(|index| macro_node.child(index as u32))
        .find(|child| child.kind() == "token_tree")
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
        let trait_name: Option<&str> =
            impl_trait_idx.and_then(|idx| capture_text_by_idx(m, idx, src));

        if let (Some(tn), Some(tr)) = (type_name, trait_name) {
            let line = capture_node_by_idx(m, impl_node_idx)
                .unwrap()
                .start_position()
                .row;
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
        if let Some(type_name) = parent_impl_type_name(parent, src) {
            return Some(type_name);
        }
        current = parent.parent();
    }
    None
}

fn parent_impl_type_name(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    for i in 0..node.child_count() {
        let Some(child) = node.child(i as u32) else {
            continue;
        };
        if child.kind() == "type_identifier" {
            return Some(node_text(child, src).to_string());
        }
    }
    None
}

fn extract_visibility(node: tree_sitter::Node, src: &[u8]) -> Option<String> {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
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
mod tests;
