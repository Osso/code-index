use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::model::{Import, Reference, Symbol};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    hash TEXT NOT NULL,
    lang TEXT NOT NULL,
    indexed_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS symbols (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    parent_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
    visibility TEXT,
    signature TEXT,
    is_test INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS refs (
    id INTEGER PRIMARY KEY,
    source_file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    source_symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
    target_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,
    kind TEXT NOT NULL,
    target_name TEXT NOT NULL,
    target_qualifier TEXT,
    line INTEGER NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS imports (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    local_name TEXT NOT NULL,
    full_path TEXT NOT NULL,
    alias TEXT,
    line INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_file_id ON symbols(file_id);
CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_refs_source_symbol ON refs(source_symbol_id);
CREATE INDEX IF NOT EXISTS idx_refs_target_symbol ON refs(target_symbol_id);
CREATE INDEX IF NOT EXISTS idx_refs_target_name ON refs(target_name);
CREATE INDEX IF NOT EXISTS idx_refs_kind ON refs(kind);
CREATE INDEX IF NOT EXISTS idx_imports_local_name ON imports(local_name);
CREATE INDEX IF NOT EXISTS idx_imports_full_path ON imports(full_path);

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open database")?;
        // busy_timeout must be set before WAL/migration: concurrent writers (the
        // watcher daemon plus a CLI invocation) otherwise fail instantly with
        // "database is locked" instead of waiting for the lock to clear.
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .context("Failed to set busy_timeout")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("Failed to set pragmas")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to open in-memory database")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .context("Failed to set pragmas")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn
            .execute_batch(SCHEMA)
            .context("Failed to run migrations")?;
        // Add is_test column if missing (existing DBs)
        let _ = self
            .conn
            .execute_batch("ALTER TABLE symbols ADD COLUMN is_test INTEGER NOT NULL DEFAULT 0");
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Insert or update a file record. Returns the file ID.
    pub fn upsert_file(&self, path: &str, hash: &str, lang: &str) -> Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        self.conn.execute(
            "INSERT INTO files (path, hash, lang, indexed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET hash=?2, lang=?3, indexed_at=?4",
            params![path, hash, lang, now],
        )?;

        // Do not use last_insert_rowid() here: on ON CONFLICT DO UPDATE it may retain
        // an unrelated previous insert rowid instead of the file row id.
        let id: i64 = self.conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    /// Get file hash to check if re-indexing is needed
    pub fn get_file_hash(&self, path: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT hash FROM files WHERE path = ?1")?;
        let result = stmt
            .query_row(params![path], |row| row.get::<_, String>(0))
            .ok();
        Ok(result)
    }

    pub fn list_file_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("Failed to list indexed files")
    }

    pub fn delete_file_by_path(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    /// Delete all symbols and refs for a file (before re-indexing)
    pub fn clear_file_data(&self, file_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
        self.conn.execute(
            "DELETE FROM refs WHERE source_file_id = ?1",
            params![file_id],
        )?;
        self.conn
            .execute("DELETE FROM imports WHERE file_id = ?1", params![file_id])?;
        Ok(())
    }

    /// Clear the entire index so a full reindex starts from a clean graph.
    pub fn reset_index(&self) -> Result<()> {
        self.conn.execute("DELETE FROM refs", [])?;
        self.conn.execute("DELETE FROM imports", [])?;
        self.conn.execute("DELETE FROM symbols", [])?;
        self.conn.execute("DELETE FROM files", [])?;
        Ok(())
    }

    /// Insert a symbol. Returns the symbol ID.
    pub fn insert_symbol(
        &self,
        file_id: i64,
        symbol: &Symbol,
        parent_id: Option<i64>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO symbols (file_id, name, kind, line_start, line_end, parent_id, visibility, signature, is_test)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                file_id,
                symbol.name,
                symbol.kind.as_str(),
                symbol.line_start as i64,
                symbol.line_end as i64,
                parent_id,
                symbol.visibility,
                symbol.signature,
                symbol.is_test as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert a reference (call, inheritance, etc.)
    pub fn insert_ref(
        &self,
        file_id: i64,
        reference: &Reference,
        source_symbol_id: Option<i64>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO refs (source_file_id, source_symbol_id, kind, target_name, target_qualifier, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                file_id,
                source_symbol_id,
                reference.kind.as_str(),
                reference.target_name,
                reference.target_qualifier,
                reference.line as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert an import statement
    pub fn insert_import(&self, file_id: i64, import: &Import) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO imports (file_id, local_name, full_path, alias, line)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file_id,
                import.local_name,
                import.full_path,
                import.alias,
                import.line as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Resolve a reference by setting its target_symbol_id
    pub fn resolve_ref(&self, ref_id: i64, target_symbol_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE refs SET target_symbol_id = ?1, resolved = 1 WHERE id = ?2",
            params![target_symbol_id, ref_id],
        )?;
        Ok(())
    }

    /// Get total counts for status reporting
    pub fn get_stats(&self) -> Result<(i64, i64, i64)> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let symbols: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let refs: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM refs", [], |row| row.get(0))?;
        Ok((files, symbols, refs))
    }

    /// Begin a transaction for batch operations
    pub fn begin_transaction(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        Ok(())
    }

    /// Commit a transaction
    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Rollback a transaction
    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    /// Read a value from the key/value meta table.
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok();
        Ok(value)
    }

    /// Write (insert or replace) a value in the key/value meta table.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RefKind, SymbolKind};

    #[test]
    fn test_schema_creation() {
        let db = Database::open_in_memory().unwrap();
        let (files, symbols, refs) = db.get_stats().unwrap();
        assert_eq!(files, 0);
        assert_eq!(symbols, 0);
        assert_eq!(refs, 0);
    }

    #[test]
    fn test_upsert_file() {
        let db = Database::open_in_memory().unwrap();
        let id1 = db.upsert_file("/test.rs", "abc123", "rust").unwrap();
        let id2 = db.upsert_file("/test.rs", "def456", "rust").unwrap();
        assert_eq!(id1, id2);

        let hash = db.get_file_hash("/test.rs").unwrap().unwrap();
        assert_eq!(hash, "def456");
    }

    #[test]
    fn test_upsert_file_after_other_inserts_returns_correct_id() {
        let db = Database::open_in_memory().unwrap();

        let file_id = db.upsert_file("/test.rs", "abc123", "rust").unwrap();
        let other_file_id = db.upsert_file("/other.rs", "other", "rust").unwrap();

        let sym = Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
            line_start: 1,
            line_end: 1,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        db.insert_symbol(other_file_id, &sym, None).unwrap();

        let updated_id = db.upsert_file("/test.rs", "newhash", "rust").unwrap();
        assert_eq!(updated_id, file_id);
    }

    #[test]
    fn test_insert_symbol_and_ref() {
        let db = Database::open_in_memory().unwrap();
        let file_id = db.upsert_file("/test.rs", "abc", "rust").unwrap();

        let sym = Symbol {
            name: "main".to_string(),
            kind: SymbolKind::Function,
            line_start: 1,
            line_end: 10,
            parent_name: None,
            visibility: Some("pub".to_string()),
            signature: Some("fn main()".to_string()),
            is_test: false,
        };
        let sym_id = db.insert_symbol(file_id, &sym, None).unwrap();

        let reference = Reference {
            kind: RefKind::Call,
            target_name: "println".to_string(),
            target_qualifier: None,
            line: 5,
            source_symbol_name: Some("main".to_string()),
        };
        db.insert_ref(file_id, &reference, Some(sym_id)).unwrap();

        let (_, sym_count, ref_count) = db.get_stats().unwrap();
        assert_eq!(sym_count, 1);
        assert_eq!(ref_count, 1);
    }

    #[test]
    fn test_clear_file_data() {
        let db = Database::open_in_memory().unwrap();
        let file_id = db.upsert_file("/test.rs", "abc", "rust").unwrap();

        let sym = Symbol {
            name: "foo".to_string(),
            kind: SymbolKind::Function,
            line_start: 1,
            line_end: 5,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        db.insert_symbol(file_id, &sym, None).unwrap();
        db.clear_file_data(file_id).unwrap();

        let (_, sym_count, _) = db.get_stats().unwrap();
        assert_eq!(sym_count, 0);
    }

    #[test]
    fn test_reset_index_clears_files_symbols_and_refs() {
        let db = Database::open_in_memory().unwrap();
        let file_id = db.upsert_file("/test.rs", "abc", "rust").unwrap();

        let sym = Symbol {
            name: "foo".to_string(),
            kind: SymbolKind::Function,
            line_start: 1,
            line_end: 5,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        let sym_id = db.insert_symbol(file_id, &sym, None).unwrap();
        let reference = Reference {
            kind: RefKind::Call,
            target_name: "bar".to_string(),
            target_qualifier: None,
            line: 2,
            source_symbol_name: Some("foo".to_string()),
        };
        db.insert_ref(file_id, &reference, Some(sym_id)).unwrap();

        db.reset_index().unwrap();

        let (files, symbols, refs) = db.get_stats().unwrap();
        assert_eq!((files, symbols, refs), (0, 0, 0));
    }

    #[test]
    fn test_delete_file_by_path_cascades_index_data() {
        let db = Database::open_in_memory().unwrap();
        let file_id = db.upsert_file("/test.rs", "abc", "rust").unwrap();

        let sym = Symbol {
            name: "foo".to_string(),
            kind: SymbolKind::Function,
            line_start: 1,
            line_end: 5,
            parent_name: None,
            visibility: None,
            signature: None,
            is_test: false,
        };
        db.insert_symbol(file_id, &sym, None).unwrap();

        db.delete_file_by_path("/test.rs").unwrap();

        let (files, symbols, refs) = db.get_stats().unwrap();
        assert_eq!((files, symbols, refs), (0, 0, 0));
    }
}
