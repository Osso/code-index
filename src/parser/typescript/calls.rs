use anyhow::{Context, Result};
use tree_sitter::Query;

use crate::model::{RefKind, Reference, Symbol};
use crate::parser::{find_enclosing_symbol, for_each_match, node_text};

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

pub(super) fn parse_calls(
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
