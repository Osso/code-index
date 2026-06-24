use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config;

const DB_FILENAME: &str = ".code-index.db";

/// Resolve the project directory from an explicit path or CWD.
pub fn resolve_project_dir(explicit_path: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = explicit_path {
        return Path::new(p)
            .canonicalize()
            .with_context(|| format!("Cannot resolve path: {p}"));
    }

    let cwd = std::env::current_dir().context("Cannot determine current directory")?;

    // Check registered projects (longest prefix match)
    if let Some(name) = config::find_project_for_path(&cwd)? {
        let config = config::load()?;
        if let Some(entry) = config.projects.get(&name) {
            return Path::new(&entry.path).canonicalize().with_context(|| {
                format!("Cannot resolve registered project path: {}", entry.path)
            });
        }
    }

    find_indexed_ancestor(&cwd).unwrap_or(Ok(cwd))
}

fn find_indexed_ancestor(cwd: &Path) -> Option<Result<PathBuf>> {
    let mut dir = cwd;
    loop {
        if dir.join(DB_FILENAME).exists() {
            return Some(Ok(dir.to_path_buf()));
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

/// Get the DB file path for a project directory.
pub fn db_path(project_dir: &Path) -> String {
    project_dir.join(DB_FILENAME).to_string_lossy().to_string()
}

/// Resolve project dir and return the DB path. Convenience wrapper.
pub fn resolve_db(explicit_path: Option<&str>) -> Result<String> {
    let dir = resolve_project_dir(explicit_path)?;
    Ok(db_path(&dir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::CWD_LOCK;

    #[test]
    fn unindexed_cwd_resolves_to_cwd() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();

        let resolved = resolve_project_dir(None).unwrap();

        std::env::set_current_dir(old_cwd).unwrap();
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn explicit_path_resolves_to_canonical_directory() {
        let tmp = tempfile::TempDir::new().unwrap();

        let resolved = resolve_project_dir(Some(tmp.path().to_str().unwrap())).unwrap();

        assert_eq!(resolved, tmp.path().canonicalize().unwrap());
    }

    #[test]
    fn indexed_ancestor_wins_over_nested_cwd() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let nested = tmp.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.path().join(DB_FILENAME), "").unwrap();
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&nested).unwrap();

        let resolved = resolve_project_dir(None).unwrap();

        std::env::set_current_dir(old_cwd).unwrap();
        assert_eq!(resolved, tmp.path());
    }

    #[test]
    fn resolve_db_appends_index_filename() {
        let tmp = tempfile::TempDir::new().unwrap();

        let resolved = resolve_db(Some(tmp.path().to_str().unwrap())).unwrap();

        assert!(resolved.ends_with(".code-index.db"));
        assert_eq!(resolved, db_path(tmp.path()));
    }
}
