use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub path: String,
}

fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Cannot determine config directory")?;
    Ok(config_dir.join("code-index").join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| format!("Failed to parse {}", path.display()))
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(config).context("Failed to serialize config")?;
    std::fs::write(&path, contents).with_context(|| format!("Failed to write {}", path.display()))
}

pub fn add_project(name: &str, path: &Path) -> Result<()> {
    let canonical = path
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {}", path.display()))?;
    let mut config = load()?;
    config.projects.insert(
        name.to_string(),
        ProjectEntry {
            path: canonical.to_string_lossy().to_string(),
        },
    );
    save(&config)
}

pub fn remove_project(name: &str) -> Result<bool> {
    let mut config = load()?;
    let removed = config.projects.remove(name).is_some();
    if removed {
        save(&config)?;
    }
    Ok(removed)
}

pub fn find_project_for_path(dir: &Path) -> Result<Option<String>> {
    let config = load()?;
    let canonical = dir
        .canonicalize()
        .with_context(|| format!("Cannot resolve path: {}", dir.display()))?;
    let dir_str = canonical.to_string_lossy();

    let mut best_match: Option<(&str, usize)> = None;
    for (name, entry) in &config.projects {
        if dir_str.starts_with(&entry.path) {
            let len = entry.path.len();
            if best_match.is_none() || len > best_match.unwrap().1 {
                best_match = Some((name, len));
            }
        }
    }

    Ok(best_match.map(|(name, _)| name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::CWD_LOCK;

    fn with_temp_config_dir<T>(run: impl FnOnce(&Path) -> T) -> T {
        let _guard = CWD_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        let old_config_home = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: tests that mutate process-wide environment hold CWD_LOCK for
        // the full mutation window, and other tests that read project config use
        // the same lock.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }
        let result = run(tmp.path());
        unsafe {
            match old_config_home {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        result
    }

    #[test]
    fn load_returns_default_when_config_is_missing() {
        with_temp_config_dir(|_| {
            let config = load().unwrap();

            assert!(config.projects.is_empty());
        });
    }

    #[test]
    fn save_and_load_round_trip_projects() {
        with_temp_config_dir(|_| {
            let mut config = Config::default();
            config.projects.insert(
                "demo".to_string(),
                ProjectEntry {
                    path: "/tmp/demo".to_string(),
                },
            );

            save(&config).unwrap();
            let loaded = load().unwrap();

            assert_eq!(loaded.projects["demo"].path, "/tmp/demo");
        });
    }

    #[test]
    fn add_remove_and_find_project_use_canonical_paths() {
        with_temp_config_dir(|_| {
            let tmp = tempfile::TempDir::new().unwrap();
            let project = tmp.path().join("project");
            let nested = project.join("src");
            std::fs::create_dir_all(&nested).unwrap();

            add_project("demo", &project).unwrap();
            assert_eq!(
                find_project_for_path(&nested).unwrap(),
                Some("demo".to_string())
            );
            assert!(remove_project("demo").unwrap());
            assert!(!remove_project("demo").unwrap());
            assert_eq!(find_project_for_path(&nested).unwrap(), None);
        });
    }

    #[test]
    fn find_project_for_path_prefers_longest_registered_prefix() {
        with_temp_config_dir(|_| {
            let tmp = tempfile::TempDir::new().unwrap();
            let parent = tmp.path().join("parent");
            let child = parent.join("child");
            let nested = child.join("src");
            std::fs::create_dir_all(&nested).unwrap();

            add_project("parent", &parent).unwrap();
            add_project("child", &child).unwrap();

            assert_eq!(
                find_project_for_path(&nested).unwrap(),
                Some("child".to_string())
            );
        });
    }
}
