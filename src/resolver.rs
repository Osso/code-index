use anyhow::Result;
use std::collections::HashMap;

use crate::db::Database;

/// Resolve unresolved references by matching target_name to known symbols.
pub fn resolve_references(db: &Database) -> Result<ResolveResult> {
    let name_to_symbols = build_symbol_map(db)?;
    let unresolved = find_unresolved_refs(db)?;
    let import_map = build_import_map(db)?;

    let mut resolved = 0;
    let mut ambiguous = 0;

    db.begin_transaction()?;

    for (ref_id, target_name, target_qualifier, source_file_id) in &unresolved {
        match resolve_single_ref(
            &name_to_symbols,
            &import_map,
            target_name,
            target_qualifier.as_deref(),
            *source_file_id,
        ) {
            Resolution::Resolved(sym_id) => {
                db.resolve_ref(*ref_id, sym_id)?;
                resolved += 1;
            }
            Resolution::Ambiguous => ambiguous += 1,
            Resolution::Unresolved => {}
        }
    }

    db.commit()?;

    Ok(ResolveResult {
        total: unresolved.len(),
        resolved,
        ambiguous,
    })
}

enum Resolution {
    Resolved(i64),
    Ambiguous,
    Unresolved,
}

fn resolve_single_ref(
    name_to_symbols: &HashMap<String, Vec<SymbolEntry>>,
    import_map: &HashMap<(i64, String), String>,
    target_name: &str,
    target_qualifier: Option<&str>,
    source_file_id: i64,
) -> Resolution {
    let candidates = match name_to_symbols.get(target_name) {
        Some(c) => c,
        None => {
            return try_import_resolution(name_to_symbols, import_map, target_name, source_file_id);
        }
    };

    if candidates.len() == 1 {
        return Resolution::Resolved(candidates[0].id);
    }

    // Try file proximity: prefer symbol in same file
    let same_file: Vec<_> = candidates
        .iter()
        .filter(|s| s.file_id == source_file_id)
        .collect();
    if same_file.len() == 1 {
        return Resolution::Resolved(same_file[0].id);
    }

    if let Some(qualifier) = target_qualifier {
        let qualified: Vec<_> = candidates
            .iter()
            .filter(|symbol| matches_qualifier(symbol, qualifier))
            .collect();
        if qualified.len() == 1 {
            return Resolution::Resolved(qualified[0].id);
        }
    }

    // Try import-based resolution
    if let Some(imported_path) = import_map.get(&(source_file_id, target_name.to_string())) {
        for sym in candidates {
            if sym.file_path.contains(imported_path) {
                return Resolution::Resolved(sym.id);
            }
        }
    }

    Resolution::Ambiguous
}

fn try_import_resolution(
    name_to_symbols: &HashMap<String, Vec<SymbolEntry>>,
    import_map: &HashMap<(i64, String), String>,
    target_name: &str,
    source_file_id: i64,
) -> Resolution {
    // Check if there's an import that maps this name to a different symbol name
    if let Some(full_path) = import_map.get(&(source_file_id, target_name.to_string())) {
        let actual_name = full_path
            .rsplit(&['\\', '.', ':'][..])
            .next()
            .unwrap_or(full_path);
        if let Some(candidates) = name_to_symbols.get(actual_name) {
            if candidates.len() == 1 {
                return Resolution::Resolved(candidates[0].id);
            }
        }
    }
    Resolution::Unresolved
}

struct SymbolEntry {
    id: i64,
    file_id: i64,
    file_path: String,
    parent_name: Option<String>,
}

fn build_symbol_map(db: &Database) -> Result<HashMap<String, Vec<SymbolEntry>>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.file_id, f.path, parent.name
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         LEFT JOIN symbols parent ON s.parent_id = parent.id",
    )?;
    let mut map: HashMap<String, Vec<SymbolEntry>> = HashMap::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;

    for row in rows {
        let (id, name, file_id, file_path, parent_name) = row?;
        map.entry(name).or_default().push(SymbolEntry {
            id,
            file_id,
            file_path,
            parent_name,
        });
    }
    Ok(map)
}

fn find_unresolved_refs(db: &Database) -> Result<Vec<(i64, String, Option<String>, i64)>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT id, target_name, target_qualifier, source_file_id
             FROM refs
             WHERE resolved = 0",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Build a map from (file_id, local_name) → full_path for import resolution
fn build_import_map(db: &Database) -> Result<HashMap<(i64, String), String>> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT file_id, local_name, full_path FROM imports")?;
    let mut map = HashMap::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (file_id, local_name, full_path) = row?;
        map.insert((file_id, local_name), full_path);
    }
    Ok(map)
}

pub struct ResolveResult {
    pub total: usize,
    pub resolved: usize,
    pub ambiguous: usize,
}

fn matches_qualifier(symbol: &SymbolEntry, qualifier: &str) -> bool {
    qualifier_path_matches(symbol, qualifier) || symbol.parent_name.as_deref() == Some(qualifier)
}

fn qualifier_path_matches(symbol: &SymbolEntry, qualifier: &str) -> bool {
    qualifier
        .split("::")
        .filter(|segment| !segment.is_empty())
        .all(|segment| file_path_contains_segment(&symbol.file_path, segment))
}

fn file_path_contains_segment(file_path: &str, segment: &str) -> bool {
    let file_name = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .strip_suffix(".rs")
        .unwrap_or(file_path);
    if file_name == segment {
        return true;
    }
    file_path.contains(&format!("/{segment}/"))
}

impl std::fmt::Display for ResolveResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Resolution: {}/{} resolved, {} ambiguous, {} unresolved",
            self.resolved,
            self.total,
            self.ambiguous,
            self.total - self.resolved - self.ambiguous
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RefKind, Reference, Symbol, SymbolKind};

    fn insert_symbol(
        db: &Database,
        file_id: i64,
        name: &str,
        parent_id: Option<i64>,
    ) -> anyhow::Result<i64> {
        let symbol = Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            line_start: 0,
            line_end: 4,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        db.insert_symbol(file_id, &symbol, parent_id)
    }

    #[test]
    fn resolve_references_uses_target_qualifier_for_module_calls() {
        let db = Database::open_in_memory().unwrap();
        let source_file_id = db
            .upsert_file("/repo/tests/combat.rs", "src", "rust")
            .unwrap();
        let melee_file_id = db
            .upsert_file("/repo/src/formulas/melee.rs", "melee", "rust")
            .unwrap();
        let spell_file_id = db
            .upsert_file("/repo/src/formulas/spell.rs", "spell", "rust")
            .unwrap();

        let melee_id = insert_symbol(&db, melee_file_id, "resolve_hit", None).unwrap();
        insert_symbol(&db, spell_file_id, "resolve_hit", None).unwrap();

        let reference = Reference {
            kind: RefKind::Call,
            target_name: "resolve_hit".to_string(),
            target_qualifier: Some("melee".to_string()),
            line: 12,
            source_symbol_name: None,
        };
        let ref_id = db.insert_ref(source_file_id, &reference, None).unwrap();

        let result = resolve_references(&db).unwrap();
        assert_eq!(result.resolved, 1);
        assert_eq!(result.ambiguous, 0);

        let resolved_target: i64 = db
            .conn()
            .query_row(
                "SELECT target_symbol_id FROM refs WHERE id = ?1",
                [ref_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resolved_target, melee_id);
    }

    #[test]
    fn resolve_references_uses_parent_name_for_methods() {
        let db = Database::open_in_memory().unwrap();
        let source_file_id = db
            .upsert_file("/repo/tests/aggro.rs", "src", "rust")
            .unwrap();
        let leash_file_id = db
            .upsert_file("/repo/src/combat/leash.rs", "leash", "rust")
            .unwrap();
        let path_file_id = db
            .upsert_file("/repo/src/movement/path.rs", "path", "rust")
            .unwrap();

        let leash_type = Symbol {
            name: "Leash".to_string(),
            kind: SymbolKind::Struct,
            line_start: 0,
            line_end: 10,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        let leash_type_id = db.insert_symbol(leash_file_id, &leash_type, None).unwrap();
        let path_type = Symbol {
            name: "Path".to_string(),
            kind: SymbolKind::Struct,
            line_start: 0,
            line_end: 10,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        let path_type_id = db.insert_symbol(path_file_id, &path_type, None).unwrap();

        let leash_method_id =
            insert_symbol(&db, leash_file_id, "should_evade", Some(leash_type_id)).unwrap();
        insert_symbol(&db, path_file_id, "should_evade", Some(path_type_id)).unwrap();

        let reference = Reference {
            kind: RefKind::Call,
            target_name: "should_evade".to_string(),
            target_qualifier: Some("Leash".to_string()),
            line: 5,
            source_symbol_name: None,
        };
        let ref_id = db.insert_ref(source_file_id, &reference, None).unwrap();

        let result = resolve_references(&db).unwrap();
        assert_eq!(result.resolved, 1);
        assert_eq!(result.ambiguous, 0);

        let resolved_target: i64 = db
            .conn()
            .query_row(
                "SELECT target_symbol_id FROM refs WHERE id = ?1",
                [ref_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(resolved_target, leash_method_id);
    }
}
