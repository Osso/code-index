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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RefKind, SymbolKind};
    use crate::query::test_support::{file, reference, symbol, test_db};

    #[test]
    fn find_hierarchy_walks_ancestors_and_descendants() {
        let db = test_db();
        let base_file = file(&db, "/repo/src/base.rs");
        let mid_file = file(&db, "/repo/src/mid.rs");
        let child_file = file(&db, "/repo/src/child.rs");
        symbol(&db, base_file, "Base", SymbolKind::Struct, 1, None);
        symbol(&db, mid_file, "Mid", SymbolKind::Struct, 1, None);
        symbol(&db, child_file, "Child", SymbolKind::Struct, 1, None);
        reference(&db, mid_file, None, RefKind::Inherit, "Base", None, 3);
        reference(&db, child_file, None, RefKind::Inherit, "Mid", None, 3);

        let ancestors = find_hierarchy(&db, "Child", "ancestors").unwrap();
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0].name, "Mid");
        assert_eq!(ancestors[0].relation, "ancestor");
        assert_eq!(ancestors[1].name, "Base");
        assert_eq!(ancestors[1].depth, 1);

        let descendants = find_hierarchy(&db, "Base", "descendants").unwrap();
        assert_eq!(descendants.len(), 2);
        assert_eq!(descendants[0].name, "Mid");
        assert_eq!(descendants[1].name, "Child");
        assert!(
            descendants
                .iter()
                .all(|entry| entry.relation == "descendant")
        );
    }

    #[test]
    fn find_hierarchy_both_includes_implemented_traits() {
        let db = test_db();
        let trait_file = file(&db, "/repo/src/traits.rs");
        let child_file = file(&db, "/repo/src/worker.rs");
        symbol(&db, trait_file, "Runnable", SymbolKind::Trait, 1, None);
        symbol(&db, child_file, "Worker", SymbolKind::Struct, 1, None);
        reference(
            &db,
            child_file,
            None,
            RefKind::Implement,
            "Runnable",
            None,
            4,
        );

        let entries = find_hierarchy(&db, "Runnable", "both").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "Worker");
        assert_eq!(entries[0].kind, "implement");
        assert_eq!(entries[0].file_path, "/repo/src/worker.rs");
    }
}
