// parser::spec::rust_identity_tests: issue #353 changes the ids of items that
// collide under exclusive `#[cfg]` gates and nothing else. The parse output of a
// file without such a collision must be what the walk produced before the change.
//
// How this is checked, and its limit: the twin identity has a switch
// (`parse_with_spec_no_twins`), and every Rust file of the repository (src/,
// tests/, crates/) is parsed with and without it. None of them holds a twin, so
// the two outputs must be equal for every file. This compares the head with
// itself minus the twin logic; the comparison with the code of main was run once
// with the same digest over the same files (see the PR), because main is not
// available to a test.

use std::fs;
use std::path::Path;

use super::{parse_with_spec_no_twins, RUST_SPEC};
use crate::parser::{parse_file, Language, ParseResult};

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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

/// Everything the parse output holds, as text, so two outputs compare exactly.
fn digest(result: &ParseResult) -> String {
    let mut text = String::new();
    for n in &result.nodes {
        text.push_str(&format!(
            "N|{}|{}|{}|{}|{}|{}|{:?}\n",
            n.label, n.name, n.qualified_name, n.start_line, n.end_line, n.visibility, n.properties
        ));
    }
    for r in &result.refs {
        text.push_str(&format!(
            "R|{}|{}|{}\n",
            r.kind, r.from_qualified_name, r.to_qualified_name
        ));
    }
    text.push_str(&format!(
        "E|{}|{:?}\n",
        result.parse_errors, result.error_ranges
    ));
    text
}

#[test]
fn files_without_twins_parse_exactly_as_they_did_before_the_twin_identity() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests", "crates"] {
        collect(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(files.len() > 200, "only {} Rust files found", files.len());
    let mut differing = Vec::new();
    for file in &files {
        let Ok(source) = fs::read_to_string(file) else {
            continue;
        };
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let with = parse_file(&source, &rel, Language::Rust).map(|r| digest(&r));
        let without = parse_with_spec_no_twins(&RUST_SPEC, &source, &rel).map(|r| digest(&r));
        if with != without {
            differing.push(rel);
        }
    }
    assert!(
        differing.is_empty(),
        "the twin identity changed the parse of files that hold no twin: {differing:?}"
    );
}

/// The control: the same comparison DOES see a difference on a file with twins,
/// so an equal result above is not an equal-by-construction comparison.
#[test]
fn a_file_with_twins_differs_with_and_without_the_twin_identity() {
    let source = "#[cfg(unix)]\nfn f() {}\n#[cfg(not(unix))]\nfn f() {}\n";
    let with = parse_file(source, "lib.rs", Language::Rust).map(|r| digest(&r));
    let without = parse_with_spec_no_twins(&RUST_SPEC, source, "lib.rs").map(|r| digest(&r));
    assert_ne!(with, without);
}
