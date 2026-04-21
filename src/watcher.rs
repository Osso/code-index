use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use notify_debouncer_mini::notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, new_debouncer};

use crate::db::Database;
use crate::indexer;
use crate::model::Language;
use crate::resolver;

/// Watch a directory for file changes and re-index affected files.
pub fn watch(db_path: &str, dir: &str) -> Result<()> {
    let dir = Path::new(dir)
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {dir}"))?;

    // Initial full index
    {
        let db = Database::open(db_path)?;
        let stats = indexer::index_directory(&db, dir.to_str().unwrap(), false)?;
        log::info!("{stats}");
        let resolved = resolver::resolve_references(&db)?;
        log::info!("{resolved}");
    }

    let (tx, rx) = mpsc::channel::<DebounceEventResult>();

    let mut debouncer =
        new_debouncer(Duration::from_secs(1), tx).context("Failed to create file watcher")?;

    debouncer
        .watcher()
        .watch(&dir, RecursiveMode::Recursive)
        .with_context(|| format!("Failed to watch {}", dir.display()))?;

    log::info!("Watching {} for changes...", dir.display());

    for result in rx {
        match result {
            Ok(events) => {
                let changed = collect_changed_files(&events);
                if !changed.is_empty() {
                    handle_changes(db_path, &changed);
                }
            }
            Err(e) => log::error!("Watch error: {e:?}"),
        }
    }

    Ok(())
}

fn collect_changed_files(events: &[notify_debouncer_mini::DebouncedEvent]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for event in events {
        let path = &event.path;
        if !path.is_file() {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if Language::from_extension(ext).is_none() {
            continue;
        }
        if seen.insert(path.clone()) {
            files.push(path.clone());
        }
    }
    files
}

fn handle_changes(db_path: &str, files: &[PathBuf]) {
    let db = match Database::open(db_path) {
        Ok(db) => db,
        Err(e) => {
            log::error!("Failed to open database: {e}");
            return;
        }
    };

    let mut indexed = 0usize;
    for path in files {
        match handle_changed_file(&db, path) {
            Ok(true) => indexed += 1,
            Ok(false) => {}
            Err(e) => log::error!("Error indexing {}: {e}", path.display()),
        }
    }

    if indexed > 0 {
        match resolver::resolve_references(&db) {
            Ok(r) => log::info!("{r}"),
            Err(e) => log::error!("Resolution error: {e}"),
        }
    }
}

fn handle_changed_file(db: &Database, path: &Path) -> Result<bool> {
    let Some(path_str) = path.to_str() else {
        return Ok(false);
    };
    if !path.exists() {
        log::info!("Removed: {path_str}");
        return Ok(false);
    }
    let Some(lang) = changed_file_language(path) else {
        return Ok(false);
    };
    let changed = reindex_file(db, path, lang)?;
    if changed {
        log::info!("Re-indexed: {path_str}");
    }
    Ok(changed)
}

fn changed_file_language(path: &Path) -> Option<Language> {
    let ext = path.extension().and_then(|e| e.to_str())?;
    Language::from_extension(ext)
}

fn reindex_file(db: &Database, path: &Path, lang: Language) -> Result<bool> {
    let path_str = path.to_str().context("Non-UTF8 path")?;
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {path_str}"))?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    if let Some(existing) = db.get_file_hash(path_str)? {
        if existing == hash {
            return Ok(false);
        }
    }

    let file_id = db.upsert_file(path_str, &hash, lang.as_str())?;
    db.clear_file_data(file_id)?;

    let result = crate::parser::parse_file(&source, lang)?;
    indexer::store_parse_result(db, file_id, &result)?;

    Ok(true)
}
