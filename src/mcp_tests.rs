use super::*;
use crate::db::Database;
use crate::model::{
    CallInfo, HierarchyEntry, Import, ImportedByEntry, RefKind, Reference, ResolvedImport, Symbol,
    SymbolKind,
};
use crate::model::{StoredReference, StoredSymbol};
use crate::test_support::CWD_LOCK;

fn stored_symbol(name: &str) -> StoredSymbol {
    StoredSymbol {
        id: 1,
        file_path: "/repo/src/lib.rs".to_string(),
        name: name.to_string(),
        kind: "function".to_string(),
        line_start: 10,
        line_end: 12,
        visibility: Some("pub".to_string()),
        signature: Some("fn helper()".to_string()),
    }
}

fn insert_symbol(
    db: &Database,
    file_id: i64,
    name: &str,
    kind: SymbolKind,
    is_test: bool,
    parent_id: Option<i64>,
) -> i64 {
    db.insert_symbol(
        file_id,
        &Symbol {
            name: name.to_string(),
            kind,
            line_start: 10,
            line_end: 12,
            parent_name: None,
            visibility: Some("pub".to_string()),
            signature: None,
            is_test,
        },
        parent_id,
    )
    .unwrap()
}

fn insert_ref(
    db: &Database,
    file_id: i64,
    source_symbol_id: Option<i64>,
    kind: RefKind,
    target_name: &str,
) -> i64 {
    db.insert_ref(
        file_id,
        &Reference {
            kind,
            target_name: target_name.to_string(),
            target_qualifier: None,
            line: 14,
            source_symbol_name: None,
        },
        source_symbol_id,
    )
    .unwrap()
}

#[test]
fn service_info_advertises_tool_capabilities() {
    let service = CodeIndexService::new();

    let info = service.get_info();

    assert!(info.capabilities.tools.is_some());
    assert!(
        info.instructions
            .unwrap()
            .contains("Structural code analysis")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tool_methods_query_current_project_database() {
    let _guard = CWD_LOCK.lock().unwrap();
    let tmp = tempfile::TempDir::new().unwrap();
    let old_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    seed_project_database(&tmp);

    assert_tool_outputs(CodeIndexService::new()).await;

    std::env::set_current_dir(old_cwd).unwrap();
}

fn seed_project_database(tmp: &tempfile::TempDir) {
    let db = Database::open(&tmp.path().join(".code-index.db").to_string_lossy()).unwrap();
    let lib_file = db.upsert_file("/repo/src/lib.rs", "lib", "rust").unwrap();
    let test_file = db
        .upsert_file("/repo/tests/lib_test.rs", "test", "rust")
        .unwrap();
    let widget = insert_symbol(&db, lib_file, "Widget", SymbolKind::Struct, false, None);
    let helper = insert_symbol(&db, lib_file, "helper", SymbolKind::Function, false, None);
    let caller = insert_symbol(&db, lib_file, "caller", SymbolKind::Function, false, None);
    let test = insert_symbol(
        &db,
        test_file,
        "test_helper",
        SymbolKind::Function,
        true,
        None,
    );
    let call_ref = insert_ref(&db, lib_file, Some(caller), RefKind::Call, "helper");
    insert_ref(&db, test_file, Some(test), RefKind::Call, "helper");
    insert_ref(&db, lib_file, Some(widget), RefKind::Inherit, "BaseWidget");
    db.resolve_ref(call_ref, helper).unwrap();
    db.insert_import(
        lib_file,
        &Import {
            local_name: "Widget".to_string(),
            full_path: "crate::widgets::Widget".to_string(),
            alias: None,
            line: 3,
        },
    )
    .unwrap();
}

async fn assert_tool_outputs(service: CodeIndexService) {
    assert!(
        service
            .symbol(Parameters(SymbolParams {
                name: "helper".to_string(),
                kind: Some("function".to_string()),
                file: Some("lib.rs".to_string()),
            }))
            .await
            .contains("function helper")
    );
    assert!(
        service
            .callers(Parameters(CallersParams {
                name: "helper".to_string(),
                file: None,
                depth: Some(1),
            }))
            .await
            .contains("caller in /repo/src/lib.rs")
    );
    assert!(
        service
            .callees(Parameters(CalleesParams {
                name: "caller".to_string(),
                file: None,
                depth: Some(1),
            }))
            .await
            .contains("helper in /repo/src/lib.rs")
    );
    assert_remaining_tool_outputs(service).await;
}

async fn assert_remaining_tool_outputs(service: CodeIndexService) {
    assert!(
        service
            .references(Parameters(ReferencesParams {
                name: "helper".to_string(),
                kind: Some("call".to_string()),
            }))
            .await
            .contains("[call] caller")
    );
    assert!(
        service
            .tested_by(Parameters(TestedByParams {
                name: "helper".to_string(),
                file: None,
                depth: Some(1),
            }))
            .await
            .contains("test_helper")
    );
    assert!(
        service
            .untested(Parameters(UntestedParams {
                path: Some("src".to_string()),
                exclude: Some(vec!["caller".to_string()]),
            }))
            .await
            .contains("All functions/methods are tested")
    );
    assert_lookup_tool_outputs(service).await;
}

async fn assert_lookup_tool_outputs(service: CodeIndexService) {
    assert_import_tool_outputs(service.clone()).await;
    assert_list_and_dead_code_outputs(service.clone()).await;
    assert_hierarchy_output(service).await;
}

async fn assert_import_tool_outputs(service: CodeIndexService) {
    assert!(
        service
            .resolve_import(Parameters(ResolveImportParams {
                name: "Widget".to_string(),
                file: Some("lib.rs".to_string()),
            }))
            .await
            .contains("struct Widget")
    );
    assert!(
        service
            .imported_by(Parameters(ImportedByParams {
                name: "widgets".to_string(),
                file: Some("lib.rs".to_string()),
            }))
            .await
            .contains("Widget (crate::widgets::Widget)")
    );
}

async fn assert_list_and_dead_code_outputs(service: CodeIndexService) {
    assert!(
        service
            .list(Parameters(ListParams {
                kind: Some("function".to_string()),
                file: Some("lib.rs".to_string()),
            }))
            .await
            .contains("\"name\":\"helper\"")
    );
    assert!(
        service
            .dead_code(Parameters(DeadCodeParams {
                path: Some("src".to_string()),
                exclude: Some(vec!["caller".to_string()]),
            }))
            .await
            .contains("No dead code found")
    );
}

async fn assert_hierarchy_output(service: CodeIndexService) {
    assert!(
        service
            .hierarchy(Parameters(HierarchyParams {
                name: "BaseWidget".to_string(),
                direction: Some("descendants".to_string()),
            }))
            .await
            .contains("Widget")
    );
}

#[test]
fn formatters_return_empty_messages() {
    assert_eq!(format_symbols(&[]), "No symbols found.");
    assert_eq!(format_call_infos(&[], "callers"), "No callers found.");
    assert_eq!(format_references(&[]), "No references found.");
    assert_eq!(format_hierarchy(&[]), "No inheritance found.");
    assert_eq!(format_resolved_imports(&[]), "No imports found.");
    assert_eq!(format_tested_by(&[]), "No tests found for this symbol.");
    assert_eq!(format_untested(&[]), "All functions/methods are tested.");
    assert_eq!(format_imported_by(&[]), "No importers found.");
    assert_eq!(format_dead_code(&[]), "No dead code found.");
}

#[test]
fn formatters_render_non_empty_results() {
    let symbol = stored_symbol("helper");
    assert_eq!(
        format_symbols(std::slice::from_ref(&symbol)),
        "pub function helper /repo/src/lib.rs:10-12 | fn helper()"
    );
    assert_eq!(
        format_call_infos(
            &[CallInfo {
                symbol_name: "caller".to_string(),
                file_path: "/repo/src/lib.rs".to_string(),
                line: 20,
                kind: "call".to_string(),
            }],
            "callers",
        ),
        "1 callers found:\n  caller in /repo/src/lib.rs:20"
    );
    assert_eq!(
        format_references(&[StoredReference {
            source_file: "/repo/src/lib.rs".to_string(),
            source_symbol: Some("caller".to_string()),
            target_name: "helper".to_string(),
            target_qualifier: None,
            kind: "call".to_string(),
            line: 20,
            resolved: true,
            target_file: Some("/repo/src/lib.rs".to_string()),
            target_symbol: Some("helper".to_string()),
        }]),
        "  [call] caller in /repo/src/lib.rs:20 [resolved]"
    );
    assert_eq!(
        format_hierarchy(&[HierarchyEntry {
            name: "Child".to_string(),
            kind: "inherit".to_string(),
            file_path: "/repo/src/child.rs".to_string(),
            relation: "descendant".to_string(),
            depth: 1,
        }]),
        "  Child (inherit) in /repo/src/child.rs | descendant"
    );
    assert_eq!(
        format_tested_by(std::slice::from_ref(&symbol)),
        "1 test(s) found:\n  function helper in /repo/src/lib.rs:10"
    );
    assert_eq!(
        format_untested(std::slice::from_ref(&symbol)),
        "1 untested functions/methods:\n  function helper in /repo/src/lib.rs:10"
    );
    assert_eq!(
        format_dead_code(&[symbol]),
        "1 potentially unused functions:\n  function helper in /repo/src/lib.rs:10"
    );
}

#[test]
fn import_formatters_render_alias_resolution_and_reverse_imports() {
    assert_eq!(
        format_resolved_imports(&[
            ResolvedImport {
                source_file: "/repo/src/app.rs".to_string(),
                local_name: "LocalWidget".to_string(),
                full_path: "crate::widgets::Widget".to_string(),
                alias: Some("LocalWidget".to_string()),
                line: 4,
                target_file: Some("/repo/src/widgets/widget.rs".to_string()),
                target_symbol: Some("Widget".to_string()),
                target_kind: Some("struct".to_string()),
                target_line: Some(1),
            },
            ResolvedImport {
                source_file: "/repo/src/app.rs".to_string(),
                local_name: "Missing".to_string(),
                full_path: "crate::missing".to_string(),
                alias: None,
                line: 5,
                target_file: None,
                target_symbol: None,
                target_kind: None,
                target_line: None,
            },
        ]),
        "/repo/src/app.rs:4 LocalWidget (crate::widgets::Widget) as LocalWidget → struct Widget in /repo/src/widgets/widget.rs:1\n/repo/src/app.rs:5 Missing (crate::missing) → [unresolved]"
    );
    assert_eq!(
        format_imported_by(&[ImportedByEntry {
            file_path: "/repo/src/app.rs".to_string(),
            local_name: "Widget".to_string(),
            full_path: "crate::widgets::Widget".to_string(),
            alias: Some("LocalWidget".to_string()),
            line: 4,
        }]),
        "1 importer(s) found:\n  /repo/src/app.rs:4 Widget (crate::widgets::Widget) as LocalWidget"
    );
}
