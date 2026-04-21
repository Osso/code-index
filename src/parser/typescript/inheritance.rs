use anyhow::Result;

use crate::model::{RefKind, Reference};
use crate::parser::node_text;

pub(super) fn parse_inheritance(
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

pub(super) fn find_parent_class_ts<'a>(
    node: tree_sitter::Node<'a>,
    src: &'a [u8],
) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "class_declaration" || parent.kind() == "abstract_class_declaration" {
            return find_class_name(parent, src);
        }
        current = parent.parent();
    }
    None
}
