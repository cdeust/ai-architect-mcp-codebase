// Fuzz target: `parse_file` over VALID UTF-8 only.
//
// Complements `parse_file`, which feeds lossy-converted arbitrary bytes. That
// target reaches encoding edges, but the replacement character it inserts makes
// the input less like real source, so the fuzzer spends effort on byte soup
// rather than on grammar structure.
//
// This target takes only inputs that are already valid UTF-8, so libFuzzer's
// coverage feedback can evolve them toward things that look like code — nested
// scopes, unbalanced delimiters, deep recursion — which is where the walkers'
// recursive descent and qualified-name construction actually live. Together the
// two cover encoding edges AND structural edges; either alone leaves half the
// surface unexplored.
#![no_main]

use libfuzzer_sys::fuzz_target;

use ai_architect_mcp::parser::{parse_file, Language};

const LANGUAGES: &[Language] = &[
    Language::Rust,
    Language::Python,
    Language::TypeScript,
    Language::Java,
    Language::Kotlin,
    Language::Swift,
    Language::ObjC,
    Language::C,
    Language::Cpp,
    Language::Go,
    Language::Ruby,
];

fuzz_target!(|input: (u8, String)| {
    let (selector, source) = input;
    let lang = LANGUAGES[selector as usize % LANGUAGES.len()];

    if let Ok(result) = parse_file(&source, "fuzz.input", lang) {
        let line_count = source.lines().count() as u64;
        for node in &result.nodes {
            assert!(node.start_line >= 1, "1-based start_line of 0");
            assert!(
                node.end_line >= node.start_line,
                "inverted span {}..{}",
                node.start_line,
                node.end_line
            );
            // A span must not point past the end of the file. Only checked for
            // non-empty input: a zero-line source legitimately yields nothing,
            // and tree-sitter's end position on a trailing newline can equal
            // the line count.
            if line_count > 0 {
                assert!(
                    node.start_line <= line_count + 1,
                    "start_line {} past the {} lines of source",
                    node.start_line,
                    line_count
                );
            }
            // A qualified name is a primary key; an empty one would collide
            // with every other empty one.
            assert!(
                !node.qualified_name.is_empty(),
                "emitted an empty qualified_name for {:?}",
                node.label
            );
        }
    }
});
