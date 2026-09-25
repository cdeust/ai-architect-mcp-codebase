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
    // Any import list or glob from `std::thread` can bring `spawn` in bare.
    "thread::{",
    "thread::*",
    "tokio::spawn",
    "tokio::task",
    "tokio::runtime",
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

/// Index just past the item that starts at `from`: through its matching `}`,
/// or through the `;` of a `mod x;` / `use` line when no `{` comes first.
/// Strings, character literals and comments are skipped so a brace in a
/// message cannot unbalance the match.
fn item_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let (mut i, mut depth) = (from, 0_i32);
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += text[i..].find('\n').unwrap_or(text.len() - i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += text[i..].find("*/").map_or(text.len() - i, |k| k + 2);
                continue;
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 2,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    text.len()
}

/// The text with every item marked `#[cfg(test)]` removed, wherever it sits in
/// the file, so production code that follows a test module is still scanned.
/// A marker only counts at the start of a line, never inside a comment.
fn production_part(text: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some(found) = text[pos..].find("#[cfg(test)]") {
        let at = pos + found;
        let line_start = text[..at].rfind('\n').map_or(0, |k| k + 1);
        if !text[line_start..at].trim().is_empty() {
            out.push_str(&text[pos..at + 1]);
            pos = at + 1;
            continue;
        }
        out.push_str(&text[pos..line_start]);
        // Skip any further attributes (`#[path = "..."]`), then the item.
        let mut item_start = at + "#[cfg(test)]".len();
        loop {
            let rest = text[item_start..].trim_start();
            if !rest.starts_with("#[") {
                break;
            }
            let skipped = text[item_start..].len() - rest.len();
            item_start += skipped + rest.find(']').map_or(rest.len(), |k| k + 1);
        }
        pos = item_end(text, item_start);
    }
    out.push_str(&text[pos..]);
    out
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
        let production = production_part(&text);
        for line in code_lines(&production) {
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
        let production = production_part(&text);
        for line in code_lines(&production) {
            if let Some(word) = GRAPH_WORDS.iter().find(|w| line.contains(**w)) {
                panic!(
                    "{allowed} starts a thread and mentions `{word}`: `{}`",
                    line.trim()
                );
            }
        }
    }
}

/// Lines of `sample` the guard would flag.
fn flagged(sample: &str) -> Vec<String> {
    let production = production_part(sample);
    code_lines(&production)
        .filter(|l| FORBIDDEN.iter().any(|w| l.contains(*w)))
        .map(|l| l.trim().to_string())
        .collect()
}

/// The guard must be able to fail: every way of starting a thread or joining a
/// pool that this repo could plausibly write is caught.
#[test]
fn the_scanner_flags_every_way_of_starting_a_thread() {
    for code in [
        "std::thread::spawn(|| {});",
        "use std::thread::spawn;",
        "use std::thread::{self, spawn};",
        "use std::thread::*;",
        "let b = std::thread::Builder::new();",
        "std::thread::scope(|s| {});",
        "tokio::spawn(async {});",
        "use tokio::task::spawn;",
        "let rt = tokio::runtime::Runtime::new();",
        "use rayon::prelude::*;",
        "crossbeam::scope(|s| {});",
    ] {
        let sample = format!("fn f() {{\n    {code}\n}}\n");
        assert_eq!(flagged(&sample).len(), 1, "not flagged: {code}");
    }
}

#[test]
fn a_thread_started_in_a_test_module_is_not_flagged() {
    let sample = "fn f() {}\n#[cfg(test)]\nmod t {\n    fn g() { std::thread::spawn(|| {}); }\n}\n";
    assert!(flagged(sample).is_empty());
}

// Cutting at the first `#[cfg(test)]` would have hidden this: production code
// that follows a test module in the same file.
#[test]
fn production_code_after_a_test_module_is_still_scanned() {
    let sample = "fn a() {}\n#[cfg(test)]\nmod t {\n    fn g() { let s = \"}\"; }\n}\n\
                  fn late() { std::thread::spawn(|| {}); }\n";
    assert_eq!(flagged(sample).len(), 1, "{:?}", flagged(sample));
}

#[test]
fn a_cfg_test_module_declared_in_another_file_is_skipped_with_its_attributes() {
    let sample = "fn a() {}\n#[cfg(test)]\n#[path = \"a_tests.rs\"]\nmod tests;\n\
                  fn late() { std::thread::spawn(|| {}); }\n";
    assert_eq!(flagged(sample).len(), 1, "{:?}", flagged(sample));
}

#[test]
fn a_marker_inside_a_comment_hides_nothing() {
    let sample = "// mentions #[cfg(test)] in prose\nfn late() { std::thread::spawn(|| {}); }\n";
    assert_eq!(flagged(sample).len(), 1);
}

/// In the scanned tree no production item follows a `#[cfg(test)]` block that
/// the earlier cut-at-first approach would have skipped: the stripped scan and
/// the old cut give the same verdict on every file, or the difference is shown.
#[test]
fn stripping_every_test_item_finds_no_thread_the_first_cut_would_have_missed() {
    for (rel, text) in source_files() {
        if ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }
        let cut = text
            .find("#[cfg(test)]")
            .map_or(text.as_str(), |i| &text[..i]);
        let stripped = production_part(&text);
        let bad = |t: &str| code_lines(t).any(|l| FORBIDDEN.iter().any(|w| l.contains(*w)));
        assert_eq!(bad(cut), bad(&stripped), "{rel}: the two scans disagree");
    }
}
