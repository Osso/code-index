use anyhow::{Context, Result};
use rusqlite::types::ToSql;

use crate::db::Database;
use crate::model::StoredReference;

use super::common::parse_qualified_name;

type SqlParam = Box<dyn ToSql>;

const REFERENCES_SQL_QUALIFIED_WITH_KIND: &str =
    "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
        tf.path, ts.name
 FROM refs r
 JOIN files f ON r.source_file_id = f.id
 LEFT JOIN symbols s ON r.source_symbol_id = s.id
 LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
 LEFT JOIN files tf ON ts.file_id = tf.id
 WHERE r.target_name = ?1 AND r.target_qualifier = ?2 AND r.kind = ?3
 ORDER BY f.path, r.line";

const REFERENCES_SQL_QUALIFIED: &str =
    "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
        tf.path, ts.name
 FROM refs r
 JOIN files f ON r.source_file_id = f.id
 LEFT JOIN symbols s ON r.source_symbol_id = s.id
 LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
 LEFT JOIN files tf ON ts.file_id = tf.id
 WHERE r.target_name = ?1 AND r.target_qualifier = ?2
 ORDER BY f.path, r.line";

const REFERENCES_SQL_WITH_KIND: &str =
    "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
        tf.path, ts.name
 FROM refs r
 JOIN files f ON r.source_file_id = f.id
 LEFT JOIN symbols s ON r.source_symbol_id = s.id
 LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
 LEFT JOIN files tf ON ts.file_id = tf.id
 WHERE r.target_name = ?1 AND r.kind = ?2
 ORDER BY f.path, r.line";

const REFERENCES_SQL_BASE: &str =
    "SELECT f.path, s.name, r.target_name, r.target_qualifier, r.kind, r.line, r.resolved,
        tf.path, ts.name
 FROM refs r
 JOIN files f ON r.source_file_id = f.id
 LEFT JOIN symbols s ON r.source_symbol_id = s.id
 LEFT JOIN symbols ts ON r.target_symbol_id = ts.id
 LEFT JOIN files tf ON ts.file_id = tf.id
 WHERE r.target_name = ?1
 ORDER BY f.path, r.line";

/// Find all structural references to a symbol.
pub fn find_references(
    db: &Database,
    name: &str,
    kind: Option<&str>,
) -> Result<Vec<StoredReference>> {
    let (bare_name, qualifier) = parse_qualified_name(name);
    let conn = db.conn();
    let sql = references_sql(qualifier, kind);
    let params = references_params(bare_name, qualifier, kind);
    execute_references_query(conn, sql, params)
}

fn references_sql(qualifier: Option<&str>, kind: Option<&str>) -> &'static str {
    match (qualifier, kind) {
        (Some(_), Some(_)) => REFERENCES_SQL_QUALIFIED_WITH_KIND,
        (Some(_), None) => REFERENCES_SQL_QUALIFIED,
        (None, Some(_)) => REFERENCES_SQL_WITH_KIND,
        (None, None) => REFERENCES_SQL_BASE,
    }
}

fn references_params(
    bare_name: &str,
    qualifier: Option<&str>,
    kind: Option<&str>,
) -> Vec<SqlParam> {
    let mut param_values: Vec<SqlParam> = vec![Box::new(bare_name.to_string())];
    if let Some(q) = qualifier {
        param_values.push(Box::new(q.to_string()));
    }
    if let Some(k) = kind {
        param_values.push(Box::new(k.to_string()));
    }
    param_values
}

fn execute_references_query(
    conn: &rusqlite::Connection,
    sql: &str,
    param_values: Vec<SqlParam>,
) -> Result<Vec<StoredReference>> {
    let mut stmt = conn.prepare(sql)?;
    let param_refs: Vec<&dyn ToSql> = param_values.iter().map(|p| p.as_ref()).collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use crate::query::test_support::{call_ref, file, symbol, test_db};

    #[test]
    fn find_references_filters_qualified_calls_and_maps_resolution() {
        let db = test_db();
        let service_file = file(&db, "/repo/src/service.rs");
        let caller_file = file(&db, "/repo/src/controller.rs");
        let service = symbol(&db, service_file, "Service", SymbolKind::Struct, 2, None);
        let run = symbol(
            &db,
            service_file,
            "run",
            SymbolKind::Method,
            4,
            Some(service),
        );
        let controller = symbol(&db, caller_file, "handle", SymbolKind::Function, 10, None);
        let ref_id = call_ref(
            &db,
            caller_file,
            Some(controller),
            "run",
            Some("Service"),
            12,
        );
        call_ref(&db, caller_file, Some(controller), "run", None, 14);
        db.resolve_ref(ref_id, run).unwrap();

        let refs = find_references(&db, "Service.run", Some("call")).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].source_file, "/repo/src/controller.rs");
        assert_eq!(refs[0].source_symbol.as_deref(), Some("handle"));
        assert_eq!(refs[0].target_file.as_deref(), Some("/repo/src/service.rs"));
        assert_eq!(refs[0].target_symbol.as_deref(), Some("run"));
        assert!(refs[0].resolved);
    }

    #[test]
    fn find_references_without_kind_returns_unresolved_matches() {
        let db = test_db();
        let file_id = file(&db, "/repo/src/free.rs");
        call_ref(&db, file_id, None, "free_fn", None, 3);

        let refs = find_references(&db, "free_fn", None).unwrap();

        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].source_symbol, None);
        assert_eq!(refs[0].target_name, "free_fn");
        assert!(!refs[0].resolved);
    }
}
