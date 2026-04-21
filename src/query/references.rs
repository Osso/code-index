use anyhow::{Context, Result};

use crate::db::Database;
use crate::model::StoredReference;

use super::common::parse_qualified_name;

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
