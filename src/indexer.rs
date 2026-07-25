use std::path::Path;

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use ignore::types::TypesBuilder;

use crate::db::Database;
use crate::model::Language;
use crate::parser;

/// Index all supported files under a directory.
/// If `full` is true, re-indexes everything regardless of hash.
pub fn index_directory(db: &Database, dir: &str, full: bool) -> Result<IndexStats> {
    let path = Path::new(dir)
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {}", dir))?;

    let walker = build_walker(&path)?;
    let mut stats = IndexStats::default();

    db.begin_transaction()?;
    if full {
        db.reset_index()?;
    } else {
        stats.pruned = prune_missing_files(db)?;
    }

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let Some((file_path, lang)) = supported_entry(&entry) else {
            continue;
        };
        let result = index_single_file(db, file_path, lang, full);
        record_index_result(result, file_path, &mut stats);
    }

    db.commit()?;
    Ok(stats)
}

fn supported_entry(entry: &ignore::DirEntry) -> Option<(&Path, Language)> {
    if !entry.file_type().is_some_and(|ft| ft.is_file()) {
        return None;
    }
    let file_path = entry.path();
    let ext = file_path.extension().and_then(|e| e.to_str())?;
    let lang = Language::from_extension(ext)?;
    Some((file_path, lang))
}

fn record_index_result(result: Result<bool>, file_path: &Path, stats: &mut IndexStats) {
    match result {
        Ok(true) => stats.indexed += 1,
        Ok(false) => stats.skipped += 1,
        Err(e) => {
            eprintln!("Error indexing {}: {}", file_path.display(), e);
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

fn prune_missing_files(db: &Database) -> Result<usize> {
    let mut removed = 0;
    for file_path in db.list_file_paths()? {
        if !Path::new(&file_path).exists() {
            db.delete_file_by_path(&file_path)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn index_single_file(db: &Database, path: &Path, lang: Language, full: bool) -> Result<bool> {
    let path_str = path.to_str().context("Non-UTF8 path")?;
    let (size, modified_ns) = read_file_metadata(path)?;
    let existing = if full {
        None
    } else {
        db.query_file_state(path_str)?
    };

    if existing
        .as_ref()
        .is_some_and(|state| state.size == Some(size) && state.modified_ns == Some(modified_ns))
    {
        return Ok(false);
    }

    let source =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {path_str}"))?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let file_id = db.upsert_file_with_metadata(
        path_str,
        &hash,
        lang.as_str(),
        Some(size),
        Some(modified_ns),
    )?;

    if existing.is_some_and(|state| state.hash == hash) {
        return Ok(false);
    }

    db.clear_file_data(file_id)?;
    let result = parser::parse_file(&source, lang)?;
    store_parse_result(db, file_id, &result)?;

    Ok(true)
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
}
