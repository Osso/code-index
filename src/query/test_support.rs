use crate::db::Database;
use crate::model::{Import, RefKind, Reference, Symbol, SymbolKind};

pub(super) fn test_db() -> Database {
    Database::open_in_memory().unwrap()
}

pub(super) fn file(db: &Database, path: &str) -> i64 {
    db.upsert_file(path, "hash", "rust").unwrap()
}

pub(super) fn symbol(
    db: &Database,
    file_id: i64,
    name: &str,
    kind: SymbolKind,
    line: usize,
    parent_id: Option<i64>,
) -> i64 {
    db.insert_symbol(
        file_id,
        &Symbol {
            name: name.to_string(),
            kind,
            line_start: line,
            line_end: line + 2,
            parent_name: None,
            visibility: Some("pub".to_string()),
            signature: None,
            is_test: false,
        },
        parent_id,
    )
    .unwrap()
}

pub(super) fn call_ref(
    db: &Database,
    file_id: i64,
    source_symbol_id: Option<i64>,
    target_name: &str,
    target_qualifier: Option<&str>,
    line: usize,
) -> i64 {
    reference(
        db,
        file_id,
        source_symbol_id,
        RefKind::Call,
        target_name,
        target_qualifier,
        line,
    )
}

pub(super) fn reference(
    db: &Database,
    file_id: i64,
    source_symbol_id: Option<i64>,
    kind: RefKind,
    target_name: &str,
    target_qualifier: Option<&str>,
    line: usize,
) -> i64 {
    db.insert_ref(
        file_id,
        &Reference {
            kind,
            target_name: target_name.to_string(),
            target_qualifier: target_qualifier.map(str::to_string),
            line,
            source_symbol_name: None,
        },
        source_symbol_id,
    )
    .unwrap()
}

pub(super) fn import(
    db: &Database,
    file_id: i64,
    local_name: &str,
    full_path: &str,
    alias: Option<&str>,
    line: usize,
) {
    db.insert_import(
        file_id,
        &Import {
            local_name: local_name.to_string(),
            full_path: full_path.to_string(),
            alias: alias.map(str::to_string),
            line,
        },
    )
    .unwrap();
}
