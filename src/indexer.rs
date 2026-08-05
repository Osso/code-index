use std::path::Path;

use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use ignore::types::TypesBuilder;

use crate::db::{Database, FileState, IndexGuard};
use crate::model::{Language, ParseResult};
use crate::parser;

const MAX_PREPARATION_ATTEMPTS: usize = 3;

struct PreparedIndex {
    files: Vec<PreparedFile>,
    missing_paths: Vec<String>,
    stats: IndexStats,
}

struct PreparedFile {
    path: String,
    size: i64,
    modified_ns: i64,
    update: Option<PreparedFileUpdate>,
}

struct PreparedFileUpdate {
    hash: String,
    language: Language,
    parse_result: Option<ParseResult>,
}

/// Index all supported files under a directory.
/// If `full` is true, re-indexes everything regardless of hash.
pub fn index_directory(db: &Database, dir: &str, full: bool) -> Result<IndexStats> {
    let index_guard = db.acquire_index_guard()?;
    index_directory_with_guard(db, dir, full, &index_guard)
}

pub(crate) fn index_directory_with_guard(
    db: &Database,
    dir: &str,
    full: bool,
    _index_guard: &IndexGuard,
) -> Result<IndexStats> {
    let path = Path::new(dir)
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {dir}"))?;

    for attempt in 1..=MAX_PREPARATION_ATTEMPTS {
        let prepared = prepare_index(db, &path, full)?;
        if prepared_index_is_current(&prepared)? {
            return apply_prepared_index(db, prepared, full);
        }
        if attempt == MAX_PREPARATION_ATTEMPTS {
            bail!("Files changed repeatedly while preparing the index");
        }
    }

    unreachable!()
}

fn prepare_index(db: &Database, path: &Path, full: bool) -> Result<PreparedIndex> {
    let walker = build_walker(path)?;
    let missing_paths = if full {
        Vec::new()
    } else {
        find_missing_file_paths(db)?
    };
    let mut prepared = PreparedIndex {
        files: Vec::new(),
        missing_paths,
        stats: IndexStats::default(),
    };

    for entry in walker {
        let Ok(entry) = entry else {
            continue;
        };
        let Some((file_path, language)) = supported_entry(&entry) else {
            continue;
        };
        let result = prepare_file(db, file_path, language, full);
        record_preparation_result(result, file_path, &mut prepared);
    }

    Ok(prepared)
}

fn prepare_file(
    db: &Database,
    path: &Path,
    language: Language,
    full: bool,
) -> Result<PreparedFile> {
    let path_str = path.to_str().context("Non-UTF8 path")?;
    let (size, modified_ns) = read_file_metadata(path)?;
    let existing = query_existing_file_state(db, path_str, full)?;
    let metadata_matches = existing
        .as_ref()
        .is_some_and(|state| state.size == Some(size) && state.modified_ns == Some(modified_ns));
    let update = if metadata_matches {
        None
    } else {
        Some(prepare_file_update(
            path,
            path_str,
            language,
            existing.as_ref(),
        )?)
    };

    Ok(PreparedFile {
        path: path_str.to_owned(),
        size,
        modified_ns,
        update,
    })
}

fn query_existing_file_state(db: &Database, path: &str, full: bool) -> Result<Option<FileState>> {
    if full {
        Ok(None)
    } else {
        db.query_file_state(path)
    }
}

fn prepare_file_update(
    path: &Path,
    path_str: &str,
    language: Language,
    existing: Option<&FileState>,
) -> Result<PreparedFileUpdate> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {path_str}"))?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let content_matches = existing.is_some_and(|state| state.hash == hash);
    let parse_result = if content_matches {
        None
    } else {
        Some(parser::parse_file(&source, language)?)
    };
    Ok(PreparedFileUpdate {
        hash,
        language,
        parse_result,
    })
}

fn record_preparation_result(
    result: Result<PreparedFile>,
    file_path: &Path,
    prepared: &mut PreparedIndex,
) {
    match result {
        Ok(file) => {
            if file.update.is_none() {
                prepared.stats.skipped += 1;
            }
            prepared.files.push(file);
        }
        Err(error) => {
            eprintln!("Error indexing {}: {error}", file_path.display());
            prepared.stats.errors += 1;
        }
    }
}

fn prepared_index_is_current(prepared: &PreparedIndex) -> Result<bool> {
    for file in &prepared.files {
        let path = Path::new(&file.path);
        let exists = path
            .try_exists()
            .with_context(|| format!("Failed to check {}", path.display()))?;
        if !exists || read_file_metadata(path)? != (file.size, file.modified_ns) {
            return Ok(false);
        }
    }
    for missing_path in &prepared.missing_paths {
        if Path::new(missing_path)
            .try_exists()
            .with_context(|| format!("Failed to check {missing_path}"))?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn apply_prepared_index(db: &Database, prepared: PreparedIndex, full: bool) -> Result<IndexStats> {
    let mut stats = prepared.stats;
    db.begin_transaction()?;
    if full {
        db.reset_index()?;
    } else {
        stats.pruned = delete_missing_files(db, &prepared.missing_paths)?;
    }
    for file in &prepared.files {
        let Some(update) = &file.update else {
            continue;
        };
        let result = apply_prepared_file(db, file, update);
        record_index_result(result, Path::new(&file.path), &mut stats);
    }
    db.commit()?;
    Ok(stats)
}

fn apply_prepared_file(
    db: &Database,
    file: &PreparedFile,
    update: &PreparedFileUpdate,
) -> Result<bool> {
    let file_id = db.upsert_file_with_metadata(
        &file.path,
        &update.hash,
        update.language.as_str(),
        Some(file.size),
        Some(file.modified_ns),
    )?;
    let Some(parse_result) = &update.parse_result else {
        return Ok(false);
    };
    db.clear_file_data(file_id)?;
    store_parse_result(db, file_id, parse_result)?;
    Ok(true)
}

fn find_missing_file_paths(db: &Database) -> Result<Vec<String>> {
    Ok(db
        .list_file_paths()?
        .into_iter()
        .filter(|file_path| !Path::new(file_path).exists())
        .collect())
}

fn delete_missing_files(db: &Database, missing_paths: &[String]) -> Result<usize> {
    for file_path in missing_paths {
        db.delete_file_by_path(file_path)?;
    }
    Ok(missing_paths.len())
}

fn supported_entry(entry: &ignore::DirEntry) -> Option<(&Path, Language)> {
    if !entry.file_type().is_some_and(|ft| ft.is_file()) {
        return None;
    }
    let file_path = entry.path();
    let ext = file_path.extension().and_then(|e| e.to_str())?;
    let language = Language::from_extension(ext)?;
    Some((file_path, language))
}

fn record_index_result(result: Result<bool>, file_path: &Path, stats: &mut IndexStats) {
    match result {
        Ok(true) => stats.indexed += 1,
        Ok(false) => stats.skipped += 1,
        Err(error) => {
            eprintln!("Error indexing {}: {error}", file_path.display());
            stats.errors += 1;
        }
    }
}

fn build_walker(path: &Path) -> Result<ignore::Walk> {
    let mut types = TypesBuilder::new();
    types.add_defaults();
    types.select("rust");
    types.select("php");
    types.select("py");
    types.add("js", "*.js")?;
    types.add("jsx", "*.jsx")?;
    types.select("js");
    types.select("jsx");
    types.select("ts");
    types.add("qml", "*.qml")?;
    types.select("qml");
    let types = types.build().context("Failed to build file types")?;

    Ok(WalkBuilder::new(path).types(types).build())
}

pub(crate) fn read_file_metadata(path: &Path) -> Result<(i64, i64)> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("Failed to read metadata for {}", path.display()))?;
    let size = i64::try_from(metadata.len())
        .with_context(|| format!("File is too large to index: {}", path.display()))?;
    let modified_ns = metadata
        .modified()
        .with_context(|| format!("Failed to read modification time for {}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .with_context(|| format!("Modification time predates Unix epoch: {}", path.display()))?
        .as_nanos();
    let modified_ns = i64::try_from(modified_ns)
        .with_context(|| format!("Modification time is out of range: {}", path.display()))?;
    Ok((size, modified_ns))
}

pub fn store_parse_result(
    db: &Database,
    file_id: i64,
    result: &crate::model::ParseResult,
) -> Result<()> {
    let mut symbol_ids: Vec<(String, i64)> = Vec::new();

    for sym in &result.symbols {
        let parent_id = sym
            .parent_name
            .as_ref()
            .and_then(|pn| symbol_ids.iter().find(|(n, _)| n == pn).map(|(_, id)| *id));
        let sym_id = db.insert_symbol(file_id, sym, parent_id)?;
        symbol_ids.push((sym.name.clone(), sym_id));
    }

    for reference in &result.references {
        let source_sym_id = reference.source_symbol_name.as_ref().and_then(|n| {
            symbol_ids
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, id)| *id)
        });
        db.insert_ref(file_id, reference, source_sym_id)?;
    }

    for import in &result.imports {
        db.insert_import(file_id, import)?;
    }

    Ok(())
}

#[derive(Default)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub pruned: usize,
}

impl IndexStats {
    /// Whether this pass changed the symbol graph (re-indexed or removed files),
    /// meaning reference resolution must be recomputed.
    pub fn changed_graph(&self) -> bool {
        self.indexed > 0 || self.pruned > 0
    }
}

impl std::fmt::Display for IndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Indexed: {}, Skipped: {}, Pruned: {}, Errors: {}",
            self.indexed, self.skipped, self.pruned, self.errors
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_index_rust_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.rs");
        fs::write(&file, "pub fn hello() { println!(\"hi\"); }\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        let stats = index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.skipped, 0);

        let (files, symbols, _) = db.get_stats().unwrap();
        assert_eq!(files, 1);
        assert!(symbols >= 1);
    }

    #[test]
    fn test_index_qml_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("View.qml");
        fs::write(
            &file,
            r#"
import QtQuick

Rectangle {
  property string title: ""

  function activate() {
    return title
  }
}
"#,
        )
        .unwrap();

        let db = Database::open_in_memory().unwrap();
        let stats = index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        assert_eq!(stats.indexed, 1);
        assert_eq!(stats.skipped, 0);

        let (files, symbols, _) = db.get_stats().unwrap();
        assert_eq!(files, 1);
        assert!(symbols >= 3);
    }

    #[test]
    fn test_skip_unchanged() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.py");
        fs::write(&file, "def greet():\n    pass\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        let stats2 = index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        assert_eq!(stats2.indexed, 0);
        assert_eq!(stats2.skipped, 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_skip_unchanged_file_without_reading_contents() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.py");
        fs::write(&file, "def greet():\n    pass\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&file, "def greet():\n    pass\n").unwrap();
        let metadata_refresh = index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        assert_eq!(metadata_refresh.skipped, 1);

        let original_permissions = fs::metadata(&file).unwrap().permissions();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();

        let stats = index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();

        fs::set_permissions(&file, original_permissions).unwrap();
        assert_eq!(stats.indexed, 0);
        assert_eq!(stats.skipped, 1);
        assert_eq!(stats.errors, 0);
    }

    #[test]
    fn test_incremental_index_removes_deleted_files() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("deleted.py");
        fs::write(&file, "def removed():\n    pass\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        fs::remove_file(&file).unwrap();

        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();

        let (files, symbols, refs) = db.get_stats().unwrap();
        assert_eq!((files, symbols, refs), (0, 0, 0));
    }

    #[test]
    fn test_full_reindex() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("test.py");
        fs::write(&file, "def greet():\n    pass\n").unwrap();

        let db = Database::open_in_memory().unwrap();
        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        let stats2 = index_directory(&db, tmp.path().to_str().unwrap(), true).unwrap();
        assert_eq!(stats2.indexed, 1);
    }

    #[test]
    fn prepare_index_does_not_wait_for_database_writer() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lib.rs");
        fs::write(&file, "fn original_symbol() {}\n").unwrap();
        let database_path = tmp.path().join("index.db");
        let db = Database::open(database_path.to_str().unwrap()).unwrap();
        index_directory(&db, tmp.path().to_str().unwrap(), false).unwrap();
        fs::write(&file, "fn replacement_symbol() {}\n").unwrap();

        let writer = rusqlite::Connection::open(&database_path).unwrap();
        writer
            .execute_batch(
                "BEGIN IMMEDIATE;
                 INSERT INTO meta (key, value) VALUES ('active_writer', 'held');",
            )
            .unwrap();

        let prepared = prepare_index(&db, tmp.path(), false);
        writer.execute_batch("ROLLBACK").unwrap();

        let prepared = prepared.unwrap();
        assert_eq!(prepared.files.len(), 1);
        let parsed = prepared.files[0]
            .update
            .as_ref()
            .unwrap()
            .parse_result
            .as_ref()
            .unwrap();
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.name == "replacement_symbol")
        );
    }
}
