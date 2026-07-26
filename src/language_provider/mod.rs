// language_provider — per-language resolution conventions for Stage 3b.
//
// The resolver's PHASE logic (imports/calls/implements/extends/uses) is
// language-agnostic: it operates on graph node LABELS (Import, CallSite,
// Method, Struct, …) that every parser emits uniformly, and on qualified
// names that every parser normalizes to `::` (verified: parser/{rust,go,
// java,kotlin,swift,objc,c,cpp,typescript,python}.rs all build QNs with
// `::`). What DIVERGES per language is a small set of surface conventions:
//
//   1. file extensions          (file-id extraction from node ids/QNs)
//   2. import-path separator     — as STORED after parsing, not as written in
//                                 source. The live parsers (Rust/Python/TS)
//                                 normalize to `::`; the dormant parsers keep
//                                 raw separators (`.` Java/Kotlin; `/` Go/
//                                 ObjC/C/C++; bare module name for Swift).
//   3. import-path prefix to strip (Rust `crate::`)
//   4. external stdlib/framework roots (skip-resolution set)
//   5. primitive/builtin type names    (skip in type-usage resolution)
//   6. derive/macro expansion table key (Rust derives only)
//
// This module isolates those six divergences behind a trait + registry so
// the seven parse-wired-but-resolution-dormant grammars (Java, Kotlin,
// Swift, ObjC, C, C++, Go) resolve cross-file edges with no change to the
// phase logic. Mirrors the existing macro_expansion::get_macro_table
// trait+registry pattern.
//
// source: stages/stage-3b.md §5 (phase contract); per-constant sources cited
// at each provider. Registered in BOTH lib.rs and main.rs (lib+bin crate).

mod kotlin_prefixes;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Per-language resolution conventions. Concrete impls are zero-sized unit
/// structs holding only `&'static` data; the registry returns shared refs.
pub trait LanguageProvider: Send + Sync {
    /// Canonical language tag (matches parser::Language::as_str). Used by the
    /// registry round-trip test and as the provider identity accessor.
    #[allow(dead_code)] // exercised by tests; identity contract for the registry.
    fn language(&self) -> &'static str;

    /// Separator used in this language's IMPORT PATHS — NOT in qualified
    /// names (QNs are uniformly `::` across every parser). Used to take the
    /// last path segment when matching an import/callee against the symbol
    /// index. Default `::`.
    fn import_separator(&self) -> &'static str {
        "::"
    }

    /// Strip a leading language-specific path prefix that is not part of any
    /// symbol's qualified name (e.g. Rust `crate::`). Borrow-only; default is
    /// identity. source: resolver.rs::normalize_import_path (Rust behavior).
    fn normalize_import_path<'a>(&self, path: &'a str) -> &'a str {
        path
    }

    /// Well-known standard-library / framework path roots that are NOT part
    /// of the indexed corpus. A reference whose first path segment matches is
    /// skipped (recorded as an "external" UnresolvedRef) instead of being
    /// matched against in-corpus symbols. Mirrors the existing Rust/Python/
    /// Node "common subset" lists — intentionally not exhaustive.
    fn external_prefixes(&self) -> &'static [&'static str];

    /// Primitive / builtin type names to skip during type-usage resolution
    /// (they are not corpus symbols). Only names that survive the uppercase-
    /// identifier convention in resolver::extract_type_identifiers need be
    /// listed; lowercase builtins (e.g. Go `int`, C `char`) are already
    /// skipped by that convention.
    fn primitives(&self) -> &'static [&'static str];

    /// macro_expansion table key for derive/decorator → trait expansion, if
    /// the language has one. `None` is the honest answer for languages with
    /// no such mechanism — we do not fabricate stdlib edges for them.
    fn derive_macro_key(&self) -> Option<&'static str> {
        None
    }

    /// Package/module grouping for a file id (PackageProximity evidence +
    /// package-keyed ImportMatch, issue #29). `None` is the honest default —
    /// returning `None` for a language is what preserves its pre-issue-#29
    /// resolution behavior unchanged (see call_evidence.rs). Override only
    /// when the directory is a reliable package/module proxy.
    fn package_of(&self, _file_id: &str) -> Option<String> {
        None
    }

    /// Last segment of an import path or callee, after normalization and
    /// separator split. Provided; do not override.
    fn import_last_segment<'a>(&self, path: &'a str) -> &'a str {
        let p = self.normalize_import_path(path);
        p.rsplit(self.import_separator()).next().unwrap_or(p)
    }

    /// pre: `path` uses this provider's `import_separator`. post: true iff
    /// `path` starts, segment-by-segment, with an `external_prefixes()`
    /// entry. A `.`-containing entry (e.g. `"com.google"`) matches as a
    /// compound root only when the separator is also `.` (Java/Kotlin);
    /// otherwise it matches atomically (preserves C's `"stdio.h"`).
    fn is_external_import(&self, path: &str) -> bool {
        let sep = self.import_separator();
        // crate/self/super and leading-dot relatives are always internal.
        let first = path.split(sep).next().unwrap_or(path);
        if first == "crate" || first == "self" || first == "super" {
            return false;
        }
        if first.starts_with('.') {
            return false;
        }
        let path_segments: Vec<&str> = path.split(sep).collect();
        self.external_prefixes().iter().any(|prefix| {
            if sep == "." && prefix.contains('.') {
                let prefix_segments: Vec<&str> = prefix.split('.').collect();
                path_segments.len() >= prefix_segments.len()
                    && path_segments[..prefix_segments.len()] == prefix_segments[..]
            } else {
                first == *prefix
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Default provider — conservative Rust-shaped fallback for unknown languages.
// Reproduces the resolver's historical hardcoded behavior so unknown/missing
// `language` columns never regress.
// ---------------------------------------------------------------------------

pub struct DefaultProvider;
impl LanguageProvider for DefaultProvider {
    fn language(&self) -> &'static str {
        "unknown"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        &[]
    }
    fn primitives(&self) -> &'static [&'static str] {
        RUST_PRIMITIVES
    }
}

// ---------------------------------------------------------------------------
// Rust — source: doc.rust-lang.org/std (std/core/alloc); Rust Reference
// "Primitive Types"; resolver.rs historical constants.
// ---------------------------------------------------------------------------

/// source: Rust Reference, "Primitive Types" + common std collections
/// (resolver.rs PRIMITIVES, preserved verbatim).
const RUST_PRIMITIVES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "char", "str", "String", "Vec", "Option", "Result", "Box", "Arc", "Rc",
    "HashMap", "HashSet", "BTreeMap", "BTreeSet", "Self", "self",
];

pub struct RustProvider;
impl LanguageProvider for RustProvider {
    fn language(&self) -> &'static str {
        "rust"
    }
    fn import_separator(&self) -> &'static str {
        "::"
    }
    fn normalize_import_path<'a>(&self, path: &'a str) -> &'a str {
        path.strip_prefix("crate::").unwrap_or(path)
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: resolver.rs is_external_crate (preserved): std lib crates +
        // crates this project depends on (Cargo.toml).
        &[
            "std",
            "core",
            "alloc",
            "serde",
            "serde_json",
            "sha2",
            "lbug",
            "tree_sitter",
            "tree_sitter_rust",
            "tree_sitter_python",
            "tree_sitter_typescript",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        RUST_PRIMITIVES
    }
    fn derive_macro_key(&self) -> Option<&'static str> {
        Some("rust")
    }
}

// ---------------------------------------------------------------------------
// Python — source: docs.python.org/3/py-modindex (stdlib, common subset,
// preserved from resolver.rs is_external_crate).
// ---------------------------------------------------------------------------

pub struct PythonProvider;
impl LanguageProvider for PythonProvider {
    fn language(&self) -> &'static str {
        "python"
    }
    fn import_separator(&self) -> &'static str {
        // The Python parser normalizes source `.` to `::` in stored import
        // paths (parser/python.rs `replace('.', "::")`), so the resolver sees
        // `::`-separated paths. source: parser/python.rs:285,339,344.
        "::"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        &[
            "os",
            "sys",
            "io",
            "re",
            "json",
            "typing",
            "collections",
            "pathlib",
            "functools",
            "itertools",
            "abc",
            "dataclasses",
            "logging",
            "unittest",
            "asyncio",
            "math",
            "datetime",
            "__future__",
            "hashlib",
            "subprocess",
            "threading",
            "time",
            "argparse",
            "shutil",
            "traceback",
            "contextlib",
            "urllib",
            "http",
            "socketserver",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // source: docs.python.org/3/library/stdtypes — builtin types that
        // begin uppercase or are commonly type-annotated.
        &[
            "None", "True", "False", "Any", "List", "Dict", "Tuple", "Set", "Optional",
        ]
    }
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript — source: nodejs.org/api (Node built-ins, common
// subset, preserved from resolver.rs).
// ---------------------------------------------------------------------------

pub struct TypeScriptProvider;
impl LanguageProvider for TypeScriptProvider {
    fn language(&self) -> &'static str {
        "typescript"
    }
    fn import_separator(&self) -> &'static str {
        // The TS parser normalizes source `/` to `::` in stored import paths
        // (parser/typescript.rs `path.replace('/', "::")`), so the resolver
        // sees `::`-separated paths. source: parser/typescript.rs emit_ts_import.
        "::"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        &[
            "fs",
            "path",
            "https",
            "http",
            "crypto",
            "util",
            "events",
            "stream",
            "child_process",
            "net",
            "url",
            "buffer",
            "os",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // source: TypeScript handbook "Everyday Types".
        &[
            "Array", "Promise", "Map", "Set", "Record", "Partial", "Readonly", "Object", "String",
            "Number", "Boolean", "Date",
        ]
    }
}

// ---------------------------------------------------------------------------
// Java — source: docs.oracle.com Java SE API (package roots); JLS §4.2
// (primitives) + java.lang implicitly-imported types.
// ---------------------------------------------------------------------------

pub struct JavaProvider;
impl LanguageProvider for JavaProvider {
    fn language(&self) -> &'static str {
        "java"
    }
    fn import_separator(&self) -> &'static str {
        "." // `java.util.List` — parser/java.rs rsplit('.')
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: Java SE / Jakarta package naming (docs.oracle.com).
        &["java", "javax", "jakarta", "sun", "jdk", "org"]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // JLS §4.2 primitives are lowercase (auto-skipped); list the boxed +
        // java.lang auto-imported types that begin uppercase.
        // source: docs.oracle.com java.lang package summary.
        &[
            "Integer",
            "Long",
            "Double",
            "Float",
            "Boolean",
            "Character",
            "Byte",
            "Short",
            "String",
            "Object",
            "Void",
            "Number",
        ]
    }
}

// ---------------------------------------------------------------------------
// Kotlin — source: kotlinlang.org/api/latest/jvm/stdlib. Interops with the
// JVM, so Java roots are external too. Ecosystem prefix data lives in
// kotlin_prefixes.rs (concern split; keeps this file under 500 lines).
// ---------------------------------------------------------------------------

pub struct KotlinProvider;
impl LanguageProvider for KotlinProvider {
    fn language(&self) -> &'static str {
        "kotlin"
    }
    fn import_separator(&self) -> &'static str {
        "." // `kotlin.collections.List` — parser/kotlin/extract/g2.rs rsplit('.')
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        kotlin_prefixes::KOTLIN_JVM_ANDROID_EXTERNAL_PREFIXES
    }
    fn primitives(&self) -> &'static [&'static str] {
        // source: kotlinlang.org/docs/basic-types.html.
        &[
            "Int", "Long", "Double", "Float", "Boolean", "Char", "Byte", "Short", "String", "Unit",
            "Any", "Nothing", "Array",
        ]
    }
    /// Kotlin/Android convention: source directories mirror the package
    /// hierarchy (kotlinlang.org coding conventions, "Directory structure";
    /// developer.android.com project-structure guide). Embedding the real
    /// `package` declaration into a symbol's qualified_name would break
    /// File-node linkage (indexer/persist.rs keys top-level Defines/Imports
    /// edges on the exact file path — issue #29 investigation), so the
    /// directory is the best zero-schema-change proxy; used only as
    /// heuristic evidence, never to override a stronger tier.
    fn package_of(&self, file_id: &str) -> Option<String> {
        let dir = file_id.rsplit_once('/')?.0;
        if dir.is_empty() {
            None
        } else {
            Some(dir.to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Swift — source: developer.apple.com (Swift standard library, Apple
// frameworks). Imports are bare module names (`import Foundation`).
// ---------------------------------------------------------------------------

pub struct SwiftProvider;
impl LanguageProvider for SwiftProvider {
    fn language(&self) -> &'static str {
        "swift"
    }
    fn import_separator(&self) -> &'static str {
        // Swift imports a whole module name with no separator; rsplit on `/`
        // (absent) returns the module verbatim. source: parser/swift.rs.
        "/"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: developer.apple.com framework + stdlib module names.
        &[
            "Swift",
            "Foundation",
            "UIKit",
            "SwiftUI",
            "Combine",
            "Dispatch",
            "CoreData",
            "CoreGraphics",
            "CoreFoundation",
            "AppKit",
            "XCTest",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // source: "The Swift Programming Language" — The Basics.
        &[
            "Int",
            "Double",
            "Float",
            "Bool",
            "String",
            "Character",
            "Array",
            "Dictionary",
            "Set",
            "Optional",
            "Any",
            "Void",
        ]
    }
}

// ---------------------------------------------------------------------------
// Objective-C — source: developer.apple.com. Imports are header paths
// (`#import <Foundation/Foundation.h>`) → `/`-separated.
// ---------------------------------------------------------------------------

pub struct ObjCProvider;
impl LanguageProvider for ObjCProvider {
    fn language(&self) -> &'static str {
        "objc"
    }
    fn import_separator(&self) -> &'static str {
        "/" // header path basename — parser/objc.rs
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // System framework umbrella headers (path roots) + core typedef hdrs.
        // source: developer.apple.com.
        &[
            "Foundation",
            "UIKit",
            "CoreFoundation",
            "CoreGraphics",
            "AppKit",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // source: developer.apple.com Foundation scalar typedefs.
        &[
            "id",
            "BOOL",
            "NSInteger",
            "NSUInteger",
            "CGFloat",
            "instancetype",
            "SEL",
            "Class",
            "IMP",
        ]
    }
}

// ---------------------------------------------------------------------------
// C — source: ISO/IEC 9899 (C standard library headers). Includes are header
// file paths → `/`-separated; basename is the header.
// ---------------------------------------------------------------------------

pub struct CProvider;
impl LanguageProvider for CProvider {
    fn language(&self) -> &'static str {
        "c"
    }
    fn import_separator(&self) -> &'static str {
        "/"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: ISO/IEC 9899 §7 standard headers (matched as the full
        // basename; first segment of a bare `stdio.h` is the header itself).
        &[
            "stdio.h",
            "stdlib.h",
            "string.h",
            "stddef.h",
            "stdint.h",
            "stdbool.h",
            "math.h",
            "ctype.h",
            "assert.h",
            "errno.h",
            "time.h",
            "limits.h",
            "stdarg.h",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // C builtin types are lowercase (auto-skipped by the uppercase
        // convention); list none. source: ISO/IEC 9899 §6.2.5.
        &[]
    }
}

// ---------------------------------------------------------------------------
// C++ — source: ISO/IEC 14882 (C++ standard library headers) + C headers.
// ---------------------------------------------------------------------------

pub struct CppProvider;
impl LanguageProvider for CppProvider {
    fn language(&self) -> &'static str {
        "cpp"
    }
    fn import_separator(&self) -> &'static str {
        "/"
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: ISO/IEC 14882 §16 standard headers (extensionless) + the C
        // compatibility headers.
        &[
            "vector",
            "string",
            "iostream",
            "memory",
            "map",
            "set",
            "unordered_map",
            "unordered_set",
            "algorithm",
            "functional",
            "utility",
            "stdexcept",
            "cstdint",
            "cstddef",
            "cstdio",
            "cstdlib",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // Lowercase builtins auto-skipped. source: ISO/IEC 14882 §6.8.1.
        &[]
    }
}

// ---------------------------------------------------------------------------
// Go — source: pkg.go.dev/std (standard library packages). Import paths are
// `/`-separated; the last segment is the package name. Third-party imports
// carry a domain in the first segment (e.g. `github.com/...`).
// ---------------------------------------------------------------------------

pub struct GoProvider;
impl LanguageProvider for GoProvider {
    fn language(&self) -> &'static str {
        "go"
    }
    fn import_separator(&self) -> &'static str {
        "/" // `net/http` — parser/go.rs rsplit('/')
    }
    fn external_prefixes(&self) -> &'static [&'static str] {
        // source: pkg.go.dev/std — first path segment of common stdlib
        // packages (matched on the first `/`-segment).
        &[
            "fmt", "os", "io", "strings", "strconv", "errors", "bytes", "bufio", "sort", "sync",
            "time", "context", "net", "encoding", "math", "reflect", "regexp", "path", "log",
            "flag", "testing",
        ]
    }
    fn primitives(&self) -> &'static [&'static str] {
        // Go builtins are lowercase (auto-skipped); none begin uppercase.
        // source: go.dev/ref/spec "Types".
        &[]
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

static DEFAULT: DefaultProvider = DefaultProvider;
static RUST: RustProvider = RustProvider;
static PYTHON: PythonProvider = PythonProvider;
static TYPESCRIPT: TypeScriptProvider = TypeScriptProvider;
static JAVA: JavaProvider = JavaProvider;
static KOTLIN: KotlinProvider = KotlinProvider;
static SWIFT: SwiftProvider = SwiftProvider;
static OBJC: ObjCProvider = ObjCProvider;
static C: CProvider = CProvider;
static CPP: CppProvider = CppProvider;
static GO: GoProvider = GoProvider;

/// Resolve a provider for a `language` column value. Unknown / empty / the
/// kuzu `Null(String)` sentinel fall back to the conservative DefaultProvider
/// so resolution never aborts on a missing language tag.
/// source: parser::Language::as_str for the canonical tags.
pub fn provider_for(language: &str) -> &'static dyn LanguageProvider {
    match language {
        "rust" => &RUST,
        "python" => &PYTHON,
        "typescript" => &TYPESCRIPT,
        "java" => &JAVA,
        "kotlin" => &KOTLIN,
        "swift" => &SWIFT,
        "objc" => &OBJC,
        "c" => &C,
        "cpp" => &CPP,
        "go" => &GO,
        _ => &DEFAULT,
    }
}

/// All recognized source-file extensions (without the dot), across every
/// supported language. File-id extraction needs no per-node language: the
/// extension set is effectively disjoint, so the union recognizes any node's
/// originating file. source: parser::Language::from_extension (authoritative).
pub const ALL_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "java", "kt", "kts", "swift", "m", "mm", "c", "h", "cc", "cpp", "cxx",
    "hh", "hpp", "hxx", "go", "js", "jsx", "mjs", "cjs", "rb",
];

/// Extract the file-path prefix from a node id or qualified name of the form
/// `<file_path>.<ext>::<rest>`. Tries every known extension; returns the file
/// path (including extension) when one matches, else None. Replaces the
/// resolver's hardcoded four-extension scan so all languages resolve.
pub fn extract_file_prefix(id: &str) -> Option<String> {
    for ext in ALL_EXTENSIONS {
        let marker = format!(".{ext}::");
        if let Some(i) = id.find(&marker) {
            // keep up to and including the extension, drop the `::` separator.
            return Some(id[..i + marker.len() - 2].to_string());
        }
    }
    None
}
