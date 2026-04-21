use anyhow::{Context, Result};
use rusqlite::params;

use crate::db::Database;
use crate::model::{
    CallInfo, HierarchyEntry, ImportedByEntry, ResolvedImport, StoredReference, StoredSymbol,
};

/// Split a qualified name like "Class::method" or "Namespace\Class" into (name, qualifier).
/// Returns (original, None) if no separator is found.
fn parse_qualified_name(input: &str) -> (&str, Option<&str>) {
    // Try :: first (PHP static methods, Rust paths)
    if let Some(pos) = input.rfind("::") {
        return (&input[pos + 2..], Some(&input[..pos]));
    }
    // Try backslash (PHP namespaces)
    if let Some(pos) = input.rfind('\\') {
        return (&input[pos + 1..], Some(&input[..pos]));
    }
    // Try dot (Python, TypeScript)
    if let Some(pos) = input.rfind('.') {
        return (&input[pos + 1..], Some(&input[..pos]));
    }
    (input, None)
}

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

    let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) = if let Some(q) = qualifier
    {
        (
            "SELECT s.id
             FROM symbols s
             JOIN files f ON s.file_id = f.id
             JOIN symbols p ON s.parent_id = p.id
             WHERE s.name = ?1
             AND f.path LIKE '%' || ?2 || '%'
             AND p.name = ?3",
            vec![
                Box::new(bare_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
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
                Box::new(bare_name.to_string()) as Box<dyn rusqlite::types::ToSql>,
                Box::new(target_file.to_string()),
            ],
        )
    };

    let mut id_stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();
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

/// Find all structural references to a symbol.
pub fn find_references(
    db: &Database,
    name: &str,
    kind: Option<&str>,
) -> Result<Vec<StoredReference>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();

    let sql = match (qualifier, kind) {
        (Some(_), Some(_)) => {
            "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                    tf.path, ts.name
             FROM refs r
             JOIN files f ON r.source_file_id = f.id
             LEFT JOIN symbols s ON r.source_symbol_id = s.id
             LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
             LEFT JOIN files tf ON ts.file_id = tf.id
             WHERE r.target_name = ?1 AND r.target_qualifier = ?2 AND r.kind = ?3
             ORDER BY f.path, r.line"
        }
        (Some(_), None) => {
            "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                    tf.path, ts.name
             FROM refs r
             JOIN files f ON r.source_file_id = f.id
             LEFT JOIN symbols s ON r.source_symbol_id = s.id
             LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
             LEFT JOIN files tf ON ts.file_id = tf.id
             WHERE r.target_name = ?1 AND r.target_qualifier = ?2
             ORDER BY f.path, r.line"
        }
        (None, Some(_)) => {
            "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                    tf.path, ts.name
             FROM refs r
             JOIN files f ON r.source_file_id = f.id
             LEFT JOIN symbols s ON r.source_symbol_id = s.id
             LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
             LEFT JOIN files tf ON ts.file_id = tf.id
             WHERE r.target_name = ?1 AND r.kind = ?2
             ORDER BY f.path, r.line"
        }
        (None, None) => {
            "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
                    tf.path, ts.name
             FROM refs r
             JOIN files f ON r.source_file_id = f.id
             LEFT JOIN symbols s ON r.source_symbol_id = s.id
             LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
             LEFT JOIN files tf ON ts.file_id = tf.id
             WHERE r.target_name = ?1
             ORDER BY f.path, r.line"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    param_values.push(Box::new(bare_name.to_string()));
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    if let Some(k) = kind {
        param_values.push(Box::new(k.to_string()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_stored_reference)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query references")
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

fn resolve_import_target(
    db: &Database,
    local_name: &str,
    full_path: &str,
) -> Result<Option<SymbolTarget>> {
    let conn = db.conn();

    let actual_name = full_path
        .rsplit(&['\\', '.', ':', '/'][..])
        .next()
        .unwrap_or(full_path);

    let candidates = query_symbol_candidates(conn, actual_name)?;

    if candidates.is_empty() {
        if local_name != actual_name {
            let fallback = query_symbol_candidates(conn, local_name)?;
            return Ok(fallback.into_iter().next());
        }
        return Ok(None);
    }

    Ok(pick_best_candidate(candidates, full_path))
}

/// Find test functions that transitively call a given symbol.
pub fn find_tested_by(
    db: &Database,
    name: &str,
    file: Option<&str>,
    depth: u32,
) -> Result<Vec<StoredSymbol>> {
    // Get all callers transitively
    let callers = find_callers(db, name, file, depth)?;

    // Filter to only test symbols
    let conn = db.conn();
    let mut tests = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for caller in &callers {
        if seen.contains(&caller.symbol_name) {
            continue;
        }
        seen.insert(caller.symbol_name.clone());

        let mut stmt = conn.prepare(
            "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
             FROM symbols s JOIN files f ON s.file_id = f.id
             WHERE s.name = ?1 AND s.is_test = 1",
        )?;
        let rows = stmt
            .query_map(params![caller.symbol_name], map_stored_symbol)?
            .collect::<Result<Vec<_>, _>>()?;
        tests.extend(rows);
    }

    // Also check if the symbol itself is a test
    let (bare_name, _) = parse_qualified_name(name);
    let mut stmt = conn.prepare(
        "SELECT s.id, f.path, s.name, s.kind, s.line_start, s.line_end, s.visibility, s.signature
         FROM symbols s JOIN files f ON s.file_id = f.id
         WHERE s.name = ?1 AND s.is_test = 1",
    )?;
    let self_tests = stmt
        .query_map(params![bare_name], map_stored_symbol)?
        .collect::<Result<Vec<_>, _>>()?;
    tests.extend(self_tests);

    Ok(tests)
}

/// Find functions/methods not called by any test (transitively).
pub fn find_untested(
    db: &Database,
    path: Option<&str>,
    exclude: &[String],
) -> Result<Vec<StoredSymbol>> {
    let reachable = build_test_reachable(db)?;
    let candidates = query_untested_candidates(db, path, exclude)?;
    let untested = candidates
        .into_iter()
        .filter(|sym| !reachable.contains(&sym.name))
        .collect();
    Ok(untested)
}

/// Build set of all symbol names reachable from test functions via call edges.
fn build_test_reachable(db: &Database) -> Result<std::collections::HashSet<String>> {
    let conn = db.conn();

    // Load call graph as adjacency list: caller_name -> [callee_names]
    let mut adj: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT s.name, r.target_name
         FROM refs r
         JOIN symbols s ON r.source_symbol_id = s.id
         WHERE r.kind = 'call'",
    )?;
    let edges = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for edge in edges {
        let (caller, callee) = edge?;
        adj.entry(caller).or_default().push(callee);
    }

    // Get all test function names as BFS seeds
    let mut seeds: Vec<String> = Vec::new();
    let mut stmt = conn.prepare("SELECT name FROM symbols WHERE is_test = 1")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        seeds.push(row?);
    }

    // BFS from test functions through callees
    let mut reachable = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    for seed in &seeds {
        reachable.insert(seed.clone());
        queue.push_back(seed.clone());
    }
    while let Some(name) = queue.pop_front() {
        if let Some(callees) = adj.get(&name) {
            for callee in callees {
                if reachable.insert(callee.clone()) {
                    queue.push_back(callee.clone());
                }
            }
        }
    }

    Ok(reachable)
}

fn query_untested_candidates(
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
         AND s.is_test = 0
         AND s.name NOT IN ('main', 'new', '__init__', '__construct')",
    );

    if path.is_some() {
        sql.push_str(" AND f.path LIKE '%' || ?1 || '%'");
    }

    for (i, _) in exclude.iter().enumerate() {
        let param_idx = if path.is_some() { i + 2 } else { i + 1 };
        sql.push_str(&format!(" AND s.name != ?{}", param_idx));
    }

    sql.push_str(" ORDER BY f.path, s.line_start");

    let mut stmt = conn.prepare(&sql)?;
    let mut dyn_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(p) = path {
        dyn_params.push(Box::new(p.to_string()));
    }
    for ex in exclude {
        dyn_params.push(Box::new(ex.clone()));
    }
    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        dyn_params.iter().map(|p| p.as_ref()).collect();

    let candidates: Vec<StoredSymbol> = stmt
        .query_map(param_refs.as_slice(), map_stored_symbol)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(candidates)
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

/// Find class/trait hierarchy (ancestors, descendants, or both).
pub fn find_hierarchy(db: &Database, name: &str, direction: &str) -> Result<Vec<HierarchyEntry>> {
    let mut results = Vec::new();

    if direction == "ancestors" || direction == "both" {
        find_ancestors(
            db,
            name,
            0,
            &mut results,
            &mut std::collections::HashSet::new(),
        )?;
    }
    if direction == "descendants" || direction == "both" {
        find_descendants(
            db,
            name,
            0,
            &mut results,
            &mut std::collections::HashSet::new(),
        )?;
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
