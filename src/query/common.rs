use crate::model::StoredSymbol;

/// Split a qualified name like "Class::method" or "Namespace\\Class" into (name, qualifier).
/// Returns (original, None) if no separator is found.
pub(crate) fn parse_qualified_name(input: &str) -> (&str, Option<&str>) {
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

pub(crate) fn map_stored_symbol(row: &rusqlite::Row) -> rusqlite::Result<StoredSymbol> {
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
