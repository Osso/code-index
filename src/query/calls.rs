use anyhow::{Context, Result};
use rusqlite::{params, types::ToSql};

use crate::db::Database;
use crate::model::CallInfo;

use super::common::parse_qualified_name;

type SqlParam = Box<dyn ToSql>;

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
    if let Some(target_file) = file {
        query_callers_by_file(db, name, target_file)
    } else {
        query_callers_by_name(db, name)
    }
}

fn query_callers_by_name(db: &Database, name: &str) -> Result<Vec<CallInfo>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();
    let sql = callers_by_name_sql(qualifier.is_some());
    let mut stmt = conn.prepare(sql)?;
    execute_callers_by_name_query(&mut stmt, bare_name, qualifier)
}

fn callers_by_name_sql(has_qualifier: bool) -> &'static str {
    if has_qualifier {
        "SELECT DISTINCT s.name, f.path, r.line, r.kind
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.target_name = ?1 AND r.kind = 'call' AND r.target_qualifier = ?2"
    } else {
        "SELECT DISTINCT s.name, f.path, r.line, r.kind
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.target_name = ?1 AND r.kind = 'call'"
    }
}

fn execute_callers_by_name_query(
    stmt: &mut rusqlite::Statement<'_>,
    bare_name: &str,
    qualifier: Option<&str>,
) -> Result<Vec<CallInfo>> {
    let rows = if let Some(q) = qualifier {
        stmt.query_map(params![bare_name, q], map_call_info)?
    } else {
        stmt.query_map(params![bare_name], map_call_info)?
    };
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query callers")
}

fn resolve_symbol_ids_in_file(
    conn: &rusqlite::Connection,
    name: &str,
    target_file: &str,
) -> Result<Vec<i64>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let (sql, params_vec) = symbol_id_query_and_params(bare_name, qualifier, target_file);
    query_symbol_ids(conn, sql, params_vec)
}

fn symbol_id_query_and_params(
    bare_name: &str,
    qualifier: Option<&str>,
    target_file: &str,
) -> (&'static str, Vec<SqlParam>) {
    if let Some(q) = qualifier {
        (
            "SELECT s.id
             FROM symbols s
             JOIN files f ON s.file_id = f.id
             JOIN symbols p ON s.parent_id = p.id
             WHERE s.name = ?1
             AND f.path LIKE '%' || ?2 || '%'
             AND p.name = ?3",
            vec![
                Box::new(bare_name.to_string()) as SqlParam,
                Box::new(target_file.to_string()),
                Box::new(q.to_string()),
            ],
        )
    } else {
        (
            "SELECT s.id
             FROM symbols s
             JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1
             AND f.path LIKE '%' || ?2 || '%'",
            vec![
                Box::new(bare_name.to_string()) as SqlParam,
                Box::new(target_file.to_string()),
            ],
        )
    }
}

fn query_symbol_ids(
    conn: &rusqlite::Connection,
    sql: &str,
    params_vec: Vec<SqlParam>,
) -> Result<Vec<i64>> {
    let mut id_stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    id_stmt
        .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to resolve symbol ids")
}

fn query_callers_by_file(db: &Database, name: &str, target_file: &str) -> Result<Vec<CallInfo>> {
    let (bare_name, _qualifier) = parse_qualified_name(name);
    let conn = db.conn();
    let target_ids = resolve_symbol_ids_in_file(conn, name, target_file)?;

    if target_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = std::iter::repeat("?")
        .take(target_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT DISTINCT s.name, f.path, r.line, r.kind
         FROM refs r
         JOIN files f ON r.source_file_id = f.id
         LEFT JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.kind = 'call'
         AND (
            r.target_symbol_id IN ({})
            OR (r.target_symbol_id IS NULL AND r.target_name = ?)
         )",
        placeholders
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut dyn_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    for id in target_ids {
        dyn_params.push(Box::new(id));
    }
    dyn_params.push(Box::new(bare_name.to_string()));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        dyn_params.iter().map(|p| p.as_ref()).collect();

    let rows = stmt.query_map(param_refs.as_slice(), map_call_info)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query callers")
}

fn map_call_info(row: &rusqlite::Row) -> rusqlite::Result<CallInfo> {
    Ok(CallInfo {
        symbol_name: row
            .get::<_, Option<String>>(0)?
            .unwrap_or_else(|| "<top-level>".into()),
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
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();

    let sql = match (qualifier, file) {
        (Some(_), Some(_)) => {
            "SELECT r.target_name, f.path, r.line, r.kind
             FROM refs r
             JOIN symbols s ON r.source_symbol_id = s.id
             JOIN symbols p ON s.parent_id = p.id
             JOIN files f ON r.source_file_id = f.id
             WHERE s.name = ?1 AND p.name = ?2 AND r.kind = 'call'
             AND f.path LIKE '%' || ?3 || '%'"
        }
        (Some(_), None) => {
            "SELECT r.target_name, f.path, r.line, r.kind
             FROM refs r
             JOIN symbols s ON r.source_symbol_id = s.id
             JOIN symbols p ON s.parent_id = p.id
             JOIN files f ON r.source_file_id = f.id
             WHERE s.name = ?1 AND p.name = ?2 AND r.kind = 'call'"
        }
        (None, Some(_)) => {
            "SELECT r.target_name, f.path, r.line, r.kind
             FROM refs r
             JOIN symbols s ON r.source_symbol_id = s.id
             JOIN files f ON r.source_file_id = f.id
             WHERE s.name = ?1 AND r.kind = 'call'
             AND f.path LIKE '%' || ?2 || '%'"
        }
        (None, None) => {
            "SELECT r.target_name, f.path, r.line, r.kind
             FROM refs r
             JOIN symbols s ON r.source_symbol_id = s.id
             JOIN files f ON r.source_file_id = f.id
             WHERE s.name = ?1 AND r.kind = 'call'"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(bare_name.to_string()));
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    if let Some(f) = file {
        param_values.push(Box::new(f.to_string()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_callee_info)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query callees")
}

fn map_callee_info(row: &rusqlite::Row) -> rusqlite::Result<CallInfo> {
    Ok(CallInfo {
        symbol_name: row.get(0)?,
        file_path: row.get(1)?,
        line: row.get(2)?,
        kind: row.get(3)?,
    })
}
