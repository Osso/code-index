use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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

    // Walk up from CWD looking for .code-index.db
    let mut dir = cwd.as_path();
    loop {
        if dir.join(DB_FILENAME).exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }

    bail!(
        "Not indexed: {}\n\n  To index this directory:\n    code-index index\n\n  Or register an existing project:\n    code-index project add <name>\n\n  Supported languages: PHP, Rust, Python, TypeScript",
        cwd.display()
    )
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
