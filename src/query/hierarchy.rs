use anyhow::Result;
use rusqlite::params;

use crate::db::Database;
use crate::model::HierarchyEntry;

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
