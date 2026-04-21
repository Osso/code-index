use anyhow::{Context, Result};
use rusqlite::params;

use crate::db::Database;
use crate::model::StoredSymbol;

use super::common::{map_stored_symbol, parse_qualified_name};

/// Find symbol definitions by name, optionally filtered by kind and file.
pub fn find_symbols(
    db: &Database,
    name: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<Vec<StoredSymbol>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();

    let mut sql = if qualifier.is_some() {
        String::from(
            "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
             FROM symbols s JOIN files f ON s.file_id = f.id
             JOIN symbols p ON s.parent_id = p.id
             WHERE s.name = ?1 AND p.name = ?2",
        )
    } else {
        String::from(
            "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1",
        )
    };

    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(bare_name.to_string()));
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    let next_idx = param_values.len() + 1;

    if let Some(k) = kind {
        sql.push_str(&format!(" AND s.kind = ?{next_idx}"));
        param_values.push(Box::new(k.to_string()));
    }
    let next_idx = param_values.len() + 1;
    if let Some(f) = file {
        sql.push_str(&format!(" AND f.path LIKE '%' || ?{next_idx} || '%'"));
        param_values.push(Box::new(f.to_string()));
    }

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_stored_symbol)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query symbols")
}

/// List all symbols, optionally filtered by kind and/or file.
pub fn list_symbols(
    db: &Database,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<Vec<StoredSymbol>> {
    let conn = db.conn();
    let mut sql = String::from(
        "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
         FROM symbols s JOIN files f ON s.file_id = f.id",
    );
    let mut conditions = Vec::new();
    if kind.is_some() {
        conditions.push("s.kind = ?1");
    }
    if file.is_some() {
        let idx = if kind.is_some() { "?2" } else { "?1" };
        conditions.push(Box::leak(
            format!("f.path LIKE '%' || {idx} || '%'").into_boxed_str(),
        ));
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY f.path, s.line_start");

    let mut stmt = conn.prepare(&sql)?;
    let rows = match (kind, file) {
        (Some(k), Some(f)) => stmt.query_map(params![k, f], map_stored_symbol)?,
        (Some(k), None) => stmt.query_map(params![k], map_stored_symbol)?,
        (None, Some(f)) => stmt.query_map(params![f], map_stored_symbol)?,
        (None, None) => stmt.query_map([], map_stored_symbol)?,
    };
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query symbols")
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
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query dead code")
}
