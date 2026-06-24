use anyhow::{Context, Result};
use rusqlite::{params, types::ToSql};

use crate::db::Database;
use crate::model::StoredSymbol;

use super::common::{map_stored_symbol, parse_qualified_name};

type SqlParam = Box<dyn ToSql>;

/// Find symbol definitions by name, optionally filtered by kind and file.
pub fn find_symbols(
    db: &Database,
    name: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<Vec<StoredSymbol>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();
    let (sql, param_values) = build_find_symbols_query(bare_name, qualifier, kind, file);

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), map_stored_symbol)?;
    rows.collect::<Result<Vec<_>, _>>()
        .context("Failed to query symbols")
}

fn build_find_symbols_query(
    bare_name: &str,
    qualifier: Option<&str>,
    kind: Option<&str>,
    file: Option<&str>,
) -> (String, Vec<SqlParam>) {
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
    let mut param_values: Vec<SqlParam> = vec![Box::new(bare_name.to_string())];
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    if let Some(k) = kind {
        let next_idx = param_values.len() + 1;
        sql.push_str(&format!(" AND s.kind = ?{next_idx}"));
        param_values.push(Box::new(k.to_string()));
    }
    if let Some(f) = file {
        let next_idx = param_values.len() + 1;
        sql.push_str(&format!(" AND f.path LIKE '%' || ?{next_idx} || '%'"));
        param_values.push(Box::new(f.to_string()));
    }
    (sql, param_values)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use crate::query::test_support::{call_ref, file, symbol, test_db};

    #[test]
    fn find_symbols_filters_by_parent_kind_and_file() {
        let db = test_db();
        let service_file = file(&db, "/repo/src/service.rs");
        let other_file = file(&db, "/repo/tests/service_test.rs");
        let service = symbol(&db, service_file, "Service", SymbolKind::Struct, 3, None);
        symbol(
            &db,
            service_file,
            "run",
            SymbolKind::Method,
            7,
            Some(service),
        );
        symbol(&db, other_file, "run", SymbolKind::Function, 4, None);

        let matches = find_symbols(&db, "Service.run", Some("method"), Some("src")).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "run");
        assert_eq!(matches[0].kind, "method");
        assert_eq!(matches[0].file_path, "/repo/src/service.rs");
    }

    #[test]
    fn list_symbols_and_dead_code_apply_filters_and_excludes() {
        let db = test_db();
        let file_id = file(&db, "/repo/src/lib.rs");
        let used = symbol(&db, file_id, "used", SymbolKind::Function, 2, None);
        let caller = symbol(&db, file_id, "caller", SymbolKind::Function, 8, None);
        symbol(&db, file_id, "main", SymbolKind::Function, 20, None);
        symbol(&db, file_id, "ignored", SymbolKind::Function, 30, None);
        call_ref(&db, file_id, Some(caller), "used", None, 9);

        let functions = list_symbols(&db, Some("function"), Some("src")).unwrap();
        assert_eq!(functions.len(), 4);
        assert!(functions.iter().any(|s| s.id == used));

        let dead = find_dead_code(&db, Some("src"), &["ignored".to_string()]).unwrap();
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].name, "caller");
    }
}
