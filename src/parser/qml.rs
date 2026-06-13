use anyhow::Result;
use ast_outline::core::{Declaration, DeclarationKind};

use crate::model::{ParseResult, Symbol, SymbolKind};

pub fn parse(source: &str) -> Result<ParseResult> {
    let outline = ast_outline::adapters::qml::parse_qml(
        std::path::Path::new("<memory>.qml"),
        source.as_bytes(),
    );
    let mut symbols = Vec::new();

    for declaration in &outline.declarations {
        collect_symbols(declaration, None, &mut symbols);
    }

    Ok(ParseResult {
        symbols,
        references: Vec::new(),
        imports: Vec::new(),
    })
}

fn collect_symbols(
    declaration: &Declaration,
    parent_name: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let symbol_kind = symbol_kind_for(declaration.kind);
    if let Some(kind) = symbol_kind {
        symbols.push(Symbol {
            name: declaration.name.clone(),
            kind,
            line_start: declaration.start_line,
            line_end: declaration.end_line,
            parent_name: parent_name.map(str::to_owned),
            visibility: None,
            signature: Some(declaration.signature.clone()),
            is_test: false,
        });
    }

    let next_parent = symbol_kind
        .filter(|kind| {
            matches!(
                kind,
                SymbolKind::Class | SymbolKind::Method | SymbolKind::Function
            )
        })
        .map(|_| declaration.name.as_str())
        .or(parent_name);

    for child in &declaration.children {
        collect_symbols(child, next_parent, symbols);
    }
}

fn symbol_kind_for(kind: DeclarationKind) -> Option<SymbolKind> {
    match kind {
        DeclarationKind::Class => Some(SymbolKind::Class),
        DeclarationKind::Function => Some(SymbolKind::Function),
        DeclarationKind::Method => Some(SymbolKind::Method),
        DeclarationKind::Property | DeclarationKind::Field => Some(SymbolKind::Property),
        DeclarationKind::Event => Some(SymbolKind::Event),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qml_symbols() {
        let parsed = parse(
            r#"
import QtQuick

Rectangle {
  id: root
  property string title: ""
  signal activated(int reason)

  function activate(reason) {
    return reason
  }
}
"#,
        )
        .unwrap();

        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Class && symbol.name == "Rectangle")
        );
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Property && symbol.name == "title")
        );
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Event && symbol.name == "activated")
        );
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Function && symbol.name == "activate")
        );
    }
}
