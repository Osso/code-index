use anyhow::Result;
use rusqlite::params;

use crate::db::Database;
use crate::model::StoredSymbol;

use super::calls::find_callers;
use super::common::{map_stored_symbol, parse_qualified_name};

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
