use anyhow::{Context, Result};
use rusqlite::params;

use crate::db::Database;
use crate::model::{ImportedByEntry, ResolvedImport};

/// Resolve an import: given a name or path, find where it comes from.
pub fn resolve_import(
    db: &Database,
    name: &str,
    file: Option<&str>,
) -> Result<Vec<ResolvedImport>> {
    let conn = db.conn();

    let imports = query_imports(conn, name, file)?;

    let mut results = Vec::new();
    for (source_file, local_name, full_path, alias, line) in imports {
        let target = resolve_import_target(db, &local_name, &full_path)?;
        results.push(ResolvedImport {
            source_file,
            local_name,
            full_path,
            alias,
            line,
            target_file: target.as_ref().map(|t| t.0.clone()),
            target_symbol: target.as_ref().map(|t| t.1.clone()),
            target_kind: target.as_ref().map(|t| t.2.clone()),
            target_line: target.as_ref().map(|t| t.3),
        });
    }

    Ok(results)
}

fn query_imports(
    conn: &rusqlite::Connection,
    name: &str,
    file: Option<&str>,
) -> Result<Vec<(String, String, String, Option<String>, i64)>> {
    let sql = if file.is_some() {
        "SELECT f.path, i.local_name, i.full_path, i.alias, i.line
         FROM imports i
         JOIN files f ON i.file_id = f.id
         WHERE (i.local_name = ?1 OR i.full_path LIKE '%' || ?1 || '%')
         AND f.path LIKE '%' || ?2 || '%'
         ORDER BY f.path, i.line"
    } else {
        "SELECT f.path, i.local_name, i.full_path, i.alias, i.line
         FROM imports i
         JOIN files f ON i.file_id = f.id
         WHERE i.local_name = ?1 OR i.full_path LIKE '%' || ?1 || '%'
         ORDER BY f.path, i.line"
    };

    let mut stmt = conn.prepare(sql)?;
    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<_> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
        ))
    };

    let rows = if let Some(f) = file {
        stmt.query_map(params![name, f], map_row)?
    } else {
        stmt.query_map(params![name], map_row)?
    };
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query imports")
}

type SymbolTarget = (String, String, String, i64);

fn query_symbol_candidates(conn: &rusqlite::Connection, name: &str) -> Result<Vec<SymbolTarget>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, s.name, s.kind, s.line_start
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1
         ORDER BY f.path",
    )?;
    stmt.query_map(params![name], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?
    .collect::<Result<Vec<_>, _>>()
    .context("Failed to query symbol candidates")
}

fn pick_best_candidate(candidates: Vec<SymbolTarget>, full_path: &str) -> Option<SymbolTarget> {
    if candidates.len() == 1 {
        return candidates.into_iter().next();
    }
    let path_parts: Vec<&str> = full_path.split(&['\\', '.', ':', '/'][..]).collect();
    for candidate in &candidates {
        let file_lower = candidate.0.to_lowercase();
        if path_parts
            .iter()
            .all(|p| file_lower.contains(&p.to_lowercase()))
        {
            return Some(candidate.clone());
        }
    }
    candidates.into_iter().next()
}

fn extract_import_symbol_name(full_path: &str) -> &str {
    full_path
        .rsplit(&['\\', '.', ':', '/'][..])
        .next()
        .unwrap_or(full_path)
}

fn resolve_import_target_from_candidates(
    conn: &rusqlite::Connection,
    local_name: &str,
    actual_name: &str,
    full_path: &str,
    candidates: Vec<SymbolTarget>,
) -> Result<Option<SymbolTarget>> {
    if !candidates.is_empty() {
        return Ok(pick_best_candidate(candidates, full_path));
    }
    if local_name == actual_name {
        return Ok(None);
    }
    let fallback = query_symbol_candidates(conn, local_name)?;
    Ok(fallback.into_iter().next())
}

fn resolve_import_target(
    db: &Database,
    local_name: &str,
    full_path: &str,
) -> Result<Option<SymbolTarget>> {
    let conn = db.conn();
    let actual_name = extract_import_symbol_name(full_path);
    let candidates = query_symbol_candidates(conn, actual_name)?;
    resolve_import_target_from_candidates(conn, local_name, actual_name, full_path, candidates)
}

/// Find files that import a given module/symbol (reverse dependency lookup).
pub fn find_imported_by(
    db: &Database,
    name: &str,
    file: Option<&str>,
) -> Result<Vec<ImportedByEntry>> {
    let conn = db.conn();

    let sql = if file.is_some() {
        "SELECT f.path, i.local_name, i.full_path, i.alias, i.line
         FROM imports i
         JOIN files f ON i.file_id = f.id
         WHERE i.full_path LIKE '%' || ?1 || '%'
         AND f.path LIKE '%' || ?2 || '%'
         ORDER BY f.path, i.line"
    } else {
        "SELECT f.path, i.local_name, i.full_path, i.alias, i.line
         FROM imports i
         JOIN files f ON i.file_id = f.id
         WHERE i.full_path LIKE '%' || ?1 || '%'
         ORDER BY f.path, i.line"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(f) = file {
        stmt.query_map(params![name, f], map_imported_by)?
    } else {
        stmt.query_map(params![name], map_imported_by)?
    };
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query imported-by")
}

fn map_imported_by(row: &rusqlite::Row) -> rusqlite::Result<ImportedByEntry> {
    Ok(ImportedByEntry {
        file_path: row.get(0)?,
        local_name: row.get(1)?,
        full_path: row.get(2)?,
        alias: row.get(3)?,
        line: row.get(4)?,
    })
}
