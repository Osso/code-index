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
    let call_names: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(call_names.contains(&"foo"));
    assert!(call_names.contains(&"baz"));
    assert!(call_names.contains(&"println!"));
}

#[test]
fn test_parse_scoped_call_uses_full_qualifier() {
    let src = "fn main() {\n    crate::combat::resolve_hit();\n}\n";
    let result = parse(src).unwrap();
    let call = result
        .references
        .iter()
        .find(|reference| reference.target_name == "resolve_hit")
        .unwrap();
    assert_eq!(call.target_qualifier.as_deref(), Some("crate::combat"));
}

#[test]
fn test_parse_macro_body_calls() {
    let src = "fn main() {\n    assert_eq!(melee::resolve_melee_outcome(&c, 0), MeleeOutcome::Hit);\n    assert!(leash.should_evade(target));\n}\n";
    let result = parse(src).unwrap();
    let call_names: Vec<&str> = result
        .references
        .iter()
        .map(|reference| reference.target_name.as_str())
        .collect();
    assert!(call_names.contains(&"assert_eq!"));
    assert!(call_names.contains(&"assert!"));
    assert!(call_names.contains(&"resolve_melee_outcome"));
    assert!(call_names.contains(&"should_evade"));

    let scoped_call = result
        .references
        .iter()
        .find(|reference| reference.target_name == "resolve_melee_outcome")
        .unwrap();
    assert_eq!(scoped_call.target_qualifier.as_deref(), Some("melee"));
}

#[test]
fn test_parse_bevy_system_registration_refs() {
    let src = r#"
fn plugin(app: &mut App) {
    app.add_systems(Update, (process_login_requests, auth::process_register_requests));
}

fn process_login_requests() {}

mod auth {
    pub fn process_register_requests() {}
}
"#;
    let result = parse(src).unwrap();
    let call_names: Vec<&str> = result
        .references
        .iter()
        .map(|reference| reference.target_name.as_str())
        .collect();

    assert!(call_names.contains(&"add_systems"));
    assert!(call_names.contains(&"process_login_requests"));
    assert!(call_names.contains(&"process_register_requests"));

    let scoped_ref = result
        .references
        .iter()
        .find(|reference| reference.target_name == "process_register_requests")
        .unwrap();
    assert_eq!(scoped_ref.target_qualifier.as_deref(), Some("auth"));
}
