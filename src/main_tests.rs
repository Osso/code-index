use super::*;
use crate::model::{CallInfo, StoredSymbol};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

fn acquire_database_lock(database_path: &str, purpose: &str) -> File {
    let lock_path = db::database_lock_path(database_path, purpose).unwrap();
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .unwrap();
    file.lock().unwrap();
    file
}

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
fn database_open_waits_for_active_migration() {
    let tmp = tempfile::TempDir::new().unwrap();
    let database_path = project::db_path(tmp.path());
    drop(db::Database::open(&database_path).unwrap());

    let migration_lock = acquire_database_lock(&database_path, "migration");
    let (started_tx, started_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let query_path = database_path.clone();
    let open_thread = thread::spawn(move || {
        started_tx.send(()).unwrap();
        result_tx.send(db::Database::open(&query_path)).unwrap();
    });

    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let early_result = result_rx.recv_timeout(Duration::from_millis(500));
    migration_lock.unlock().unwrap();
    let open_result = match early_result {
        Ok(_) => panic!("database opened while another migration owned the startup lock"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            result_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        }
        Err(error) => panic!("migration result channel failed: {error}"),
    };
    open_thread.join().unwrap();

    open_result.unwrap();
}

#[test]
fn open_refreshed_database_reads_existing_index_while_another_refresh_is_active() {
    let tmp = tempfile::TempDir::new().unwrap();
    let source_path = tmp.path().join("lib.rs");
    std::fs::write(&source_path, "fn indexed_symbol() {}\n").unwrap();
    let project_path = tmp.path().to_str().unwrap().to_string();

    let _ = open_refreshed_database(Some(&project_path)).unwrap();
    std::fs::write(&source_path, "fn replacement_symbol() {}\n").unwrap();

    let database_path = project::db_path(tmp.path());
    let db = db::Database::open(&database_path).unwrap();
    db.set_meta(LAST_REFRESH_KEY, "0").unwrap();
    drop(db);

    let refresh_lock = acquire_database_lock(&database_path, "refresh");
    let writer = rusqlite::Connection::open(&database_path).unwrap();
    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO meta (key, value) VALUES ('active_refresh', 'held');",
        )
        .unwrap();

    let (result_tx, result_rx) = mpsc::channel();
    let query_thread = thread::spawn(move || {
        let result = open_refreshed_database(Some(&project_path))
            .and_then(|(_, db)| query::find_symbols(&db, "indexed_symbol", None, None))
            .map(|symbols| {
                symbols
                    .into_iter()
                    .map(|symbol| symbol.name)
                    .collect::<Vec<_>>()
            })
            .map_err(|error| format!("{error:#}"));
        result_tx.send(result).unwrap();
    });

    let result = result_rx.recv_timeout(Duration::from_secs(3));
    writer.execute_batch("ROLLBACK").unwrap();
    refresh_lock.unlock().unwrap();
    query_thread.join().unwrap();

    let symbols = result.expect("read query joined the active refresh instead of using the index");
    assert_eq!(symbols.unwrap(), vec!["indexed_symbol"]);
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
