// single_thread_graph_handles: guard for the single-thread invariant behind
// the graph handle cache (issue #352).
//
// The read cache (`graph_cache`) and the release hook that
// `graph_store::handles` calls are per thread. A graph opened on another
// thread would neither see nor release the cache's handle, and the older
// handle would close over the newer one's writes. The server runs every
// request on one thread; this test makes that mechanical, like
// `no_naive_cypher_escape`: production code under `src/` may not start a
// thread or use an async runtime or a thread pool, except where listed here.
//
// Allowlist: `lsp_client/frames.rs` runs a reader thread over the language
// server's stdout. It exchanges JSON frames over a channel and must never
// touch a graph handle; the second test checks that by content.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    "thread::spawn",
    "thread::Builder",
    "thread::scope",
    "tokio::spawn",
    "tokio::task",
    "#[tokio::main]",
    "rayon::",
    "use rayon",
    "crossbeam::scope",
];

/// Files allowed to start a thread, relative to `src/`.
const ALLOWLIST: &[&str] = &["lsp_client/frames.rs"];

/// Words that would mean an allowlisted file reaches a graph handle.
const GRAPH_WORDS: &[&str] = &["GraphStore", "graph_store", "graph_cache", "open_cached"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Test-only files: a `*_tests.rs` file, `tests.rs`, or anything under a `tests` dir.
fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || path.components().any(|c| c.as_os_str() == "tests")
}

/// The part of a file before its inline `#[cfg(test)]` module.
fn production_part(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(i) => &text[..i],
        None => text,
    }
}

fn source_files() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files
        .into_iter()
        .filter(|f| !is_test_file(f))
        .map(|f| {
            let rel = f
                .strip_prefix(&src)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = fs::read_to_string(&f).expect("read");
            (rel, text)
        })
        .collect()
}

/// Comment lines are not code: the cache's own docs name the forbidden words.
fn code_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| {
        let t = l.trim_start();
        !(t.starts_with("//") || t.starts_with("///") || t.starts_with("//!"))
    })
}

#[test]
fn production_code_starts_no_thread_outside_the_allowlist() {
    let mut offenders = Vec::new();
    for (rel, text) in source_files() {
        if ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }
        for line in code_lines(production_part(&text)) {
            if let Some(word) = FORBIDDEN.iter().find(|w| line.contains(**w)) {
                offenders.push(format!("{rel}: `{word}` in `{}`", line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the graph handle cache is per thread (issue #352): a thread that opens a graph \
         bypasses its release. Offenders:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn an_allowlisted_thread_never_touches_a_graph_handle() {
    for allowed in ALLOWLIST {
        let (_, text) = source_files()
            .into_iter()
            .find(|(rel, _)| rel == allowed)
            .unwrap_or_else(|| panic!("allowlisted file {allowed} is gone: drop it from the list"));
        for line in code_lines(production_part(&text)) {
            if let Some(word) = GRAPH_WORDS.iter().find(|w| line.contains(**w)) {
                panic!(
                    "{allowed} starts a thread and mentions `{word}`: `{}`",
                    line.trim()
                );
            }
        }
    }
}

/// The guard must be able to fail: a file that spawns a thread is caught.
#[test]
fn the_scanner_flags_a_spawn_in_production_text() {
    let sample = "fn f() {\n    std::thread::spawn(|| {});\n}\n#[cfg(test)]\nmod t { fn g() { std::thread::spawn(|| {}); } }\n";
    let hits: Vec<_> = code_lines(production_part(sample))
        .filter(|l| FORBIDDEN.iter().any(|w| l.contains(*w)))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
}
