use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Php,
    Rust,
    Python,
    TypeScript,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "php" => Some(Self::Php),
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "ts" | "tsx" => Some(Self::TypeScript),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Php => "php",
            Self::Rust => "rust",
            Self::Python => "python",
            Self::TypeScript => "typescript",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Trait,
    Interface,
    Struct,
    Enum,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Struct => "struct",
            Self::Enum => "enum",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "class" => Some(Self::Class),
            "trait" => Some(Self::Trait),
            "interface" => Some(Self::Interface),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RefKind {
    Call,
    Inherit,
    Implement,
    Import,
    TraitImpl,
}

impl RefKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Inherit => "inherit",
            Self::Implement => "implement",
            Self::Import => "import",
            Self::TraitImpl => "trait_impl",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "call" => Some(Self::Call),
            "inherit" => Some(Self::Inherit),
            "implement" => Some(Self::Implement),
            "import" => Some(Self::Import),
            "trait_impl" => Some(Self::TraitImpl),
            _ => None,
        }
    }
}

/// A parsed symbol definition (function, class, etc.)
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line_start: usize,
    pub line_end: usize,
    pub parent_name: Option<String>,
    pub visibility: Option<String>,
    pub signature: Option<String>,
}

/// A parsed reference (call, inheritance, import usage)
#[derive(Debug, Clone)]
pub struct Reference {
    pub kind: RefKind,
    pub target_name: String,
    pub target_qualifier: Option<String>,
    pub line: usize,
    /// Name of the enclosing symbol, if any
    pub source_symbol_name: Option<String>,
}

/// A parsed import statement
#[derive(Debug, Clone)]
pub struct Import {
    pub local_name: String,
    pub full_path: String,
    pub alias: Option<String>,
    pub line: usize,
}

/// Result of parsing a single file
#[derive(Debug, Clone)]
pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub references: Vec<Reference>,
    pub imports: Vec<Import>,
}

/// A stored file record
#[derive(Debug, Clone, Serialize)]
pub struct FileInfo {
    pub id: i64,
    pub path: String,
    pub hash: String,
    pub lang: String,
    pub indexed_at: i64,
}

/// A stored symbol with its database ID and file info
#[derive(Debug, Clone, Serialize)]
pub struct StoredSymbol {
    pub id: i64,
    pub file_path: String,
    pub name: String,
    pub kind: String,
    pub line_start: i64,
    pub line_end: i64,
    pub visibility: Option<String>,
    pub signature: Option<String>,
}

/// A stored reference with context
#[derive(Debug, Clone, Serialize)]
pub struct StoredReference {
    pub source_file: String,
    pub source_symbol: Option<String>,
    pub target_name: String,
    pub target_qualifier: Option<String>,
    pub kind: String,
    pub line: i64,
    pub resolved: bool,
    pub target_file: Option<String>,
    pub target_symbol: Option<String>,
}

/// Caller/callee info for call graph queries
#[derive(Debug, Clone, Serialize)]
pub struct CallInfo {
    pub symbol_name: String,
    pub file_path: String,
    pub line: i64,
    pub kind: String,
}

/// Hierarchy entry for class/trait inheritance
#[derive(Debug, Clone, Serialize)]
pub struct HierarchyEntry {
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub relation: String,
    pub depth: i32,
}

/// Resolved import: maps an import path to its target file and symbol
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedImport {
    /// File containing the import statement
    pub source_file: String,
    /// Local name used in the importing file
    pub local_name: String,
    /// Full import path as written
    pub full_path: String,
    /// Alias if any
    pub alias: Option<String>,
    /// Line of the import statement
    pub line: i64,
    /// Resolved target file path (if found)
    pub target_file: Option<String>,
    /// Resolved target symbol name (if found)
    pub target_symbol: Option<String>,
    /// Resolved target symbol kind (if found)
    pub target_kind: Option<String>,
    /// Resolved target line (if found)
    pub target_line: Option<i64>,
}
