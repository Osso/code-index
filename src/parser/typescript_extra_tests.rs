use super::*;
use crate::model::{RefKind, SymbolKind};

#[test]
fn parses_static_async_methods_and_nested_calls() {
    let src = r#"
class Queue {
    static async drain(items: Item[]) {
        await Promise.all(items.map(item => processItem(item)));
    }
}
"#;
    let result = parse(src).unwrap();

    assert!(result.symbols.iter().any(|s| {
        s.name == "drain"
            && s.kind == SymbolKind::Method
            && s.parent_name.as_deref() == Some("Queue")
    }));
    assert!(
        result
            .references
            .iter()
            .any(|r| r.kind == RefKind::Call && r.target_name == "all")
    );
    assert!(
        result
            .references
            .iter()
            .any(|r| r.kind == RefKind::Call && r.target_name == "processItem")
    );
}

#[test]
fn parses_arrow_with_nested_calls() {
    let result = parse("const normalize = (value) => clean(value.trim());").unwrap();

    assert!(result.symbols.iter().any(|s| s.name == "normalize"));
    assert!(result.references.iter().any(|r| r.target_name == "clean"));
    assert!(result.references.iter().any(|r| r.target_name == "trim"));
}

#[test]
fn parses_function_expression_symbol() {
    let src = "const handler = function (event: Event) { return build(event); };";
    let result = parse(src).unwrap();

    assert!(result.symbols.iter().any(|s| s.name == "handler"));
    assert!(result.references.iter().any(|r| r.target_name == "build"));
}

#[test]
fn parses_exported_class_and_constructor_call() {
    let result = parse("export class View {}\nconst view = new View();").unwrap();

    assert!(result.symbols.iter().any(|s| s.name == "View"));
    assert!(result.references.iter().any(|r| r.target_name == "View"));
}
