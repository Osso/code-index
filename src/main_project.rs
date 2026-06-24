use anyhow::Result;

use crate::{ProjectAction, config};

#[cfg_attr(coverage_nightly, coverage(off))]
pub(super) fn cmd_project(action: ProjectAction) -> Result<()> {
    run_project_action(action)
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn run_project_action(action: ProjectAction) -> Result<()> {
    match action {
        ProjectAction::Add { name, path } => cmd_project_add(&name, path)?,
        ProjectAction::Remove { name } => cmd_project_remove(&name)?,
        ProjectAction::List => cmd_project_list()?,
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_project_add(name: &str, path: Option<String>) -> Result<()> {
    let dir = resolve_project_registration_dir(path)?;
    config::add_project(name, &dir)?;
    println!("Registered project '{}' at {}", name, dir.display());
    Ok(())
}

fn resolve_project_registration_dir(path: Option<String>) -> Result<std::path::PathBuf> {
    match path {
        Some(p) => Ok(std::path::PathBuf::from(p)),
        None => std::env::current_dir().map_err(Into::into),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_project_remove(name: &str) -> Result<()> {
    let removed = config::remove_project(name)?;
    print_project_remove_result(name, removed);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn print_project_remove_result(name: &str, removed: bool) {
    let message = project_remove_result_message(name, removed);
    println!("{message}");
}

fn project_remove_result_message(name: &str, removed: bool) -> String {
    match removed {
        true => format!("Removed project '{name}'"),
        false => format!("Project '{name}' not found"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_project_list() -> Result<()> {
    let config = config::load()?;
    print_project_entries(&config.projects);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn print_project_entries(projects: &std::collections::BTreeMap<String, config::ProjectEntry>) {
    let rows = project_rows(projects);
    if rows.is_empty() {
        println!("No projects registered.");
        return;
    }
    print_project_rows(&rows);
}

fn project_rows(
    projects: &std::collections::BTreeMap<String, config::ProjectEntry>,
) -> Vec<String> {
    projects
        .iter()
        .map(|(name, entry)| format_project_list_row(name, entry))
        .collect()
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn print_project_rows(rows: &[String]) {
    for row in rows {
        println!("{row}");
    }
}

fn project_index_status(project_path: &str) -> &'static str {
    let db_file = std::path::Path::new(project_path).join(".code-index.db");
    if db_file.exists() {
        "indexed"
    } else {
        "not indexed"
    }
}

fn format_project_list_row(name: &str, entry: &config::ProjectEntry) -> String {
    let status = project_index_status(&entry.path);
    format!("{name}: {} ({status})", entry.path)
}
