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
    let git_root = find_git_ancestor(&cwd);

    if let Some(project_dir) = find_registered_project(&cwd, git_root.as_deref())? {
        return Ok(project_dir);
    }

    find_project_ancestor(&cwd).unwrap_or(Ok(cwd))
}

fn find_registered_project(cwd: &Path, git_root: Option<&Path>) -> Result<Option<PathBuf>> {
    let Some(name) = config::find_project_for_path(cwd)? else {
        return Ok(None);
    };
    let config = config::load()?;
    let Some(entry) = config.projects.get(&name) else {
        return Ok(None);
    };
    let project_dir = Path::new(&entry.path)
        .canonicalize()
        .with_context(|| format!("Cannot resolve registered project path: {}", entry.path))?;
    if git_root.is_some_and(|root| !project_dir.starts_with(root)) {
        return Ok(None);
    }
    Ok(Some(project_dir))
}

fn find_git_ancestor(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

fn find_project_ancestor(cwd: &Path) -> Option<Result<PathBuf>> {
    let mut dir = cwd;
    loop {
        if dir.join(DB_FILENAME).exists() || dir.join(".git").exists() {
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
    fn git_worktree_root_wins_over_higher_indexed_ancestor() {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let indexed_parent = tmp.path().join("home");
        let worktree = indexed_parent.join("worktree");
        let nested = worktree.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(indexed_parent.join(DB_FILENAME), "").unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /tmp/example\n").unwrap();
        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&nested).unwrap();

        let resolved = resolve_project_dir(None).unwrap();

        std::env::set_current_dir(old_cwd).unwrap();
        assert_eq!(resolved, worktree);
    }

    #[test]
    fn resolve_db_appends_index_filename() {
        let tmp = tempfile::TempDir::new().unwrap();

        let resolved = resolve_db(Some(tmp.path().to_str().unwrap())).unwrap();

        assert!(resolved.ends_with(".code-index.db"));
        assert_eq!(resolved, db_path(tmp.path()));
    }
}
