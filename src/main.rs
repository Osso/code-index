mod config;
mod db;
mod indexer;
mod mcp;
mod model;
mod parser;
mod project;
mod query;
mod resolver;
mod watcher;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "code-index", about = "Structural code analysis MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start MCP server (stdio)
    Serve,
    /// Index a directory
    Index {
        /// Directory to index (default: current directory)
        path: Option<String>,
        #[arg(long)]
        full: bool,
    },
    /// Find symbol definitions
    Symbol {
        name: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        file: Option<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Show who calls a function/method
    Callers {
        name: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = "1")]
        depth: u32,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Show what a function calls
    Callees {
        name: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = "1")]
        depth: u32,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Find functions never called
    DeadCode {
        #[arg(long)]
        exclude: Vec<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Show class/trait inheritance tree
    Hierarchy {
        name: String,
        #[arg(long, default_value = "both")]
        direction: String,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Find all structural references to a symbol
    References {
        name: String,
        #[arg(long)]
        kind: Option<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Find test functions that call a given symbol
    TestedBy {
        name: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = "10")]
        depth: u32,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Find functions/methods not covered by any test
    Untested {
        #[arg(long)]
        exclude: Vec<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Find files that import a given module/symbol
    ImportedBy {
        name: String,
        #[arg(long)]
        file: Option<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Resolve an import to its target file and symbol
    ResolveImport {
        /// Import name or path to resolve
        name: String,
        #[arg(long)]
        file: Option<String>,
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// List all indexed symbols
    List {
        /// Filter by symbol kind (function, method, class, trait, interface, struct, enum)
        #[arg(long)]
        kind: Option<String>,
        /// Filter by file path (substring match)
        #[arg(long)]
        file: Option<String>,
        /// Override project path
        #[arg(long)]
        path: Option<String>,
    },
    /// Watch directory and re-index on changes
    Watch {
        /// Directory to watch (default: current directory)
        path: Option<String>,
    },
    /// Show index status
    Status {
        /// Project path override
        #[arg(long)]
        path: Option<String>,
    },
    /// Manage registered projects
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Register a project
    Add {
        name: String,
        /// Directory path (default: current directory)
        path: Option<String>,
    },
    /// Unregister a project
    Remove { name: String },
    /// List registered projects
    List,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    dispatch(cli.command)
}

fn dispatch(command: Command) -> Result<()> {
    match command {
        Command::Serve => run_mcp_server()?,
        Command::Index { path, full } => cmd_index(path.as_deref(), full)?,
        Command::Symbol {
            name,
            kind,
            file,
            path,
        } => {
            cmd_symbol(path.as_deref(), &name, kind.as_deref(), file.as_deref())?;
        }
        Command::Callers {
            name,
            file,
            depth,
            path,
        } => {
            cmd_callers(path.as_deref(), &name, file.as_deref(), depth)?;
        }
        Command::Callees {
            name,
            file,
            depth,
            path,
        } => {
            cmd_callees(path.as_deref(), &name, file.as_deref(), depth)?;
        }
        Command::DeadCode { exclude, path } => cmd_dead_code(path.as_deref(), &exclude)?,
        Command::Hierarchy {
            name,
            direction,
            path,
        } => {
            cmd_hierarchy(path.as_deref(), &name, &direction)?;
        }
        Command::References { name, kind, path } => {
            cmd_references(path.as_deref(), &name, kind.as_deref())?;
        }
        Command::TestedBy {
            name,
            file,
            depth,
            path,
        } => {
            cmd_tested_by(path.as_deref(), &name, file.as_deref(), depth)?;
        }
        Command::Untested { exclude, path } => cmd_untested(path.as_deref(), &exclude)?,
        Command::ImportedBy { name, file, path } => {
            cmd_imported_by(path.as_deref(), &name, file.as_deref())?;
        }
        Command::ResolveImport { name, file, path } => {
            cmd_resolve_import(path.as_deref(), &name, file.as_deref())?;
        }
        Command::List { kind, file, path } => {
            cmd_list(path.as_deref(), kind.as_deref(), file.as_deref())?;
        }
        Command::Watch { path } => cmd_watch(path.as_deref())?,
        Command::Status { path } => cmd_status(path.as_deref())?,
        Command::Project { action } => cmd_project(action)?,
    }
    Ok(())
}

fn run_mcp_server() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        use rmcp::ServiceExt;
        let service = mcp::CodeIndexService::new();
        let transport = rmcp::transport::io::stdio();
        let server = service.serve(transport).await?;
        server.waiting().await?;
        Ok(())
    })
}

fn cmd_index(path: Option<&str>, full: bool) -> Result<()> {
    let project_dir = project::resolve_project_dir(path).or_else(|_| {
        // For index, if no project found, use the explicit path or CWD
        let p = path.unwrap_or(".");
        std::path::Path::new(p)
            .canonicalize()
            .map_err(anyhow::Error::from)
    })?;
    let db_path = project::db_path(&project_dir);
    let dir_str = project_dir.to_string_lossy();
    let db = db::Database::open(&db_path)?;
    let stats = indexer::index_directory(&db, &dir_str, full)?;
    println!("{stats}");
    let resolve = resolver::resolve_references(&db)?;
    println!("{resolve}");
    Ok(())
}

fn cmd_symbol(
    path: Option<&str>,
    name: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let symbols = query::find_symbols(&db, name, kind, file)?;
    let json = serde_json::to_string_pretty(&symbols)?;
    println!("{json}");
    Ok(())
}

fn cmd_callers(path: Option<&str>, name: &str, file: Option<&str>, depth: u32) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let callers = query::find_callers(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&callers)?;
    println!("{json}");
    Ok(())
}

fn cmd_callees(path: Option<&str>, name: &str, file: Option<&str>, depth: u32) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let callees = query::find_callees(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&callees)?;
    println!("{json}");
    Ok(())
}

fn cmd_dead_code(path: Option<&str>, exclude: &[String]) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let dead = query::find_dead_code(&db, None, exclude)?;
    let json = serde_json::to_string_pretty(&dead)?;
    println!("{json}");
    Ok(())
}

fn cmd_hierarchy(path: Option<&str>, name: &str, direction: &str) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let entries = query::find_hierarchy(&db, name, direction)?;
    let json = serde_json::to_string_pretty(&entries)?;
    println!("{json}");
    Ok(())
}

fn cmd_references(path: Option<&str>, name: &str, kind: Option<&str>) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let refs = query::find_references(&db, name, kind)?;
    let json = serde_json::to_string_pretty(&refs)?;
    println!("{json}");
    Ok(())
}

fn cmd_tested_by(path: Option<&str>, name: &str, file: Option<&str>, depth: u32) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let tests = query::find_tested_by(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&tests)?;
    println!("{json}");
    Ok(())
}

fn cmd_untested(path: Option<&str>, exclude: &[String]) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let untested = query::find_untested(&db, None, exclude)?;
    let json = serde_json::to_string_pretty(&untested)?;
    println!("{json}");
    Ok(())
}

fn cmd_imported_by(path: Option<&str>, name: &str, file: Option<&str>) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let entries = query::find_imported_by(&db, name, file)?;
    let json = serde_json::to_string_pretty(&entries)?;
    println!("{json}");
    Ok(())
}

fn cmd_resolve_import(path: Option<&str>, name: &str, file: Option<&str>) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let imports = query::resolve_import(&db, name, file)?;
    let json = serde_json::to_string_pretty(&imports)?;
    println!("{json}");
    Ok(())
}

fn cmd_list(path: Option<&str>, kind: Option<&str>, file: Option<&str>) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let symbols = query::list_symbols(&db, kind, file)?;
    let json = serde_json::to_string(&symbols)?;
    println!("{json}");
    Ok(())
}

fn cmd_watch(path: Option<&str>) -> Result<()> {
    let project_dir = project::resolve_project_dir(path).or_else(|_| {
        let p = path.unwrap_or(".");
        std::path::Path::new(p)
            .canonicalize()
            .map_err(anyhow::Error::from)
    })?;
    let db_path = project::db_path(&project_dir);
    let dir_str = project_dir.to_string_lossy();
    watcher::watch(&db_path, &dir_str)
}

fn cmd_status(path: Option<&str>) -> Result<()> {
    let db = db::Database::open(&project::resolve_db(path)?)?;
    let (files, symbols, refs) = db.get_stats()?;
    println!("Files: {files}, Symbols: {symbols}, References: {refs}");
    Ok(())
}

fn cmd_project(action: ProjectAction) -> Result<()> {
    run_project_action(action)
}

fn run_project_action(action: ProjectAction) -> Result<()> {
    match action {
        ProjectAction::Add { name, path } => cmd_project_add(&name, path)?,
        ProjectAction::Remove { name } => cmd_project_remove(&name)?,
        ProjectAction::List => cmd_project_list()?,
    }
    Ok(())
}

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

fn cmd_project_remove(name: &str) -> Result<()> {
    let removed = config::remove_project(name)?;
    print_project_remove_result(name, removed);
    Ok(())
}

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

fn cmd_project_list() -> Result<()> {
    let config = config::load()?;
    print_project_entries(&config.projects);
    Ok(())
}

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
