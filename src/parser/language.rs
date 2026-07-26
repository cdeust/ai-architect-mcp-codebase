// parser::language — the Language enum and its extension/string detection.
// Split from parser/mod.rs to keep files under the 300-line limit. Pure move.

// ---------------------------------------------------------------------------
// Supported languages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    Java,
    Kotlin,
    Swift,
    ObjC,
    C,
    Cpp,
    Go,
    /// Shallow-path language (ADR-0056): node-kind rows only, no deep
    /// extraction. See `spec::ruby`.
    Ruby,
}

impl Language {
    /// Detects language from file extension. Returns None for unsupported.
    ///
    /// Ambiguity policy — a few extensions do not map 1:1 to a grammar. Each is
    /// resolved to the most common case below; when a file is mapped to a
    /// grammar that doesn't fit it, tree-sitter recovers into ERROR/MISSING
    /// nodes and `ParseResult.parse_errors` (see stage-3.md §10.5) becomes
    /// non-zero — so a mis-detection is observable rather than silent. Callers
    /// that know better can override via `from_str_opt` (the tool's `language:`
    /// arg). The specific rulings:
    ///   - `.h`  → C. Ambiguous across C / C++ / Objective-C headers; C is the
    ///     majority case and C constructs also parse under the C++ grammar. A
    ///     C++-only header (templates, namespaces) will show parse_errors; pass
    ///     `language: "cpp"` for such trees.
    ///   - `.mm` → ObjC. This is Objective-C++; the Objective-C grammar covers
    ///     the ObjC constructs and embedded C, but NOT full C++ (templates,
    ///     namespaces) — those portions become parse_errors. Full fidelity would
    ///     need a dedicated ObjC++ grammar, which tree-sitter has no crate for.
    ///   - `.js/.jsx/.mjs/.cjs` → TypeScript enum, parsed with the TSX grammar
    ///     dialect (see parser::typescript). JS is a syntactic subset of TSX
    ///     (minus type annotations, which JS files don't use), so functions /
    ///     classes / JSX all extract cleanly. source: viz "AST regardless of
    ///     file type" requirement, 2026-06-03.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Language::Rust),
            "py" => Some(Language::Python),
            "ts" | "tsx" => Some(Language::TypeScript),
            "java" => Some(Language::Java),
            "kt" | "kts" => Some(Language::Kotlin),
            "swift" => Some(Language::Swift),
            "m" | "mm" => Some(Language::ObjC),
            "c" | "h" => Some(Language::C),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(Language::Cpp),
            "go" => Some(Language::Go),
            "rb" => Some(Language::Ruby),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::TypeScript),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::Java => "java",
            Language::Kotlin => "kotlin",
            Language::Swift => "swift",
            Language::ObjC => "objc",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Ruby => "ruby",
        }
    }

    /// Parses a language string from the tool schema.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "rust" => Some(Language::Rust),
            "python" => Some(Language::Python),
            "typescript" => Some(Language::TypeScript),
            "java" => Some(Language::Java),
            "kotlin" => Some(Language::Kotlin),
            "swift" => Some(Language::Swift),
            "objc" | "objective-c" => Some(Language::ObjC),
            "c" => Some(Language::C),
            "cpp" | "c++" => Some(Language::Cpp),
            "go" => Some(Language::Go),
            "ruby" => Some(Language::Ruby),
            _ => None,
        }
    }
}
