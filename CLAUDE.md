# code-index

Structural code analysis tool using tree-sitter AST parsing. Indexes symbols, call graphs, imports, and inheritance into per-project SQLite databases.

## Architecture

- `src/parser/` - Language-specific tree-sitter parsers (PHP, Rust, Python, TypeScript)
- `src/indexer.rs` - File walker + parser orchestrator, writes to DB
- `src/db.rs` - SQLite schema and CRUD operations
- `src/resolver.rs` - Post-indexing reference resolution (links refs to symbol IDs)
- `src/query.rs` - All query functions (callers, callees, dead-code, hierarchy, etc.)
- `src/mcp.rs` - MCP server exposing queries as tools
- `src/main.rs` - CLI entry point
- `src/model.rs` - Shared types (Symbol, Reference, Import, query result structs)
- `src/project.rs` - Project directory and DB path resolution
- `src/config.rs` - Project registry (~/.config/code-index/config.toml)
- `src/watcher.rs` - File watcher for incremental re-indexing

## Commands

symbol, callers, callees, references, hierarchy, tested-by, untested, dead-code, imported-by, resolve-import, index, watch, status, project

## Testing

```bash
cargo test
```

## Adding a New Query

1. Add result struct in `model.rs` (derive `Serialize`)
2. Add query function in `query.rs`
3. Add CLI command variant in `main.rs` (`Command` enum + `dispatch` match + `cmd_` handler)
4. Add MCP tool in `mcp.rs` (params struct + tool method + formatter)
5. Update skill file: `~/.claude/skills/code-index/SKILL.md`
