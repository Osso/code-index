use anyhow::{Context, Result};
use rusqlite::params;

use crate::db::Database;
use crate::model::{ImportedByEntry, ResolvedImport};

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

fn extract_import_symbol_name(full_path: &str) -> &str {
    full_path
        .rsplit(&['\\', '.', ':', '/'][..])
        .next()
        .unwrap_or(full_path)
}

fn resolve_import_target_from_candidates(
    conn: &rusqlite::Connection,
    local_name: &str,
    actual_name: &str,
    full_path: &str,
    candidates: Vec<SymbolTarget>,
) -> Result<Option<SymbolTarget>> {
    if !candidates.is_empty() {
        return Ok(pick_best_candidate(candidates, full_path));
    }
    if local_name == actual_name {
        return Ok(None);
    }
    let fallback = query_symbol_candidates(conn, local_name)?;
    Ok(fallback.into_iter().next())
}

fn resolve_import_target(
    db: &Database,
    local_name: &str,
    full_path: &str,
) -> Result<Option<SymbolTarget>> {
    let conn = db.conn();
    let actual_name = extract_import_symbol_name(full_path);
    let candidates = query_symbol_candidates(conn, actual_name)?;
    resolve_import_target_from_candidates(conn, local_name, actual_name, full_path, candidates)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SymbolKind;
    use crate::query::test_support::{file, import, symbol, test_db};

    #[test]
    fn resolve_import_maps_alias_to_best_symbol_candidate() {
        let db = test_db();
        let source_file = file(&db, "/repo/src/app.rs");
        let target_file = file(&db, "/repo/src/service.rs");
        let other_file = file(&db, "/repo/vendor/service.rs");
        symbol(&db, target_file, "Service", SymbolKind::Struct, 1, None);
        symbol(&db, other_file, "Service", SymbolKind::Struct, 1, None);
        import(
            &db,
            source_file,
            "LocalService",
            "src::service::Service",
            Some("LocalService"),
            5,
        );

        let resolved = resolve_import(&db, "LocalService", Some("app.rs")).unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].source_file, "/repo/src/app.rs");
        assert_eq!(resolved[0].alias.as_deref(), Some("LocalService"));
        assert_eq!(
            resolved[0].target_file.as_deref(),
            Some("/repo/src/service.rs")
        );
        assert_eq!(resolved[0].target_symbol.as_deref(), Some("Service"));
        assert_eq!(resolved[0].target_kind.as_deref(), Some("struct"));
    }

    #[test]
    fn resolve_import_falls_back_to_local_name_and_imported_by_filters_file() {
        let db = test_db();
        let source_file = file(&db, "/repo/src/consumer.rs");
        let test_file = file(&db, "/repo/tests/consumer_test.rs");
        let target_file = file(&db, "/repo/src/widget.rs");
        symbol(&db, target_file, "Widget", SymbolKind::Struct, 1, None);
        import(&db, source_file, "Widget", "crate::renamed", None, 4);
        import(&db, test_file, "Widget", "crate::renamed", None, 6);

        let resolved = resolve_import(&db, "renamed", None).unwrap();
        assert_eq!(resolved.len(), 2);
        assert!(
            resolved
                .iter()
                .all(|entry| entry.target_symbol.as_deref() == Some("Widget"))
        );

        let imported_by = find_imported_by(&db, "renamed", Some("tests")).unwrap();
        assert_eq!(imported_by.len(), 1);
        assert_eq!(imported_by[0].file_path, "/repo/tests/consumer_test.rs");
        assert_eq!(imported_by[0].local_name, "Widget");
    }
}
