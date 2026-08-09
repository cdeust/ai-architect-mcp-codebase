// parser::spec::structural_imports_coverage_tests — end-to-end coverage for
// the issue #224 imports follow-up: runs the FULL `parse_structural`
// pipeline (not `structural_imports.rs`'s unit-level extraction functions,
// covered in `structural_imports_tests.rs`) and asserts on the emitted
// `ExtractedNode`/`ExtractedRef` shape — `LABEL_IMPORT` nodes, `"Imports"`
// edges, and correct file-scope attribution — the thing a real caller of
// `parse_structural` actually observes.
//
// Swift and Kotlin (kotlin-ng) are the two languages this follow-up was
// scoped to close (both `import_declaration` nodes carry ZERO fields,
// exactly the case field-shape classification structurally cannot reach —
// see `structural_imports.rs`'s module doc) and get a dedicated test each,
// matching how `structural_coverage_tests.rs` gives each of its priority
// languages its own named test rather than folding everything into a table.

use super::structural::{parse_structural, StructuralSpec};
use crate::parser::{Language, LABEL_IMPORT};

fn import_paths(refs: &[crate::parser::ExtractedRef]) -> Vec<String> {
    let mut v: Vec<String> = refs
        .iter()
        .filter(|e| e.kind == "Imports")
        .map(|e| e.to_qualified_name.clone())
        .collect();
    v.sort();
    v
}

fn import_node_names(nodes: &[crate::parser::ExtractedNode]) -> Vec<String> {
    let mut v: Vec<String> = nodes
        .iter()
        .filter(|n| n.label == LABEL_IMPORT)
        .map(|n| n.name.clone())
        .collect();
    v.sort();
    v
}

#[test]
fn swift_imports_are_now_extracted_the_owners_named_priority_case() {
    // Swift's `import_declaration` has ZERO fields (verified in
    // `structural.rs`'s original module doc) — the exact fieldless case
    // pure field-shape classification could never reach. This is the
    // headline flip this follow-up exists to produce.
    let spec = StructuralSpec {
        language: Language::Swift,
        ts_language: || tree_sitter_swift::LANGUAGE.into(),
    };
    let src = "import Foundation\nimport UIKit\n\nfunc helper() -> Int {\n    return 1\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "m.swift").expect("swift structural parse");
    assert_eq!(
        import_paths(&r.refs),
        vec!["Foundation".to_string(), "UIKit".to_string()]
    );
    assert_eq!(
        import_node_names(&r.nodes),
        vec!["Foundation".to_string(), "UIKit".to_string()]
    );
    let import_node = r
        .nodes
        .iter()
        .find(|n| n.label == LABEL_IMPORT && n.name == "Foundation")
        .expect("Foundation import node");
    assert_eq!(
        import_node.properties,
        vec![("path".to_string(), "Foundation".to_string())]
    );
    assert!(
        r.refs
            .iter()
            .any(|e| e.kind == "Imports" && e.from_qualified_name.starts_with("m.swift::import:")),
        "import scoped to file, got {:?}",
        r.refs
            .iter()
            .map(|e| (&e.kind, &e.from_qualified_name))
            .collect::<Vec<_>>()
    );
}

#[test]
fn kotlin_ng_imports_are_now_extracted_the_owners_other_named_priority_case() {
    // kotlin-ng's `import_declaration` is likewise fieldless (verified in
    // `structural.rs`'s original module doc).
    let spec = StructuralSpec {
        language: Language::Kotlin,
        ts_language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
    };
    let src = "import kotlin.io.println\nimport kotlin.collections.List\n\nfun helper(): Int {\n    return 1\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "Greeter.kt").expect("kotlin structural parse");
    assert_eq!(
        import_paths(&r.refs),
        vec![
            "kotlin.collections.List".to_string(),
            "kotlin.io.println".to_string()
        ]
    );
    assert_eq!(
        import_node_names(&r.nodes),
        vec!["List".to_string(), "println".to_string()],
        "the Import node's `name` is the trailing segment (last_segment), \
         matching shallow.rs's Import display convention"
    );
}

#[test]
fn go_grouped_import_produces_two_import_entries_with_an_alias() {
    let spec = StructuralSpec {
        language: Language::Go,
        ts_language: || tree_sitter_go::LANGUAGE.into(),
    };
    let src = "package main\n\nimport (\n\t\"fmt\"\n\tos2 \"os\"\n)\n\nfunc helper() int {\n\treturn 1\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "m.go").expect("go structural parse");
    assert_eq!(
        import_paths(&r.refs),
        vec!["fmt".to_string(), "os".to_string()]
    );
    let os_import = r
        .nodes
        .iter()
        .find(|n| n.label == LABEL_IMPORT && n.name == "os")
        .expect("os import node");
    assert!(
        os_import
            .properties
            .contains(&("alias".to_string(), "os2".to_string())),
        "got {:?}",
        os_import.properties
    );
}

#[test]
fn ruby_require_is_reached_only_via_the_call_path_not_a_keyword() {
    // Ruby's `require` parses as an ordinary `call` node — TIER 1 already
    // classifies it as a CallSite; `structural_imports`'s IMPORT_CALL_NAMES
    // table additionally emits an Import from the same node (both
    // representations coexist, see `structural.rs`'s wiring comment).
    let spec = StructuralSpec {
        language: Language::Ruby,
        ts_language: || tree_sitter_ruby::LANGUAGE.into(),
    };
    let src = "require 'set'\n\ndef helper()\n  1\nend\n";
    let (r, _stats) = parse_structural(&spec, src, "m.rb").expect("ruby structural parse");
    assert_eq!(import_paths(&r.refs), vec!["set".to_string()]);
    assert!(
        r.nodes
            .iter()
            .any(|n| n.label == crate::parser::LABEL_CALL_SITE && n.name == "require"),
        "require must still be reachable as an ordinary CallSite too, got {:?}",
        r.nodes
            .iter()
            .map(|n| (&n.label, &n.name))
            .collect::<Vec<_>>()
    );
}

/// The original 10-hand-written-languages-plus-Ruby sample's imports column
/// (companion to `structural_wide_sample_tests`'s 12-grammar OUT-OF-SAMPLE
/// column) — one minimal, real import statement per language, measured by
/// actually running `parse_structural`, not predicted from grammar docs.
fn original_sample_imports_recall() -> (usize, usize, Vec<(&'static str, bool)>) {
    struct Sample {
        name: &'static str,
        ts_language: fn() -> tree_sitter::Language,
        src: &'static str,
        file: &'static str,
    }
    let samples = vec![
        Sample {
            name: "Go",
            ts_language: || tree_sitter_go::LANGUAGE.into(),
            src: "package main\n\nimport \"fmt\"\n",
            file: "m.go",
        },
        Sample {
            name: "Python",
            ts_language: || tree_sitter_python::LANGUAGE.into(),
            src: "import os\n",
            file: "m.py",
        },
        Sample {
            name: "Java",
            ts_language: || tree_sitter_java::LANGUAGE.into(),
            src: "import java.util.List;\n",
            file: "M.java",
        },
        Sample {
            name: "TypeScript",
            ts_language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            src: "import { foo } from 'bar';\n",
            file: "m.ts",
        },
        Sample {
            name: "Ruby",
            ts_language: || tree_sitter_ruby::LANGUAGE.into(),
            src: "require 'set'\n",
            file: "m.rb",
        },
        Sample {
            name: "Rust",
            ts_language: || tree_sitter_rust::LANGUAGE.into(),
            src: "use std::io;\n",
            file: "m.rs",
        },
        Sample {
            name: "C",
            ts_language: || tree_sitter_c::LANGUAGE.into(),
            src: "#include <stdio.h>\n",
            file: "m.c",
        },
        Sample {
            name: "C++",
            ts_language: || tree_sitter_cpp::LANGUAGE.into(),
            src: "#include <vector>\n",
            file: "m.cpp",
        },
        Sample {
            name: "ObjC",
            ts_language: || tree_sitter_objc::LANGUAGE.into(),
            src: "#import <Foundation/Foundation.h>\n",
            file: "m.m",
        },
        Sample {
            name: "Swift",
            ts_language: || tree_sitter_swift::LANGUAGE.into(),
            src: "import Foundation\n",
            file: "m.swift",
        },
        Sample {
            name: "Kotlin",
            ts_language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
            src: "import kotlin.io.println\n",
            file: "m.kt",
        },
    ];
    let mut results = Vec::with_capacity(samples.len());
    let mut recalled = 0usize;
    let total = samples.len();
    for sample in &samples {
        let spec = StructuralSpec {
            language: Language::Rust,
            ts_language: sample.ts_language,
        };
        let import_ok = match parse_structural(&spec, sample.src, sample.file) {
            Ok((r, _stats)) => r.refs.iter().any(|e| e.kind == "Imports"),
            Err(_) => false,
        };
        if import_ok {
            recalled += 1;
        }
        results.push((sample.name, import_ok));
    }
    (recalled, total, results)
}

#[test]
fn original_sample_imports_recall_report() {
    let (recalled, total, results) = original_sample_imports_recall();
    eprintln!("\n== Original-sample (10 hand-written + Ruby) IMPORTS recall ==");
    for (name, ok) in &results {
        eprintln!("{name:12} import={}", if *ok { "YES" } else { "no" });
    }
    eprintln!(
        "== imports recalled {recalled}/{total} ({:.0}%) ==\n",
        100.0 * recalled as f64 / total as f64
    );
    assert_eq!(total, 11, "10 hand-written languages + Ruby");
}

#[test]
fn java_import_declaration_reached_via_span_reconstruction_fieldless_container() {
    // Java's `import_declaration` has ZERO fields of its own (verified in
    // `structural.rs`'s original module doc) — the span-reconstruction
    // fallback resolves the dotted path from the raw source slice.
    let spec = StructuralSpec {
        language: Language::Java,
        ts_language: || tree_sitter_java::LANGUAGE.into(),
    };
    let src = "import java.util.List;\n\nclass Greeter {\n    int helper() { return 1; }\n}\n";
    let (r, _stats) = parse_structural(&spec, src, "Greeter.java").expect("java structural parse");
    assert_eq!(import_paths(&r.refs), vec!["java.util.List".to_string()]);
}
