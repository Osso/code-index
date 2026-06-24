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
    let sql = callers_by_file_sql(target_ids.len());
    let query_params = callers_by_file_params(target_ids, bare_name);
    execute_callers_by_file_query(conn, &sql, query_params)
}

fn callers_by_file_sql(target_id_count: usize) -> String {
    let placeholders = std::iter::repeat("?")
        .take(target_id_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
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
    )
}

fn callers_by_file_params(target_ids: Vec<i64>, bare_name: &str) -> Vec<SqlParam> {
    let mut params: Vec<SqlParam> = Vec::new();
    for id in target_ids {
        params.push(Box::new(id));
    }
    params.push(Box::new(bare_name.to_string()));
    params
}

fn execute_callers_by_file_query(
    conn: &rusqlite::Connection,
    sql: &str,
    params: Vec<SqlParam>,
) -> Result<Vec<CallInfo>> {
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
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
    let sql = callees_sql(qualifier, file);
    let params = callees_params(bare_name, qualifier, file);
    execute_callees_query(conn, sql, params)
}

fn callees_sql(qualifier: Option<&str>, file: Option<&str>) -> &'static str {
    match (qualifier, file) {
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
    }
}

fn callees_params(bare_name: &str, qualifier: Option<&str>, file: Option<&str>) -> Vec<SqlParam> {
    let mut param_values: Vec<SqlParam> = vec![Box::new(bare_name.to_string())];
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    if let Some(f) = file {
        param_values.push(Box::new(f.to_string()));
    }
    param_values
}

fn execute_callees_query(
    conn: &rusqlite::Connection,
    sql: &str,
    param_values: Vec<SqlParam>,
) -> Result<Vec<CallInfo>> {
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use crate::query::test_support::{call_ref, file, symbol, test_db};

    #[test]
    fn find_callers_follows_name_and_resolved_file_paths() {
        let db = test_db();
        let target_file = file(&db, "/repo/src/target.rs");
        let caller_file = file(&db, "/repo/src/caller.rs");
        let target = symbol(&db, target_file, "target", SymbolKind::Function, 2, None);
        let caller = symbol(&db, caller_file, "caller", SymbolKind::Function, 8, None);
        let grand_caller = symbol(
            &db,
            caller_file,
            "grand_caller",
            SymbolKind::Function,
            20,
            None,
        );
        let target_ref = call_ref(&db, caller_file, Some(caller), "target", None, 9);
        call_ref(&db, caller_file, Some(grand_caller), "caller", None, 21);
        db.resolve_ref(target_ref, target).unwrap();

        let recursive = find_callers(&db, "target", None, 2).unwrap();
        let names: Vec<_> = recursive
            .iter()
            .map(|call| call.symbol_name.as_str())
            .collect();
        assert_eq!(names, vec!["caller", "grand_caller"]);

        let by_file = find_callers(&db, "target", Some("target.rs"), 1).unwrap();
        assert_eq!(by_file.len(), 1);
        assert_eq!(by_file[0].symbol_name, "caller");
        assert_eq!(by_file[0].file_path, "/repo/src/caller.rs");
    }

    #[test]
    fn find_callees_filters_qualified_symbol_and_file() {
        let db = test_db();
        let file_id = file(&db, "/repo/src/service.rs");
        let service = symbol(&db, file_id, "Service", SymbolKind::Struct, 2, None);
        let run = symbol(&db, file_id, "run", SymbolKind::Method, 5, Some(service));
        let target = symbol(&db, file_id, "target", SymbolKind::Function, 12, None);
        symbol(&db, file_id, "leaf", SymbolKind::Function, 30, None);
        call_ref(&db, file_id, Some(run), "target", None, 6);
        call_ref(&db, file_id, Some(target), "leaf", None, 13);

        let callees = find_callees(&db, "Service.run", Some("service.rs"), 2).unwrap();
        let names: Vec<_> = callees
            .iter()
            .map(|call| call.symbol_name.as_str())
            .collect();

        assert_eq!(names, vec!["target", "leaf"]);
        assert!(callees.iter().all(|call| call.kind == "call"));
        assert!(
            callees
                .iter()
                .all(|call| call.file_path == "/repo/src/service.rs")
        );
    }
}
