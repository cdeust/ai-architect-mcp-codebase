// language_provider::providers — the concrete per-language implementations.
//
// Split from `language_provider/mod.rs`, which was over the §4.1 cap: the trait
// and the registry that selects an implementation are one concern, and the ten
// implementations behind it are another. Adding a language now touches this
// file and the registry's match arm, not the contract itself.

use super::kotlin_prefixes;
use super::LanguageProvider;

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
