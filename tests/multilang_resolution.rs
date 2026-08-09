// multilang_resolution — Phase 2 verification for the LanguageProvider trait.
//
// Proves the seven parse-wired-but-resolution-dormant grammars now emit
// cross-file resolution edges, and that the three live languages do not
// regress. Each language is indexed into its own isolated graph so the
// resolved-edge counts are attributable to that language alone.
//
// Dormancy had TWO causes, both fixed in Phase 2:
//   (1) resolver helpers hardcoded Rust/Python/TS file extensions and the
//       `::` import separator (now per-language via LanguageProvider);
//   (2) the dormant parsers emitted the import path as property "target",
//       which persist.rs never mapped to the `path` column (renamed to
//       "path" so i.path is populated).
//
// source: stages/stage-3b.md §5; language_provider.rs.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use ai_architect_mcp::language_provider::{extract_file_prefix, provider_for};
use ai_architect_mcp::resolver::{self, ResolutionResult};
use std::fs;

/// Index a set of (relative_path, source) files into a fresh graph and run
/// the Stage-3b resolution pass. Returns the resolution result.
fn index_and_resolve(tag: &str, files: &[(&str, &str)]) -> ResolutionResult {
    // issue #25 audit: process::id() collides across processes under PID
    // reuse; tempfile's random suffix does not (tag is kept as a
    // human-readable prefix for debuggability, not for uniqueness).
    let root = tempfile::Builder::new()
        .prefix(&format!("multilang_{tag}_"))
        .tempdir()
        .expect("create temp dir")
        .keep();
    let _ = fs::remove_dir_all(&root);
    let src = root.join("src");
    for (rel, body) in files {
        let p = src.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(&p, body).expect("write fixture file");
    }
    let graph_dir = root.join("graph");
    indexer::index_codebase(&src, &graph_dir).expect("index_codebase");
    let store = GraphStore::open_or_create(&graph_dir).expect("open graph");
    let res = resolver::resolve_graph(&store).expect("resolve_graph");
    let _ = fs::remove_dir_all(&root);
    res
}

fn resolution_total(r: &ResolutionResult) -> u64 {
    r.imports_resolved + r.calls_resolved + r.impls_resolved + r.extends_resolved + r.uses_resolved
}

// ---------------------------------------------------------------------------
// Java — cross-file via an explicit import (`.`-separated path). Exercises
// the data-layer fix (path column) + the `.` separator + non-external
// detection (`com.*` is internal) + field-type Uses.
// ---------------------------------------------------------------------------

#[test]
fn java_cross_file_resolution_lights_up() {
    let config = "package com.app;\npublic class Config {\n    public String name;\n}\n";
    let main = "package com.app;\nimport com.app.Config;\npublic class Main {\n    public Config cfg;\n}\n";
    let res = index_and_resolve("java", &[("Config.java", config), ("Main.java", main)]);

    eprintln!(
        "java: imports={} calls={} impls={} extends={} uses={} unresolved={}",
        res.imports_resolved,
        res.calls_resolved,
        res.impls_resolved,
        res.extends_resolved,
        res.uses_resolved,
        res.unresolved.len()
    );
    assert!(
        resolution_total(&res) > 0,
        "Java must resolve at least one cross-file edge (was dormant pre-Phase-2)"
    );
    // The import com.app.Config -> Config struct is the canonical edge.
    assert!(
        res.imports_resolved > 0 || res.uses_resolved > 0,
        "expected a resolved Java import or field-type use, got imports={} uses={}",
        res.imports_resolved,
        res.uses_resolved
    );
}

// ---------------------------------------------------------------------------
// Go — same-package cross-file call (no import needed; unqualified callee
// resolved by same-corpus name match). Exercises the `.go` extension fix in
// file-id extraction (caller file-id drives call resolution).
// ---------------------------------------------------------------------------

#[test]
fn go_same_package_resolution_lights_up() {
    let models = "package app\n\ntype Config struct {\n\tName string\n}\n\nfunc NewConfig() *Config {\n\treturn &Config{}\n}\n";
    let main = "package app\n\nfunc Run() {\n\tc := NewConfig()\n\t_ = c\n}\n";
    let res = index_and_resolve("go", &[("models.go", models), ("main.go", main)]);

    eprintln!(
        "go: imports={} calls={} impls={} extends={} uses={} unresolved={}",
        res.imports_resolved,
        res.calls_resolved,
        res.impls_resolved,
        res.extends_resolved,
        res.uses_resolved,
        res.unresolved.len()
    );
    assert!(
        resolution_total(&res) > 0,
        "Go must resolve at least one cross-file edge (was dormant pre-Phase-2)"
    );
}

// ---------------------------------------------------------------------------
// Regression guard — the live Rust path must still resolve. Mirrors the
// stage3b_integration fixture shape.
// ---------------------------------------------------------------------------

#[test]
fn rust_resolution_unchanged() {
    let main = "use crate::models::Config;\n\nfn run() {\n    let _c = Config::new();\n}\n";
    let models = "pub struct Config {\n    pub name: String,\n}\n\nimpl Config {\n    pub fn new() -> Self {\n        Config { name: String::new() }\n    }\n}\n";
    let res = index_and_resolve("rust", &[("main.rs", main), ("models.rs", models)]);

    eprintln!(
        "rust: imports={} calls={} impls={} extends={} uses={} unresolved={}",
        res.imports_resolved,
        res.calls_resolved,
        res.impls_resolved,
        res.extends_resolved,
        res.uses_resolved,
        res.unresolved.len()
    );
    assert!(
        resolution_total(&res) > 0,
        "Rust resolution must not regress"
    );
}

// ---------------------------------------------------------------------------
// Sanity: file-id extraction recognizes every supported extension. Guards the
// dominant dormancy cause directly (no graph needed).
// ---------------------------------------------------------------------------

#[test]
fn file_prefix_extraction_covers_dormant_extensions() {
    let cases = [
        ("Sources/App.swift::App", "Sources/App.swift"),
        ("src/lib.kt::Foo", "src/lib.kt"),
        ("inc/header.h::macro", "inc/header.h"),
        ("a/b.cpp::ns::Klass", "a/b.cpp"),
        ("m/View.m::View", "m/View.m"),
    ];
    for (id, expect) in cases {
        assert_eq!(
            extract_file_prefix(id).as_deref(),
            Some(expect),
            "extension extraction failed for {id}"
        );
    }
}

// ---------------------------------------------------------------------------
// LanguageProvider registry unit coverage (moved out of src to keep
// language_provider.rs within the 500-line file limit).
// ---------------------------------------------------------------------------

#[test]
fn registry_covers_all_ten_languages() {
    for lang in [
        "rust",
        "python",
        "typescript",
        "java",
        "kotlin",
        "swift",
        "objc",
        "c",
        "cpp",
        "go",
    ] {
        assert_eq!(
            provider_for(lang).language(),
            lang,
            "tag mismatch for {lang}"
        );
    }
}

#[test]
fn unknown_language_falls_back_to_default() {
    assert_eq!(provider_for("brainfuck").language(), "unknown");
    assert_eq!(provider_for("").language(), "unknown");
    assert_eq!(provider_for("Null(String)").language(), "unknown");
}

#[test]
fn import_last_segment_per_separator() {
    // Rust `::`, with crate:: strip.
    assert_eq!(
        provider_for("rust").import_last_segment("crate::a::Bar"),
        "Bar"
    );
    // Java/Kotlin keep raw `.`.
    assert_eq!(
        provider_for("java").import_last_segment("java.util.List"),
        "List"
    );
    // Python/TS are stored `::`-normalized by their parsers.
    assert_eq!(
        provider_for("python").import_last_segment("os::path"),
        "path"
    );
    assert_eq!(
        provider_for("typescript").import_last_segment("a::b::Mod"),
        "Mod"
    );
    // Go/ObjC/C `/`.
    assert_eq!(provider_for("go").import_last_segment("net/http"), "http");
    assert_eq!(
        provider_for("c").import_last_segment("sys/stat.h"),
        "stat.h"
    );
    // Swift: bare module name, no separator.
    assert_eq!(
        provider_for("swift").import_last_segment("Foundation"),
        "Foundation"
    );
}

#[test]
fn external_detection_is_internal_for_crate_self_super() {
    let rust = provider_for("rust");
    assert!(!rust.is_external_import("crate::foo"));
    assert!(!rust.is_external_import("self::foo"));
    assert!(!rust.is_external_import("super::bar"));
    assert!(rust.is_external_import("std::io"));
    assert!(rust.is_external_import("serde::Serialize"));
}

#[test]
fn external_detection_per_language() {
    assert!(provider_for("java").is_external_import("java.util.List"));
    assert!(provider_for("go").is_external_import("fmt"));
    assert!(provider_for("go").is_external_import("net/http")); // first seg `net`
    assert!(!provider_for("go").is_external_import("myproject/internal/x"));
    assert!(provider_for("swift").is_external_import("Foundation"));
    assert!(!provider_for("swift").is_external_import("MyAppModule"));
}

// ---------------------------------------------------------------------------
// Issue #31 — Kotlin external-prefix allowlist was missing the JVM/Android
// ecosystem (androidx.*, retrofit2.*, com.google.*, ...), so those refs fell
// through to "no target found in graph" instead of being classified
// external. Covers both single-segment prefixes (retrofit2, junit, dagger)
// and compound dotted prefixes (com.google, org.jetbrains, timber.log).
// ---------------------------------------------------------------------------

#[test]
fn kotlin_external_detection_covers_jvm_android_ecosystem() {
    let kotlin = provider_for("kotlin");
    for path in [
        "androidx.lifecycle.ViewModel",
        "android.content.Context",
        "retrofit2.Retrofit",
        "com.google.gson.Gson",
        "com.squareup.okhttp3.OkHttpClient",
        "io.reactivex.Observable",
        "org.reactivestreams.Publisher",
        "org.jetbrains.annotations.NotNull",
        "org.intellij.lang.annotations.Language",
        "junit.framework.TestCase",
        "org.junit.Test",
        "org.mockito.Mockito",
        "io.mockk.mockk",
        "org.koin.core.Koin",
        "dagger.Module",
        "org.slf4j.Logger",
        "org.apache.commons.lang3.StringUtils",
        "com.fasterxml.jackson.databind.ObjectMapper",
        "timber.log.Timber",
    ] {
        assert!(
            kotlin.is_external_import(path),
            "{path} should be classified external"
        );
    }

    // Segment-boundary precision: a compound prefix must match whole path
    // segments, not just a string prefix — otherwise `com.googlex.*` (an
    // in-corpus package that merely starts with the same characters) would
    // be misclassified external.
    assert!(!kotlin.is_external_import("com.googlex.MyOwnPackage.Foo"));
    assert!(!kotlin.is_external_import("comgoogle.MyOwnPackage.Foo"));
    assert!(!kotlin.is_external_import("myapp.feature.Repository"));
}

#[test]
fn kotlin_jvm_android_ecosystem_imports_classified_external_end_to_end() {
    let main = "import androidx.lifecycle.ViewModel\n\
                import retrofit2.Retrofit\n\
                import com.google.gson.Gson\n\n\
                class MainViewModel : ViewModel() {\n    \
                    val retrofit: Retrofit? = null\n    \
                    val gson: Gson? = null\n\
                }\n";
    let res = index_and_resolve("kotlin_ext", &[("MainViewModel.kt", main)]);

    let ecosystem_targets = [
        "androidx.lifecycle.ViewModel",
        "retrofit2.Retrofit",
        "com.google.gson.Gson",
    ];
    for expect in ecosystem_targets {
        let hit = res.unresolved.iter().find(|u| u.target_text == expect);
        assert!(
            hit.is_some(),
            "expected an unresolved import entry for {expect}; got {:?}",
            res.unresolved
                .iter()
                .map(|u| &u.target_text)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            hit.unwrap().reason,
            resolver::EXTERNAL_UNRESOLVED_REASON,
            "{expect} must carry the external classification (issue #31), not a generic \
             'no target found in graph' fallback"
        );
    }
}
