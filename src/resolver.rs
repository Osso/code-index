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

    for (ref_id, target_name, source_file_id) in &unresolved {
        match resolve_single_ref(&name_to_symbols, &import_map, target_name, *source_file_id) {
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
}

fn build_symbol_map(db: &Database) -> Result<HashMap<String, Vec<SymbolEntry>>> {
    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, s.file_id, f.path FROM symbols s JOIN files f ON s.file_id = f.id",
    )?;
    let mut map: HashMap<String, Vec<SymbolEntry>> = HashMap::new();

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    for row in rows {
        let (id, name, file_id, file_path) = row?;
        map.entry(name).or_default().push(SymbolEntry {
            id,
            file_id,
            file_path,
        });
    }
    Ok(map)
}

fn find_unresolved_refs(db: &Database) -> Result<Vec<(i64, String, i64)>> {
    let conn = db.conn();
    let mut stmt =
        conn.prepare("SELECT id, target_name, source_file_id FROM refs WHERE resolved = 0")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
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
