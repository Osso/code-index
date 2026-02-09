use anyhow::{Context, Result};
use rusqlite::params;

use crate::db::Database;
use crate::model::{CallInfo, HierarchyEntry, StoredReference, StoredSymbol};

/// Find symbol definitions by name, optionally filtered by kind and file.
pub fn find_symbols(
    db: &Database,
    name: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<Vec<StoredSymbol>> {
    let conn = db.conn();
    let mut sql = String::from(
        "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
         FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1",
    );
    if kind.is_some() {
        sql.push_str(" AND s.kind = ?2");
    }
    if file.is_some() {
        sql.push_str(if kind.is_some() {
            " AND f.path LIKE '%' || ?3 || '%'"
        } else {
            " AND f.path LIKE '%' || ?2 || '%'"
        });
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = match (kind, file) {
        (Some(k), Some(f)) => stmt.query_map(params![name, k, f], map_stored_symbol)?,
        (Some(k), None) => stmt.query_map(params![name, k], map_stored_symbol)?,
        (None, Some(f)) => stmt.query_map(params![name, f], map_stored_symbol)?,
        (None, None) => stmt.query_map(params![name], map_stored_symbol)?,
    };
    rows.collect::<Result<Vec<_>, _>>().context("Failed to query symbols")
}

fn map_stored_symbol(row: &rusqlite::Row) -> rusqlite::Result<StoredSymbol> {
    Ok(StoredSymbol {
        id: row.get(0)?,
        file_path: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        line_start: row.get(4)?,
        line_end: row.get(5)?,
        visibility: row.get(6)?,
        signature: row.get(7)?,
    })
}

/// Find callers of a function/method (who calls this symbol).
pub fn find_callers(
    db: &Database,
    name: &str,
    file: Option<&str>,
    depth: u32,
) -> Result<Vec<CallInfo>> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    find_callers_recursive(db, name, file, depth, 0, &mut results, &mut visited)?;
    Ok(results)
}

fn find_callers_recursive(
    db: &Database,
    name: &str,
    file: Option<&str>,
    max_depth: u32,
    current_depth: u32,
    results: &mut Vec<CallInfo>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if current_depth >= max_depth || visited.contains(name) {
        return Ok(());
    }
    visited.insert(name.to_string());

    let direct = query_callers(db, name, file)?;
    for caller in &direct {
        results.push(caller.clone());
    }

    if current_depth + 1 < max_depth {
        for caller in direct {
            find_callers_recursive(
                db,
                &caller.symbol_name,
                None,
                max_depth,
                current_depth + 1,
                results,
                visited,
            )?;
        }
    }
    Ok(())
}

fn query_callers(db: &Database, name: &str, file: Option<&str>) -> Result<Vec<CallInfo>> {
    let conn = db.conn();
    let sql = if file.is_some() {
        "SELECT DISTINCT s.name, f.path, r.line, r.kind
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.target_name = ?1 AND r.kind = 'call'
         AND f.path LIKE '%' || ?2 || '%'"
    } else {
        "SELECT DISTINCT s.name, f.path, r.line, r.kind
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.target_name = ?1 AND r.kind = 'call'"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(f) = file {
        stmt.query_map(params![name, f], map_call_info)?
    } else {
        stmt.query_map(params![name], map_call_info)?
    };
    rows.collect::<Result<Vec<_>, _>>().context("Failed to query callers")
}

fn map_call_info(row: &rusqlite::Row) -> rusqlite::Result<CallInfo> {
    Ok(CallInfo {
        symbol_name: row.get::<_, Option<String>>(0)?.unwrap_or_else(|| "<top-level>".into()),
        file_path: row.get(1)?,
        line: row.get(2)?,
        kind: row.get(3)?,
    })
}

/// Find callees of a function/method (what does this symbol call).
pub fn find_callees(
    db: &Database,
    name: &str,
    file: Option<&str>,
    depth: u32,
) -> Result<Vec<CallInfo>> {
    let mut results = Vec::new();
    let mut visited = std::collections::HashSet::new();
    find_callees_recursive(db, name, file, depth, 0, &mut results, &mut visited)?;
    Ok(results)
}

fn find_callees_recursive(
    db: &Database,
    name: &str,
    file: Option<&str>,
    max_depth: u32,
    current_depth: u32,
    results: &mut Vec<CallInfo>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if current_depth >= max_depth || visited.contains(name) {
        return Ok(());
    }
    visited.insert(name.to_string());

    let direct = query_callees(db, name, file)?;
    for callee in &direct {
        results.push(callee.clone());
    }

    if current_depth + 1 < max_depth {
        for callee in direct {
            find_callees_recursive(
                db,
                &callee.symbol_name,
                None,
                max_depth,
                current_depth + 1,
                results,
                visited,
            )?;
        }
    }
    Ok(())
}

fn query_callees(db: &Database, name: &str, file: Option<&str>) -> Result<Vec<CallInfo>> {
    let conn = db.conn();

    let sql = if file.is_some() {
        "SELECT r.target_name, f.path, r.line, r.kind
         FROM refs r
         JOIN symbols s ON r.source_symbol_id = s.id
         JOIN files f ON r.source_file_id = f.id
         WHERE s.name = ?1 AND r.kind = 'call'
         AND f.path LIKE '%' || ?2 || '%'"
    } else {
        "SELECT r.target_name, f.path, r.line, r.kind
         FROM refs r
         JOIN symbols s ON r.source_symbol_id = s.id
         JOIN files f ON r.source_file_id = f.id
         WHERE s.name = ?1 AND r.kind = 'call'"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(f) = file {
        stmt.query_map(params![name, f], map_callee_info)?
    } else {
        stmt.query_map(params![name], map_callee_info)?
    };
    rows.collect::<Result<Vec<_>, _>>().context("Failed to query callees")
}

fn map_callee_info(row: &rusqlite::Row) -> rusqlite::Result<CallInfo> {
    Ok(CallInfo {
        symbol_name: row.get(0)?,
        file_path: row.get(1)?,
        line: row.get(2)?,
        kind: row.get(3)?,
    })
}

/// Find functions/methods that are never called (dead code).
pub fn find_dead_code(
    db: &Database,
    path: Option<&str>,
    exclude: &[String],
) -> Result<Vec<StoredSymbol>> {
    let conn = db.conn();

    let mut sql = String::from(
        "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
         FROM symbols s
         JOIN files f ON s.file_id = f.id
         WHERE s.kind IN ('function', 'method')
         AND NOT EXISTS (
             SELECT 1 FROM refs r WHERE r.target_name = s.name AND r.kind = 'call'
         )",
    );

    if path.is_some() {
        sql.push_str(" AND f.path LIKE '%' || ?1 || '%'");
    }

    // Exclude common entry points
    sql.push_str(" AND s.name NOT IN ('main', 'new', '__init__', '__construct')");

    for (i, _) in exclude.iter().enumerate() {
        let param_idx = if path.is_some() { i + 2 } else { i + 1 };
        sql.push_str(&format!(" AND s.name != ?{}", param_idx));
    }

    sql.push_str(" ORDER BY f.path, s.line_start");

    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(p) = path {
        params.push(Box::new(p.to_string()));
    }
    for ex in exclude {
        params.push(Box::new(ex.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), map_stored_symbol)?;
    rows.collect::<Result<Vec<_>, _>>().context("Failed to query dead code")
}

/// Find all structural references to a symbol.
pub fn find_references(
    db: &Database,
    name: &str,
    kind: Option<&str>,
) -> Result<Vec<StoredReference>> {
    let conn = db.conn();

    let sql = if kind.is_some() {
        "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                tf.path, ts.name
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
         LEFT JOIN files tf ON ts.file_id = tf.id
         WHERE r.target_name = ?1 AND r.kind = ?2
         ORDER BY f.path, r.line"
    } else {
        "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                tf.path, ts.name
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
         LEFT JOIN files tf ON ts.file_id = tf.id
         WHERE r.target_name = ?1
         ORDER BY f.path, r.line"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = if let Some(k) = kind {
        stmt.query_map(params![name, k], map_stored_reference)?
    } else {
        stmt.query_map(params![name], map_stored_reference)?
    };
    rows.collect::<Result<Vec<_>, _>>().context("Failed to query references")
}

fn map_stored_reference(row: &rusqlite::Row) -> rusqlite::Result<StoredReference> {
    Ok(StoredReference {
        source_file: row.get(0)?,
        source_symbol: row.get(1)?,
        target_name: row.get(2)?,
        target_qualifier: row.get(3)?,
        kind: row.get(4)?,
        line: row.get(5)?,
        resolved: row.get::<_, i64>(6)? != 0,
        target_file: row.get(7)?,
        target_symbol: row.get(8)?,
    })
}

/// Find class/trait hierarchy (ancestors, descendants, or both).
pub fn find_hierarchy(
    db: &Database,
    name: &str,
    direction: &str,
) -> Result<Vec<HierarchyEntry>> {
    let mut results = Vec::new();

    if direction == "ancestors" || direction == "both" {
        find_ancestors(db, name, 0, &mut results, &mut std::collections::HashSet::new())?;
    }
    if direction == "descendants" || direction == "both" {
        find_descendants(db, name, 0, &mut results, &mut std::collections::HashSet::new())?;
    }

    Ok(results)
}

fn find_ancestors(
    db: &Database,
    name: &str,
    depth: i32,
    results: &mut Vec<HierarchyEntry>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if visited.contains(name) {
        return Ok(());
    }
    visited.insert(name.to_string());

    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT r.target_name, r.kind, f.path
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         WHERE r.kind IN ('inherit', 'implement', 'trait_impl')
         AND EXISTS (
             SELECT 1 FROM symbols s WHERE s.name = ?1
             AND s.file_id = r.source_file_id
         )",
    )?;

    let rows: Vec<(String, String, String)> = stmt
        .query_map(params![name], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (parent_name, kind, file_path) in &rows {
        results.push(HierarchyEntry {
            name: parent_name.clone(),
            kind: kind.clone(),
            file_path: file_path.clone(),
            relation: "ancestor".to_string(),
            depth,
        });
        find_ancestors(db, parent_name, depth + 1, results, visited)?;
    }
    Ok(())
}

fn find_descendants(
    db: &Database,
    name: &str,
    depth: i32,
    results: &mut Vec<HierarchyEntry>,
    visited: &mut std::collections::HashSet<String>,
) -> Result<()> {
    if visited.contains(name) {
        return Ok(());
    }
    visited.insert(name.to_string());

    let conn = db.conn();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.name, r.kind, f.path
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         JOIN symbols s ON s.file_id = r.source_file_id
         WHERE r.target_name = ?1
         AND r.kind IN ('inherit', 'implement', 'trait_impl')
         AND s.kind IN ('class', 'struct', 'trait', 'interface')",
    )?;

    let rows: Vec<(String, String, String)> = stmt
        .query_map(params![name], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    for (child_name, kind, file_path) in &rows {
        results.push(HierarchyEntry {
            name: child_name.clone(),
            kind: kind.clone(),
            file_path: file_path.clone(),
            relation: "descendant".to_string(),
            depth,
        });
        find_descendants(db, child_name, depth + 1, results, visited)?;
    }
    Ok(())
}
