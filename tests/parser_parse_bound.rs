//! Regression tests for the `parse_file` byte-size boundary cap (issue #148).
//!
//! The scheduled long-run `Fuzz` job OOMed (used 4136 MB > 4096 MB limit) under
//! the 900 s libFuzzer search. Root cause: `parse_file` — the tool's only
//! untrusted-input boundary — had no size cap, so libFuzzer grew inputs without
//! bound; each large adversarial unit costs up to the full 5 s parse timeout in
//! tree-sitter's superlinear error recovery and is retained in libFuzzer's
//! in-memory corpus (the `std::vector` the heap dump attributed 78 % of live
//! heap to) until RSS crossed the limit. The 5 s timeout bounds a single parse's
//! time/RSS but not this accumulation; a byte cap at the parse boundary does.
//!
//! The load-bearing fix (`MAX_TREE_DEPTH`) is separate: the definition walkers
//! recurse one stack frame per parse-tree level, and tree-sitter's error
//! recovery on adversarial input produces pathologically deep trees (a 1 KiB run
//! of unbalanced C punctuation parses to depth 853, measured) that overflow the
//! stack (a local ASAN fuzz run hits exactly this in `walk_c_defs` at a 4 KiB
//! input) or, on a larger stack, recurse deep enough to exhaust the heap — the
//! OOM. Those inputs are small (≤ 4 KiB, libFuzzer's ceiling), so neither the
//! byte cap nor the parse timeout catches them; a depth guard at the parse
//! boundary does. The largest real source file in the eval corpus is depth 26.
//!
//! These tests pin both bounds' contracts WITHOUT needing to reproduce the 4 GB
//! OOM: an oversized adversarial input is rejected before the expensive parse,
//! a too-DEEP tree is rejected before the walk (the falsifiable regression —
//! pre-fix the walker descends it and returns `Ok`), an extreme-depth input is
//! handled without a stack overflow, and real files still parse.

use std::time::Instant;

use ai_architect_mcp::parser::{parse_file, Language, MAX_PARSE_BYTES, MAX_TREE_DEPTH};

/// Builds a C source whose parse tree is at least `nesting` levels deep: a run
/// of `nesting` open braces then the matching closes. tree-sitter nests one
/// level per unmatched brace, so the tree depth tracks `nesting` (measured:
/// 600 braces → tree depth 602).
fn deeply_nested_source(nesting: usize) -> String {
    let mut s = String::with_capacity(nesting * 2);
    s.push_str(&"{".repeat(nesting));
    s.push_str(&"}".repeat(nesting));
    s
}

/// The pathological token pattern the fuzzer evolved toward: C operators and
/// preprocessor punctuation with no valid structure, which drives tree-sitter's
/// error recovery into superlinear work (measured: 28 ms @ 2.6 KB, 378 ms @
/// 10 KB, hits the 5 s timeout at ~42 KB).
const SOUP_UNIT: &str = "**********+++*******************{;.+++.*;";

/// Builds an adversarial C source of at least `min_bytes` bytes.
fn adversarial_source(min_bytes: usize) -> String {
    let reps = min_bytes / SOUP_UNIT.len() + 1;
    SOUP_UNIT.repeat(reps)
}

/// THE regression. An input above `MAX_PARSE_BYTES` must be rejected before
/// tree-sitter runs, i.e. in well under the 5 s parse timeout.
///
/// Pre-fix (no boundary cap) this input is handed to tree-sitter and spends the
/// entire `PARSE_TIMEOUT_MICROS` (5 s) in superlinear error recovery before the
/// timeout cancels it and returns `Err` — so the elapsed-time assertion FAILS on
/// pre-fix code. Post-fix the cap returns `Err` in microseconds. The generous
/// 500 ms bound leaves no ambiguity between the two regimes (µs vs 5000 ms)
/// while staying robust on a loaded CI runner.
#[test]
fn oversized_input_is_rejected_before_the_expensive_parse() {
    let source = adversarial_source(2 * MAX_PARSE_BYTES as usize); // ~2 MiB
    assert!(source.len() as u64 > MAX_PARSE_BYTES);

    let start = Instant::now();
    let result = parse_file(&source, "fuzz.input", Language::C);
    let elapsed = start.elapsed();

    let msg = match result {
        Err(m) => m,
        Ok(_) => panic!("an input larger than MAX_PARSE_BYTES must be rejected with Err"),
    };
    assert!(
        msg.contains("oversized") && msg.contains("MAX_PARSE_BYTES"),
        "oversized rejection must name the cap it tripped, got: {msg:?}"
    );
    assert!(
        elapsed.as_millis() < 500,
        "oversized input must be rejected BEFORE the parse (no 5 s timeout spin); \
         took {elapsed:?} — pre-fix code spends the full parse timeout here"
    );
}

/// The cap must reject the byte AFTER the limit and admit the byte AT the limit,
/// so the boundary is exact (a mutant that flips `>` to `>=` is killed here).
#[test]
fn cap_boundary_is_exact() {
    // Exactly MAX_PARSE_BYTES: admitted (parsed, not rejected as oversized). The
    // content is valid, tiny-structure C so the parse itself is cheap and Ok.
    let at_cap = {
        let mut s = String::from("int x = 0;\n");
        s.push_str(&"// pad\n".repeat((MAX_PARSE_BYTES as usize - s.len()) / 7));
        while (s.len() as u64) < MAX_PARSE_BYTES {
            s.push(' ');
        }
        s.truncate(MAX_PARSE_BYTES as usize);
        s
    };
    assert_eq!(at_cap.len() as u64, MAX_PARSE_BYTES);
    let at = parse_file(&at_cap, "at_cap.c", Language::C);
    assert!(
        at.is_ok(),
        "a source of exactly MAX_PARSE_BYTES must NOT be rejected as oversized"
    );

    // One byte over: rejected.
    let mut over = at_cap;
    over.push(' ');
    assert_eq!(over.len() as u64, MAX_PARSE_BYTES + 1);
    let res = parse_file(&over, "over.c", Language::C);
    assert!(
        res.is_err_and(|m| m.contains("oversized")),
        "a source one byte over MAX_PARSE_BYTES must be rejected as oversized"
    );
}

/// The cap must not drop real source files. The largest file in the eval corpus
/// is ~47 KB; this builds a valid ~900 KB C translation unit (well under the
/// 1 MiB cap, and larger than any real fixture) and asserts it parses and yields
/// symbols — a guard against a cap set too low.
#[test]
fn large_but_valid_source_under_the_cap_still_parses() {
    let mut src = String::new();
    let mut i = 0usize;
    while (src.len() as u64) < 900 * 1024 {
        src.push_str(&format!(
            "int function_{i}(int a, int b) {{ return a + b + {i}; }}\n"
        ));
        i += 1;
    }
    assert!(
        (src.len() as u64) < MAX_PARSE_BYTES,
        "fixture must stay under the cap"
    );
    assert!(
        src.len() > 47_641,
        "fixture must exceed the largest real corpus file"
    );

    let result = parse_file(&src, "big_valid.c", Language::C)
        .expect("a valid source under the cap must parse");
    assert!(
        !result.nodes.is_empty(),
        "a valid ~900 KB C unit under the cap must still yield symbols"
    );
    assert_eq!(
        result.parse_errors, 0,
        "a valid C unit must parse without errors"
    );
}

/// THE depth regression. A parse tree deeper than `MAX_TREE_DEPTH` must be
/// rejected before the recursive walker descends it. The nesting here (600) is
/// just over the 512 bound but shallow enough that the walker would NOT overflow
/// a test thread's stack — so pre-fix (no depth guard) the walker descends it and
/// returns `Ok`, and this assertion FAILS; post-fix it returns the depth `Err`.
#[test]
fn deep_tree_is_rejected_before_the_walk() {
    let source = deeply_nested_source(600);
    let result = parse_file(&source, "deep.c", Language::C);
    let msg = match result {
        Err(m) => m,
        Ok(_) => panic!("a parse tree deeper than MAX_TREE_DEPTH must be rejected with Err"),
    };
    assert!(
        msg.contains("parse_tree_too_deep"),
        "depth rejection must name the guard it tripped, got: {msg:?}"
    );
}

/// An extreme-depth input (50 000 levels) must be rejected quickly and WITHOUT a
/// stack overflow — the guard's traversal is iterative, and it rejects before the
/// recursive walker (which would abort the process on a tree this deep) ever
/// runs. This input is ~100 KB (under the byte cap), so the depth guard, not the
/// size cap, is what handles it. Pre-fix this test would crash the runner.
#[test]
fn extreme_depth_input_does_not_overflow_the_stack() {
    let source = deeply_nested_source(50_000);
    assert!(
        (source.len() as u64) < MAX_PARSE_BYTES,
        "must be under the byte cap"
    );
    let start = Instant::now();
    let result = parse_file(&source, "very_deep.c", Language::C);
    assert!(
        result.is_err(),
        "an extreme-depth tree must be rejected, not walked"
    );
    assert!(
        start.elapsed().as_millis() < 500,
        "the depth guard must reject an extreme-depth tree promptly"
    );
}

/// A tree nested UNDER the bound must still parse (the guard must not reject
/// ordinary nesting). 200 levels is ~8× the deepest real corpus file (26) yet
/// well under the 512 bound, so it is accepted.
#[test]
fn nesting_under_the_bound_still_parses() {
    let source = deeply_nested_source(200);
    let result = parse_file(&source, "moderate.c", Language::C);
    assert!(
        result.is_ok(),
        "nesting under MAX_TREE_DEPTH must not be rejected"
    );
}

/// The depth bound is EXACT: a tree of depth exactly `MAX_TREE_DEPTH` is
/// accepted; one level deeper is rejected. This pins `depth > limit` against the
/// off-by-one mutants (`>=` / `==`), which would reject the exactly-at-bound tree.
///
/// tree-sitter-c nests one level per unmatched brace atop a two-node root
/// wrapper (measured: N braces → tree depth N + 2), so `N = MAX_TREE_DEPTH - 2`
/// yields a tree of depth exactly `MAX_TREE_DEPTH`. The paired rejection of
/// `N + 1` also fails loudly (rather than silently) if that offset ever drifts.
#[test]
fn depth_bound_is_exact() {
    let at_bound = deeply_nested_source(MAX_TREE_DEPTH - 2); // tree depth == MAX_TREE_DEPTH
    assert!(
        parse_file(&at_bound, "at_bound.c", Language::C).is_ok(),
        "a tree of depth exactly MAX_TREE_DEPTH must be accepted, not rejected"
    );

    let over_bound = deeply_nested_source(MAX_TREE_DEPTH - 1); // tree depth == MAX_TREE_DEPTH + 1
    assert!(
        parse_file(&over_bound, "over_bound.c", Language::C)
            .is_err_and(|m| m.contains("parse_tree_too_deep")),
        "a tree one level past MAX_TREE_DEPTH must be rejected"
    );
}

/// The two committed libFuzzer artifacts (issue #148) are tiny and must parse
/// bounded and fast through the boundary for every language the target selects —
/// they are seeds, not themselves OOM triggers, and must never panic.
#[test]
fn committed_reproducers_parse_bounded() {
    // Decoded bytes of the two base64 artifacts on issue #148. Their raw form is
    // also committed under fuzz/corpus/parse_file_utf8/ as regression seeds.
    let oom_input: &[u8] = &[
        0xf9, 0x60, 0x00, 0x3b, 0x77, 0x00, 0x00, 0x47, 0x0a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x2a, 0x2b, 0x2b, 0x2b, 0x2a, 0x2a, 0x77, 0x00, 0x00, 0x47, 0x0a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2b, 0x2b, 0x2b, 0x2a, 0x2a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x2a, 0x7b, 0x3b, 0x2e, 0x2b, 0x2b, 0x2b, 0x2e, 0x2a, 0x3b,
    ];
    let slow_unit: &[u8] = &[
        0xf9, 0x60, 0x00, 0x3b, 0x77, 0x00, 0x00, 0x47, 0x0a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x2a, 0x2b, 0x2b, 0x2b, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a, 0x2a,
        0x2a, 0x2a, 0x2a, 0x7b, 0x3b, 0x2e, 0x2b, 0x2b, 0x2b, 0x2b, 0x2b, 0x23, 0x2b, 0x2b, 0x2e,
        0x2a, 0x3b,
    ];

    let langs = [
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
    for artifact in [oom_input, slow_unit] {
        let source = String::from_utf8_lossy(artifact);
        for lang in langs {
            let start = Instant::now();
            // Must not panic; Ok or Err both acceptable (matches the harness).
            let _ = parse_file(&source, "fuzz.input", lang);
            assert!(
                start.elapsed().as_millis() < 500,
                "a committed reproducer must parse bounded for {lang:?}"
            );
        }
    }
}
