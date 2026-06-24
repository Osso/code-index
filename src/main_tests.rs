use super::*;
use crate::model::{CallInfo, StoredSymbol};
use std::path::Path;

#[test]
fn outline_file_args_include_definition_and_unique_callers() {
    let definitions = vec![StoredSymbol {
        id: 1,
        file_path: "src/base.php".to_string(),
        name: "blockedReleaseResponse".to_string(),
        kind: "method".to_string(),
        line_start: 10,
        line_end: 20,
        visibility: None,
        signature: None,
    }];
    let callers = vec![
        CallInfo {
            symbol_name: "handle_pages".to_string(),
            file_path: "src/releases.php".to_string(),
            line: 30,
            kind: "call".to_string(),
        },
        CallInfo {
            symbol_name: "handle_fragments".to_string(),
            file_path: "src/releases.php".to_string(),
            line: 40,
            kind: "call".to_string(),
        },
    ];

    let files = build_outline_file_args(Path::new("/repo"), &definitions, &callers);

    assert_eq!(files, vec!["/repo/src/base.php", "/repo/src/releases.php"]);
}

#[test]
fn open_refreshed_database_prunes_missing_files_before_queries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let missing_file = tmp.path().join("missing.rs");
    let db = db::Database::open(&project::db_path(tmp.path())).unwrap();
    db.upsert_file(missing_file.to_str().unwrap(), "stale", "rust")
        .unwrap();

    let (_project_dir, db) = open_refreshed_database(Some(tmp.path().to_str().unwrap())).unwrap();

    let (files, symbols, refs) = db.get_stats().unwrap();
    assert_eq!((files, symbols, refs), (0, 0, 0));
}

#[test]
fn open_refreshed_database_creates_missing_index_before_queries() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn indexed_symbol() {}\n").unwrap();

    let (_project_dir, db) = open_refreshed_database(Some(tmp.path().to_str().unwrap())).unwrap();

    let symbols = query::find_symbols(&db, "indexed_symbol", None, None).unwrap();
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "indexed_symbol");
    assert!(tmp.path().join(".code-index.db").exists());
}

#[test]
fn refresh_due_when_never_refreshed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db::Database::open(&project::db_path(tmp.path())).unwrap();
    assert!(refresh_due(&db, 10_000).unwrap());
}

#[test]
fn refresh_not_due_within_interval() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db::Database::open(&project::db_path(tmp.path())).unwrap();
    db.set_meta(LAST_REFRESH_KEY, "10000").unwrap();
    assert!(!refresh_due(&db, 10_000 + REFRESH_INTERVAL_SECS - 1).unwrap());
}

#[test]
fn refresh_due_after_interval_elapses() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db = db::Database::open(&project::db_path(tmp.path())).unwrap();
    db.set_meta(LAST_REFRESH_KEY, "10000").unwrap();
    assert!(refresh_due(&db, 10_000 + REFRESH_INTERVAL_SECS).unwrap());
}

#[test]
fn open_refreshed_database_skips_rescan_within_interval() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(tmp.path().join("lib.rs"), "fn first_symbol() {}\n").unwrap();

    let _ = open_refreshed_database(Some(tmp.path().to_str().unwrap())).unwrap();

    std::fs::write(tmp.path().join("more.rs"), "fn second_symbol() {}\n").unwrap();
    let (_dir, db) = open_refreshed_database(Some(tmp.path().to_str().unwrap())).unwrap();

    let found = query::find_symbols(&db, "second_symbol", None, None).unwrap();
    assert!(
        found.is_empty(),
        "file added within refresh interval should be ignored until the gate elapses"
    );
}
