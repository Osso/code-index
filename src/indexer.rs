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

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map_or(false, |ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path();
        let ext = match file_path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        let lang = match Language::from_extension(ext) {
            Some(l) => l,
            None => continue,
        };

        match index_single_file(db, file_path, lang, full) {
            Ok(changed) => {
                if changed {
                    stats.indexed += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Err(e) => {
                eprintln!("Error indexing {}: {}", file_path.display(), e);
                stats.errors += 1;
            }
        }
    }

    db.commit()?;
    Ok(stats)
}

fn build_walker(path: &Path) -> Result<ignore::Walk> {
    let mut types = TypesBuilder::new();
    types.add_defaults();
    types.select("rust");
    types.select("php");
    types.select("py");
    types.select("ts");
    let types = types.build().context("Failed to build file types")?;

    Ok(WalkBuilder::new(path).types(types).build())
}

fn index_single_file(db: &Database, path: &Path, lang: Language, full: bool) -> Result<bool> {
    let path_str = path.to_str().context("Non-UTF8 path")?;
    let source =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {}", path_str))?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();

    if !full {
        if let Some(existing) = db.get_file_hash(path_str)? {
            if existing == hash {
                return Ok(false);
            }
        }
    }

    let file_id = db.upsert_file(path_str, &hash, lang.as_str())?;
    db.clear_file_data(file_id)?;

    let result = parser::parse_file(&source, lang)?;
    store_parse_result(db, file_id, &result)?;

    Ok(true)
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
}

impl std::fmt::Display for IndexStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Indexed: {}, Skipped: {}, Errors: {}",
            self.indexed, self.skipped, self.errors
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
