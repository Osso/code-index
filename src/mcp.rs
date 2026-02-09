use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::db::Database;
use crate::indexer;
use crate::query;
use crate::resolver;

#[derive(Clone)]
pub struct CodeIndexService {
    tool_router: ToolRouter<Self>,
}

impl CodeIndexService {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    fn open_db(&self) -> Result<Database, String> {
        let db_path = crate::project::resolve_db(None).map_err(|e| format!("Project error: {e}"))?;
        Database::open(&db_path).map_err(|e| format!("DB error: {e}"))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IndexParams {
    #[schemars(description = "Directory path to index")]
    path: String,
    #[schemars(description = "Force full re-index (ignore file hashes)")]
    full: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SymbolParams {
    #[schemars(description = "Symbol name to search for")]
    name: String,
    #[schemars(description = "Filter by kind: function, method, class, trait, interface, struct, enum")]
    kind: Option<String>,
    #[schemars(description = "Filter by file path (substring match)")]
    file: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CallersParams {
    #[schemars(description = "Function/method name to find callers of")]
    name: String,
    #[schemars(description = "Filter by file path (substring match)")]
    file: Option<String>,
    #[schemars(description = "Max call depth to traverse (default: 1)")]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CalleesParams {
    #[schemars(description = "Function/method name to find callees of")]
    name: String,
    #[schemars(description = "Filter by file path (substring match)")]
    file: Option<String>,
    #[schemars(description = "Max call depth to traverse (default: 1)")]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReferencesParams {
    #[schemars(description = "Symbol name to find references to")]
    name: String,
    #[schemars(description = "Filter by ref kind: call, inherit, implement, import, trait_impl")]
    kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HierarchyParams {
    #[schemars(description = "Class/trait/interface name")]
    name: String,
    #[schemars(description = "Direction: ancestors, descendants, or both (default: both)")]
    direction: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TestedByParams {
    #[schemars(description = "Function/method name to find tests for")]
    name: String,
    #[schemars(description = "Filter by file path (substring match)")]
    file: Option<String>,
    #[schemars(description = "Max call chain depth to search (default: 10)")]
    depth: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UntestedParams {
    #[schemars(description = "Filter by path (substring match)")]
    path: Option<String>,
    #[schemars(description = "Symbol names to exclude")]
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveImportParams {
    #[schemars(description = "Import name or path to resolve")]
    name: String,
    #[schemars(description = "Filter by source file path (substring match)")]
    file: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImportedByParams {
    #[schemars(description = "Module path or symbol name to find importers of (substring match on import path)")]
    name: String,
    #[schemars(description = "Filter by importing file path (substring match)")]
    file: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeadCodeParams {
    #[schemars(description = "Scope to directory path (substring match)")]
    path: Option<String>,
    #[schemars(description = "Function names to exclude from dead code detection")]
    exclude: Option<Vec<String>>,
}

#[tool_router]
impl CodeIndexService {
    #[tool(description = "Index/re-index a directory for structural code analysis")]
    async fn index(&self, Parameters(p): Parameters<IndexParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        match indexer::index_directory(&db, &p.path, p.full.unwrap_or(false)) {
            Ok(result) => {
                let resolve = resolver::resolve_references(&db);
                let resolve_msg = match resolve {
                    Ok(r) => format!("\n{}", r),
                    Err(e) => format!("\nResolution failed: {}", e),
                };
                format!("{}{}", result, resolve_msg)
            }
            Err(e) => format!("Indexing failed: {}", e),
        }
    }

    #[tool(description = "Find symbol definition(s) by name")]
    async fn symbol(&self, Parameters(p): Parameters<SymbolParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        match query::find_symbols(&db, &p.name, p.kind.as_deref(), p.file.as_deref()) {
            Ok(symbols) => format_symbols(&symbols),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find who calls a function/method")]
    async fn callers(&self, Parameters(p): Parameters<CallersParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let depth = p.depth.unwrap_or(1);
        match query::find_callers(&db, &p.name, p.file.as_deref(), depth) {
            Ok(callers) => format_call_infos(&callers, "callers"),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find what a function/method calls")]
    async fn callees(&self, Parameters(p): Parameters<CalleesParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let depth = p.depth.unwrap_or(1);
        match query::find_callees(&db, &p.name, p.file.as_deref(), depth) {
            Ok(callees) => format_call_infos(&callees, "callees"),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find all structural references to a symbol")]
    async fn references(&self, Parameters(p): Parameters<ReferencesParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        match query::find_references(&db, &p.name, p.kind.as_deref()) {
            Ok(refs) => format_references(&refs),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find class/trait/interface inheritance hierarchy")]
    async fn hierarchy(&self, Parameters(p): Parameters<HierarchyParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let dir = p.direction.as_deref().unwrap_or("both");
        match query::find_hierarchy(&db, &p.name, dir) {
            Ok(entries) => format_hierarchy(&entries),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Resolve an import path to its target file and symbol definition")]
    async fn resolve_import(&self, Parameters(p): Parameters<ResolveImportParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        match query::resolve_import(&db, &p.name, p.file.as_deref()) {
            Ok(imports) => format_resolved_imports(&imports),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find which test functions call a given symbol (directly or transitively)")]
    async fn tested_by(&self, Parameters(p): Parameters<TestedByParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let depth = p.depth.unwrap_or(10);
        match query::find_tested_by(&db, &p.name, p.file.as_deref(), depth) {
            Ok(tests) => format_tested_by(&tests),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find functions/methods not covered by any test")]
    async fn untested(&self, Parameters(p): Parameters<UntestedParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let exclude = p.exclude.unwrap_or_default();
        match query::find_untested(&db, p.path.as_deref(), &exclude) {
            Ok(symbols) => format_untested(&symbols),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find files that import a given module/symbol (reverse dependency lookup)")]
    async fn imported_by(&self, Parameters(p): Parameters<ImportedByParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        match query::find_imported_by(&db, &p.name, p.file.as_deref()) {
            Ok(entries) => format_imported_by(&entries),
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Find functions/methods that are never called (dead code)")]
    async fn dead_code(&self, Parameters(p): Parameters<DeadCodeParams>) -> String {
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return e,
        };
        let exclude = p.exclude.unwrap_or_default();
        match query::find_dead_code(&db, p.path.as_deref(), &exclude) {
            Ok(symbols) => format_dead_code(&symbols),
            Err(e) => format!("Error: {}", e),
        }
    }
}

fn format_symbols(symbols: &[crate::model::StoredSymbol]) -> String {
    if symbols.is_empty() {
        return "No symbols found.".to_string();
    }
    symbols
        .iter()
        .map(|s| {
            let sig = s.signature.as_deref().unwrap_or("");
            let vis = s.visibility.as_deref().unwrap_or("");
            format!("{} {} {} {}:{}-{}", vis, s.kind, s.name, s.file_path, s.line_start, s.line_end)
                .trim().to_string()
                + if sig.is_empty() { "".to_string() } else { format!(" | {}", sig) }.as_str()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_call_infos(infos: &[crate::model::CallInfo], label: &str) -> String {
    if infos.is_empty() {
        return format!("No {} found.", label);
    }
    let header = format!("{} {} found:\n", infos.len(), label);
    let lines: Vec<String> = infos
        .iter()
        .map(|c| format!("  {} in {}:{}", c.symbol_name, c.file_path, c.line))
        .collect();
    header + &lines.join("\n")
}

fn format_references(refs: &[crate::model::StoredReference]) -> String {
    if refs.is_empty() {
        return "No references found.".to_string();
    }
    refs.iter()
        .map(|r| {
            let source = r.source_symbol.as_deref().unwrap_or("<file-level>");
            let resolved = if r.resolved { " [resolved]" } else { "" };
            format!("  [{}] {} in {}:{}{}", r.kind, source, r.source_file, r.line, resolved)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_hierarchy(entries: &[crate::model::HierarchyEntry]) -> String {
    if entries.is_empty() {
        return "No inheritance found.".to_string();
    }
    entries
        .iter()
        .map(|e| {
            let indent = "  ".repeat(e.depth as usize);
            format!("{}{} ({}) in {} | {}", indent, e.name, e.kind, e.file_path, e.relation)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_resolved_imports(imports: &[crate::model::ResolvedImport]) -> String {
    if imports.is_empty() {
        return "No imports found.".to_string();
    }
    imports
        .iter()
        .map(|i| {
            let target = match (&i.target_file, &i.target_symbol, &i.target_line) {
                (Some(f), Some(s), Some(l)) => format!(" → {} {} in {}:{}", i.target_kind.as_deref().unwrap_or("?"), s, f, l),
                _ => " → [unresolved]".to_string(),
            };
            let alias = i.alias.as_ref().map(|a| format!(" as {a}")).unwrap_or_default();
            format!("{}:{} {} ({}){}{}", i.source_file, i.line, i.local_name, i.full_path, alias, target)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_tested_by(tests: &[crate::model::StoredSymbol]) -> String {
    if tests.is_empty() {
        return "No tests found for this symbol.".to_string();
    }
    let header = format!("{} test(s) found:\n", tests.len());
    let lines: Vec<String> = tests
        .iter()
        .map(|s| format!("  {} {} in {}:{}", s.kind, s.name, s.file_path, s.line_start))
        .collect();
    header + &lines.join("\n")
}

fn format_untested(symbols: &[crate::model::StoredSymbol]) -> String {
    if symbols.is_empty() {
        return "All functions/methods are tested.".to_string();
    }
    let header = format!("{} untested functions/methods:\n", symbols.len());
    let lines: Vec<String> = symbols
        .iter()
        .map(|s| format!("  {} {} in {}:{}", s.kind, s.name, s.file_path, s.line_start))
        .collect();
    header + &lines.join("\n")
}

fn format_imported_by(entries: &[crate::model::ImportedByEntry]) -> String {
    if entries.is_empty() {
        return "No importers found.".to_string();
    }
    let header = format!("{} importer(s) found:\n", entries.len());
    let lines: Vec<String> = entries
        .iter()
        .map(|e| {
            let alias = e.alias.as_ref().map(|a| format!(" as {a}")).unwrap_or_default();
            format!("  {}:{} {} ({}){}", e.file_path, e.line, e.local_name, e.full_path, alias)
        })
        .collect();
    header + &lines.join("\n")
}

fn format_dead_code(symbols: &[crate::model::StoredSymbol]) -> String {
    if symbols.is_empty() {
        return "No dead code found.".to_string();
    }
    let header = format!("{} potentially unused functions:\n", symbols.len());
    let lines: Vec<String> = symbols
        .iter()
        .map(|s| format!("  {} {} in {}:{}", s.kind, s.name, s.file_path, s.line_start))
        .collect();
    header + &lines.join("\n")
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodeIndexService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "Structural code analysis MCP server. Index codebases and query symbols, callers, callees, hierarchy, and dead code."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
