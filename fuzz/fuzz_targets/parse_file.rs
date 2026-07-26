// Fuzz target: `parse_file` over arbitrary bytes, across every language.
//
// This is the right thing to fuzz because it is the tool's ONLY untrusted-input
// boundary: the indexer walks a user's repository and hands whatever it finds to
// this function. Every other subsystem consumes the parser's typed output.
//
// The contract being fuzzed (from `parse_file`'s own docs): tree-sitter never
// throws on malformed input — it recovers into ERROR/MISSING nodes and returns a
// tree anyway — so `parse_file` must return `Ok` with a non-zero `parse_errors`
// count, or a clean `Err`. It must NEVER panic. A panic here is a denial of
// service on `index_codebase`: one malformed file in a repository would abort
// the indexing run.
//
// Two specific panic classes this reaches, both of which the walkers are
// exposed to because they slice source text by byte offsets taken from AST
// nodes:
//   * a byte range that is not a UTF-8 char boundary (`source[node.byte_range()]`
//     panics on a multi-byte character split), and
//   * an arithmetic/indexing edge in the name/qualified-name construction paths.
#![no_main]

use libfuzzer_sys::fuzz_target;

use ai_architect_mcp::parser::{parse_file, Language};

/// Every language the dispatcher accepts. Kept explicit rather than derived so
/// that adding a `Language` variant without adding it here is visible in review
/// — a silently unfuzzed language is the gap this list exists to prevent.
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

fuzz_target!(|data: &[u8]| {
    // The first byte selects the language so one corpus exercises every
    // grammar; the remainder is the source text. An empty input is a valid
    // case (an empty file) and must not panic either.
    let (lang, rest) = match data.split_first() {
        Some((selector, rest)) => (LANGUAGES[*selector as usize % LANGUAGES.len()], rest),
        None => (Language::Rust, &[][..]),
    };

    // Feed genuinely arbitrary bytes: `from_utf8_lossy` keeps the input
    // adversarial (replacement characters, lone surrogates rendered as U+FFFD,
    // mixed-width sequences) while satisfying the `&str` signature. The
    // multi-byte characters this produces are exactly what makes a byte-range
    // slice panic if a walker splits a character.
    let source = String::from_utf8_lossy(rest);

    // The only assertion is the contract: do not panic, and do not lie about
    // the outcome. Both arms are acceptable results.
    match parse_file(&source, "fuzz.input", lang) {
        Ok(result) => {
            // Every emitted span must be within the file, or a consumer
            // following it back to source would read the wrong lines. Line
            // numbers are 1-based, so 0 is never valid for a real node.
            for node in &result.nodes {
                assert!(
                    node.start_line >= 1,
                    "emitted a 1-based start_line of 0 for {:?}",
                    node.label
                );
                assert!(
                    node.end_line >= node.start_line,
                    "emitted an inverted span {}..{} for {:?}",
                    node.start_line,
                    node.end_line,
                    node.label
                );
            }
            // An error range must likewise be ordered.
            for (start, end) in &result.error_ranges {
                assert!(start <= end, "inverted error range {start}..{end}");
            }
        }
        Err(_) => {
            // A clean error (grammar-set failure or parse timeout) is fine.
        }
    }
});
