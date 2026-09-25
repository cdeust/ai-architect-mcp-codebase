//! Prints one line per Rust file under `$CORPUS/{src,tests,crates}`: its path, the
//! node and ref counts, and a SHA-256 of everything `parse_file` returns for it
//! (nodes, refs, parse errors). Run by `scripts/check_parse_identity.py`, which
//! copies this file to `examples/parse_digest.rs` of the tree under test:
//!   CORPUS=<dir> OUT=<file> cargo run --example parse_digest
use ai_architect_mcp::parser::{parse_file, Language};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn line(root: &Path, file: &Path) -> Option<String> {
    let source = fs::read_to_string(file).ok()?;
    let rel = file.strip_prefix(root).ok()?.to_string_lossy().to_string();
    let mut hasher = Sha256::new();
    let parsed = match parse_file(&source, &rel, Language::Rust) {
        Ok(parsed) => parsed,
        Err(error) => return Some(format!("{rel} ERR {error}")),
    };
    for n in &parsed.nodes {
        let fields = format!(
            "N|{}|{}|{}|{}|{}|{}|{:?}\n",
            n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
        );
        hasher.update(fields.as_bytes());
    }
    for r in &parsed.refs {
        let fields = format!(
            "R|{}|{}|{}\n",
            r.kind, r.from_qualified_name, r.to_qualified_name
        );
        hasher.update(fields.as_bytes());
    }
    hasher.update(format!("E|{}|{:?}\n", parsed.parse_errors, parsed.error_ranges).as_bytes());
    let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(format!(
        "{rel} nodes={} refs={} {digest}",
        parsed.nodes.len(),
        parsed.refs.len()
    ))
}

fn main() {
    let corpus = std::env::var("CORPUS").expect("CORPUS is the tree to fingerprint");
    let out = std::env::var("OUT").expect("OUT is the file to write");
    let root = Path::new(&corpus);
    let mut files = Vec::new();
    for dir in ["src", "tests", "crates"] {
        collect(&root.join(dir), &mut files);
    }
    files.sort();
    let lines: Vec<String> = files.iter().filter_map(|f| line(root, f)).collect();
    fs::write(out, lines.join("\n") + "\n").expect("write OUT");
}
