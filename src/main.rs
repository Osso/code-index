#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

mod config;
mod db;
mod indexer;
mod main_project;
#[cfg(test)]
mod main_tests;
mod mcp;
mod model;
mod parser;
mod project;
mod query;
mod resolver;
#[cfg(test)]
mod test_support;
mod watcher;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use ast_outline::core::{DigestOptions, OutlineOptions};
use clap::{Parser, Subcommand};
use main_project::cmd_project;

#[derive(Parser)]
#[command(name = "code-index", about = "Structural code analysis MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

struct OutlineCommandOptions<'a> {
    paths: &'a [PathBuf],
    digest: bool,
    no_private: bool,
    no_fields: bool,
    no_docs: bool,
    no_attrs: bool,
    no_lines: bool,
    glob: Option<&'a str>,
    show: &'a [String],
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
        /// Also print outlines for the definition and caller files
        #[arg(long)]
        outline: bool,
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
        /// Filter by symbol kind (function, method, class, trait, interface, struct, enum, property, event)
        #[arg(long)]
        kind: Option<String>,
        /// Filter by file path (substring match)
        #[arg(long)]
        file: Option<String>,
        /// Override project path
        #[arg(long)]
        path: Option<String>,
    },
    /// Print structural outlines for files or directories
    Outline {
        /// Files or directories to outline
        #[arg(num_args = 1..)]
        paths: Vec<PathBuf>,
        /// Print a compact per-directory digest
        #[arg(long)]
        digest: bool,
        #[arg(long)]
        no_private: bool,
        #[arg(long)]
        no_fields: bool,
        #[arg(long)]
        no_docs: bool,
        #[arg(long)]
        no_attrs: bool,
        #[arg(long)]
        no_lines: bool,
        #[arg(long)]
        glob: Option<String>,
        /// Extract source for a symbol; can be passed multiple times
        #[arg(long = "show")]
        show: Vec<String>,
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    dispatch(cli.command)
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch(command: Command) -> Result<()> {
    match command {
        command @ Command::Serve
        | command @ Command::Index { .. }
        | command @ Command::Project { .. } => dispatch_core_command(command)?,
        command @ Command::Symbol { .. }
        | command @ Command::Callers { .. }
        | command @ Command::Callees { .. }
        | command @ Command::References { .. }
        | command @ Command::DeadCode { .. }
        | command @ Command::Hierarchy { .. }
        | command @ Command::TestedBy { .. }
        | command @ Command::Untested { .. } => dispatch_analysis_command(command)?,
        command @ Command::ImportedBy { .. }
        | command @ Command::ResolveImport { .. }
        | command @ Command::List { .. }
        | command @ Command::Outline { .. }
        | command @ Command::Watch { .. }
        | command @ Command::Status { .. } => dispatch_utility_command(command)?,
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_core_command(command: Command) -> Result<()> {
    match command {
        Command::Serve => run_mcp_server()?,
        Command::Index { path, full } => cmd_index(path.as_deref(), full)?,
        Command::Project { action } => cmd_project(action)?,
        _ => unreachable!("non-core command routed to dispatch_core_command"),
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_analysis_command(command: Command) -> Result<()> {
    match command {
        command @ Command::Symbol { .. }
        | command @ Command::Callers { .. }
        | command @ Command::Callees { .. }
        | command @ Command::References { .. } => dispatch_symbol_call_command(command),
        command @ Command::DeadCode { .. }
        | command @ Command::Hierarchy { .. }
        | command @ Command::TestedBy { .. }
        | command @ Command::Untested { .. } => dispatch_quality_command(command),
        _ => unreachable!("non-analysis command routed to dispatch_analysis_command"),
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_symbol_call_command(command: Command) -> Result<()> {
    match command {
        Command::Symbol {
            name,
            kind,
            file,
            path,
        } => cmd_symbol(path.as_deref(), &name, kind.as_deref(), file.as_deref())?,
        Command::Callers {
            name,
            file,
            depth,
            outline,
            path,
        } => cmd_callers(path.as_deref(), &name, file.as_deref(), depth, outline)?,
        Command::Callees {
            name,
            file,
            depth,
            path,
        } => cmd_callees(path.as_deref(), &name, file.as_deref(), depth)?,
        Command::References { name, kind, path } => {
            cmd_references(path.as_deref(), &name, kind.as_deref())?
        }
        _ => unreachable!("non-symbol/call command routed to dispatch_symbol_call_command"),
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_quality_command(command: Command) -> Result<()> {
    match command {
        Command::DeadCode { exclude, path } => cmd_dead_code(path.as_deref(), &exclude)?,
        Command::Hierarchy {
            name,
            direction,
            path,
        } => cmd_hierarchy(path.as_deref(), &name, &direction)?,
        Command::TestedBy {
            name,
            file,
            depth,
            path,
        } => cmd_tested_by(path.as_deref(), &name, file.as_deref(), depth)?,
        Command::Untested { exclude, path } => cmd_untested(path.as_deref(), &exclude)?,
        _ => unreachable!("non-quality command routed to dispatch_quality_command"),
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_utility_command(command: Command) -> Result<()> {
    match command {
        Command::ImportedBy { name, file, path } => {
            cmd_imported_by(path.as_deref(), &name, file.as_deref())?
        }
        Command::ResolveImport { name, file, path } => {
            cmd_resolve_import(path.as_deref(), &name, file.as_deref())?
        }
        Command::List { kind, file, path } => {
            cmd_list(path.as_deref(), kind.as_deref(), file.as_deref())?
        }
        Command::Outline { .. } => dispatch_outline_command(command)?,
        Command::Watch { path } => cmd_watch(path.as_deref())?,
        Command::Status { path } => cmd_status(path.as_deref())?,
        _ => unreachable!("non-utility command routed to dispatch_utility_command"),
    }
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn dispatch_outline_command(command: Command) -> Result<()> {
    if let Command::Outline {
        paths,
        digest,
        no_private,
        no_fields,
        no_docs,
        no_attrs,
        no_lines,
        glob,
        show,
    } = command
    {
        return cmd_outline(OutlineCommandOptions {
            paths: &paths,
            digest,
            no_private,
            no_fields,
            no_docs,
            no_attrs,
            no_lines,
            glob: glob.as_deref(),
            show: &show,
        });
    }
    unreachable!("non-outline command routed to dispatch_outline_command")
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_symbol(
    path: Option<&str>,
    name: &str,
    kind: Option<&str>,
    file: Option<&str>,
) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let symbols = query::find_symbols(&db, name, kind, file)?;
    let json = serde_json::to_string_pretty(&symbols)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_callers(
    path: Option<&str>,
    name: &str,
    file: Option<&str>,
    depth: u32,
    outline: bool,
) -> Result<()> {
    let (project_dir, db) = open_refreshed_database(path)?;
    let callers = query::find_callers(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&callers)?;
    println!("{json}");
    if outline {
        let definitions = query::find_symbols(&db, name, None, file)?;
        let outline_files = build_outline_file_args(&project_dir, &definitions, &callers);
        run_outline(&outline_files)?;
    }
    Ok(())
}

fn open_refreshed_database(path: Option<&str>) -> Result<(PathBuf, db::Database)> {
    let project_dir = project::resolve_project_dir(path)?;
    let db = db::Database::open(&project::db_path(&project_dir))?;
    refresh_project_index(&db, &project_dir)?;
    Ok((project_dir, db))
}

/// How often a read query is allowed to re-scan the project for changes.
/// Between checks, queries trust the existing index rather than re-walking the
/// whole tree and re-resolving every reference (expensive on large repos).
const REFRESH_INTERVAL_SECS: u64 = 3600;
const LAST_REFRESH_KEY: &str = "last_refresh";

fn refresh_project_index(db: &db::Database, project_dir: &Path) -> Result<()> {
    let now = unix_now();
    if !refresh_due(db, now)? {
        return Ok(());
    }

    let dir_str = project_dir.to_string_lossy();
    let stats = indexer::index_directory(db, &dir_str, false)?;
    // Only the resolution pass is costly on large repos, and it is pointless
    // when no file changed — the prior resolution is still valid.
    if stats.changed_graph() {
        resolver::resolve_references(db)?;
    }

    db.set_meta(LAST_REFRESH_KEY, &now.to_string())?;
    Ok(())
}

/// True when no freshness check has run within REFRESH_INTERVAL_SECS.
fn refresh_due(db: &db::Database, now: u64) -> Result<bool> {
    let last = db
        .get_meta(LAST_REFRESH_KEY)?
        .and_then(|v| v.parse::<u64>().ok());
    Ok(match last {
        Some(last) => now.saturating_sub(last) >= REFRESH_INTERVAL_SECS,
        None => true,
    })
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_outline_file_args(
    project_dir: &Path,
    definitions: &[model::StoredSymbol],
    callers: &[model::CallInfo],
) -> Vec<String> {
    let mut files = BTreeSet::new();
    for file_path in definitions
        .iter()
        .map(|symbol| symbol.file_path.as_str())
        .chain(callers.iter().map(|caller| caller.file_path.as_str()))
    {
        files.insert(resolve_outline_file(project_dir, file_path));
    }
    files
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn resolve_outline_file(project_dir: &Path, file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_dir.join(path)
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_outline(options: OutlineCommandOptions<'_>) -> Result<()> {
    let results = ast_outline::walk_and_parse(options.paths, options.glob);
    if !options.show.is_empty() {
        print_outline_symbol_matches(&results, options.show);
        return Ok(());
    }

    if options.digest {
        let digest_options = DigestOptions {
            include_private: !options.no_private,
            include_fields: !options.no_fields,
            max_members_per_type: 50,
            max_heading_depth: 3,
        };
        let root = if options.paths.len() == 1 && options.paths[0].is_dir() {
            Some(options.paths[0].as_path())
        } else {
            None
        };
        println!(
            "{}",
            ast_outline::core::render_digest(&results, &digest_options, root)
        );
        return Ok(());
    }

    let outline_options = OutlineOptions {
        include_private: !options.no_private,
        include_fields: !options.no_fields,
        include_xml_doc: !options.no_docs,
        include_attributes: !options.no_attrs,
        include_line_numbers: !options.no_lines,
        max_doc_lines: 6,
    };
    render_outline_results(&results, &outline_options);
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn print_outline_symbol_matches(results: &[ast_outline::core::ParseResult], symbols: &[String]) {
    for result in results {
        for symbol in symbols {
            print_outline_matches_for_symbol(result, symbol);
        }
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn print_outline_matches_for_symbol(result: &ast_outline::core::ParseResult, symbol: &str) {
    for symbol_match in ast_outline::core::find_symbols(result, symbol) {
        println!(
            "# {}:{}-{} {} ({})",
            result.path.display(),
            symbol_match.start_line,
            symbol_match.end_line,
            symbol_match.qualified_name,
            symbol_match.kind
        );
        if !symbol_match.ancestor_signatures.is_empty() {
            println!("# in: {}", symbol_match.ancestor_signatures.join(" -> "));
        }
        println!("{}", symbol_match.source);
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn render_outline_results(results: &[ast_outline::core::ParseResult], options: &OutlineOptions) {
    for result in results {
        println!("{}", ast_outline::core::render_outline(result, options));
        println!();
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn run_outline(files: &[String]) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }

    println!("\n--- outline ---");
    let options = OutlineOptions::default();
    for file in files {
        let path = Path::new(file);
        if let Some(result) = ast_outline::parse_file(path) {
            render_outline_results(&[result], &options);
        }
    }

    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_callees(path: Option<&str>, name: &str, file: Option<&str>, depth: u32) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let callees = query::find_callees(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&callees)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_dead_code(path: Option<&str>, exclude: &[String]) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let dead = query::find_dead_code(&db, None, exclude)?;
    let json = serde_json::to_string_pretty(&dead)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_hierarchy(path: Option<&str>, name: &str, direction: &str) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let entries = query::find_hierarchy(&db, name, direction)?;
    let json = serde_json::to_string_pretty(&entries)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_references(path: Option<&str>, name: &str, kind: Option<&str>) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let refs = query::find_references(&db, name, kind)?;
    let json = serde_json::to_string_pretty(&refs)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_tested_by(path: Option<&str>, name: &str, file: Option<&str>, depth: u32) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let tests = query::find_tested_by(&db, name, file, depth)?;
    let json = serde_json::to_string_pretty(&tests)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_untested(path: Option<&str>, exclude: &[String]) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let untested = query::find_untested(&db, None, exclude)?;
    let json = serde_json::to_string_pretty(&untested)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_imported_by(path: Option<&str>, name: &str, file: Option<&str>) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let entries = query::find_imported_by(&db, name, file)?;
    let json = serde_json::to_string_pretty(&entries)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_resolve_import(path: Option<&str>, name: &str, file: Option<&str>) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let imports = query::resolve_import(&db, name, file)?;
    let json = serde_json::to_string_pretty(&imports)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_list(path: Option<&str>, kind: Option<&str>, file: Option<&str>) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let symbols = query::list_symbols(&db, kind, file)?;
    let json = serde_json::to_string(&symbols)?;
    println!("{json}");
    Ok(())
}

#[cfg_attr(coverage_nightly, coverage(off))]
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

#[cfg_attr(coverage_nightly, coverage(off))]
fn cmd_status(path: Option<&str>) -> Result<()> {
    let (_project_dir, db) = open_refreshed_database(path)?;
    let (files, symbols, refs) = db.get_stats()?;
    println!("Files: {files}, Symbols: {symbols}, References: {refs}");
    Ok(())
}
