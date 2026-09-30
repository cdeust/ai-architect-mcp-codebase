//! `doc_claims` end to end on a real indexed Cargo package: every kind, every
//! verdict, the anchor, and the rule that an absence or a lower-bound count
//! never yields `contradicted`.
use ai_architect_mcp::doc_claims::{check_claims, Claim, Report, Verdict};
use ai_architect_mcp::{graph_store::GraphStore, indexer, resolver};
use std::fs;
use std::path::PathBuf;

const LIB: &str = r#"pub struct TaskSet;

impl TaskSet {
    pub fn response_of(&self) -> u32 {
        1
    }
    fn hidden(&self) {}
}

pub fn api() {}
pub(crate) fn internal() {}
fn private_fn() {}

pub enum Refusal {
    A,
    B(u8),
    C { x: u8 },
}

pub enum Gated {
    A,
    #[cfg(unix)]
    B,
}

pub mod one {
    pub fn dup() {}
}

pub mod two {
    pub fn dup() {}
}

#[cfg(feature = "extra")]
mod extra {
    #[test]
    fn feature_test() {}
}

#[cfg(kani)]
mod proofs {
    #[kani::proof]
    fn p() {}
}

#[cfg(test)]
mod tests {
    #[test]
    fn t1() {}

    #[test]
    fn t2() {}

    fn helper() {
        #[test]
        fn nested() {}
    }
}
"#;

const README: &str = "# fx\n\
tests: 7\n\
The 3 tests.\n\
The 5 tests.\n\
The 40 tests.\n\
At least 4 tests.\n\
Two integration tests.\n\
One proof.\n\
No proof at all.\n\
`TaskSet::response_of`, `api`, `internal`, `private_fn`, `TaskSet::hidden`, `dup`, `nothing_here`.\n\
Three refusals; four refusals; two refusals.\n\
Gated has 2, or 3, or at least 1.\n\
Files: src/lib.rs, LICENSE, nope.txt, ../outside.\n\
Symbols: src/lib.rs::api, TaskSet, Missing, src/gone.sh::x, src/lib.rs::tests.\n\
Unknown kind: 0 panics.\n";

struct Project {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    graph: PathBuf,
}

fn project() -> Project {
    build(&[
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", LIB),
        (
            "tests/it.rs",
            "#[test]\nfn i1() {}\n\n#[test]\nfn i2() {}\n",
        ),
        (
            "examples/ex.rs",
            "fn main() {}\n\n#[test]\nfn ex_test() {}\n",
        ),
        ("loose/stray.rs", "#[test]\nfn stray() {}\n"),
        ("LICENSE", "MIT\n"),
        ("README.md", README),
    ])
}

fn build(files: &[(&str, &str)]) -> Project {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("repo");
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(path, text).expect("write");
    }
    fs::write(tmp.path().join("outside"), "secret\n").expect("outside");
    let graph = tmp.path().join("graph");
    indexer::index_codebase(&root, &graph).expect("index");
    resolver::resolve_graph(&GraphStore::open_or_create(&graph).expect("open")).expect("resolve");
    Project {
        _tmp: tmp,
        root,
        graph,
    }
}

/// A README claim: `(line, text)` is the anchor, `(subject, expected)` what
/// the rule of `kind` checks.
fn claim(
    id: &str,
    (line, text): (u64, &str),
    kind: &str,
    (subject, expected): (&str, &str),
) -> Claim {
    Claim {
        id: id.into(),
        text: text.into(),
        file: "README.md".into(),
        line,
        kind: kind.into(),
        subject: subject.into(),
        expected: expected.into(),
    }
}

fn run(p: &Project, claims: Vec<Claim>) -> Report {
    let store = GraphStore::open_or_create(&p.graph).expect("open");
    check_claims(&store, &p.root, claims).expect("check")
}

fn verdict_of(report: &Report, id: &str) -> (Verdict, String) {
    let row = report
        .rows
        .iter()
        .find(|r| r.claim.id == id)
        .unwrap_or_else(|| panic!("no row {id}"));
    (
        row.outcome.verdict,
        row.outcome.reason.clone().unwrap_or_default(),
    )
}

fn assert_verdict(report: &Report, id: &str, verdict: Verdict, reason_prefix: &str) {
    let (got, reason) = verdict_of(report, id);
    let row = report.rows.iter().find(|r| r.claim.id == id).expect("row");
    assert_eq!(got, verdict, "{id}: {}", row.to_json());
    assert!(
        reason.starts_with(reason_prefix),
        "{id}: reason {reason:?} does not start with {reason_prefix:?}"
    );
}

/// Declared: feature_test, t1, t2, i1, i2, ex_test, stray (a `#[test]` nested
/// in a function is not an entry point in the graph). The floor keeps t1, t2
/// (gate `test`), i1, i2 and leaves out a feature gate, an example target and
/// a file no target reaches.
fn assert_floor_evidence(r: &Report) {
    let all = r
        .rows
        .iter()
        .find(|row| row.claim.id == "all")
        .expect("all");
    let floor = all.outcome.evidence[2]["compiled_floor"].as_u64();
    assert_eq!(floor, Some(4), "{}", all.to_json());
    let declared = all.outcome.evidence[1]["result"]["declared"]
        .as_u64()
        .expect("declared");
    assert_eq!(declared, 7, "{}", all.to_json());
    let left_out = &all.outcome.evidence[2]["left_out_of_floor"];
    for why in [
        "gated(feature=extra)",
        "target_not_run_by_harness(example)",
        "target_not_run_by_harness(none)",
    ] {
        assert_eq!(left_out[why], 1, "{why}: {left_out}");
    }
}

#[test]
fn test_counts_are_contradicted_only_by_the_compiled_floor() {
    let p = project();
    let r = run(
        &p,
        vec![
            claim("all", (2, "tests: 7"), "test_count", ("", "7")),
            claim("three", (3, "3 tests"), "test_count", ("", "3")),
            claim("five", (4, "5 tests"), "test_count", ("", "5")),
            claim("forty", (5, "40 tests"), "test_count", ("", "40")),
            claim(
                "atleast",
                (6, "At least 4 tests"),
                "test_count",
                ("", ">=4"),
            ),
            claim(
                "it",
                (7, "Two integration tests"),
                "test_count",
                ("tests/", "2"),
            ),
            claim(
                "lib",
                (7, "Two integration tests"),
                "test_count",
                ("src/lib.rs", "2"),
            ),
        ],
    );
    assert_floor_evidence(&r);
    assert_verdict(&r, "all", Verdict::NotVerifiable, "build_dependent");
    assert_verdict(&r, "three", Verdict::Contradicted, "floor_exceeds_claim");
    assert_verdict(&r, "five", Verdict::NotVerifiable, "build_dependent");
    assert_verdict(&r, "forty", Verdict::NotVerifiable, "lower_bound");
    assert_verdict(&r, "atleast", Verdict::Supported, "");
    assert_verdict(&r, "it", Verdict::Supported, "");
    // Seven declared match "7", but the build runs four: not supported.
    // src/lib.rs declares 3 (t1, t2, feature_test) with a floor of 2.
    assert_verdict(&r, "lib", Verdict::NotVerifiable, "build_dependent");
}

#[test]
fn a_proof_count_uses_the_kani_option_as_the_harness_gate() {
    let p = project();
    let r = run(
        &p,
        vec![
            claim("one", (8, "One proof"), "proof_count", ("", "1")),
            claim("none", (9, "No proof at all"), "proof_count", ("", "0")),
        ],
    );
    assert_verdict(&r, "one", Verdict::Supported, "");
    assert_verdict(&r, "none", Verdict::Contradicted, "floor_exceeds_claim");
}

/// Issue #423: a harness in a file only `#[cfg(kani)] mod` declares has the file
/// context `proof`; `cargo kani` verifies it, so it stays in the compiled floor.
#[test]
fn a_proof_in_a_file_only_a_kani_module_declares_is_in_the_floor() {
    let p = build(&[
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "#[cfg(kani)]\n#[path = \"../kani/h.rs\"]\nmod h;\n",
        ),
        ("kani/h.rs", "#[kani::proof]\nfn one() {}\n"),
        ("README.md", "One proof\nNo proof\n"),
    ]);
    let r = run(
        &p,
        vec![
            claim("exact", (1, "One proof"), "proof_count", ("", "1")),
            claim("none", (2, "No proof"), "proof_count", ("", "0")),
        ],
    );
    assert_verdict(&r, "exact", Verdict::Supported, "");
    assert_verdict(&r, "none", Verdict::Contradicted, "floor_exceeds_claim");
}

#[test]
fn visibility_is_supported_contradicted_or_undecided_from_the_declaration() {
    let p = project();
    let line = "`TaskSet::response_of`, `api`";
    let r = run(
        &p,
        vec![
            claim("m", (10, line), "is_public", ("TaskSet::response_of", "")),
            claim("f", (10, line), "is_public", ("api", "")),
            claim("crate", (10, line), "is_public", ("internal", "")),
            claim("priv", (10, line), "is_public", ("private_fn", "")),
            claim("hidden", (10, line), "is_public", ("TaskSet::hidden", "")),
            claim("dup", (10, line), "is_public", ("dup", "")),
            claim("gone", (10, line), "is_public", ("nothing_here", "")),
        ],
    );
    assert_verdict(&r, "m", Verdict::Supported, "");
    assert_verdict(&r, "f", Verdict::Supported, "");
    assert_verdict(&r, "crate", Verdict::Contradicted, "declared `pub(crate)`");
    assert_verdict(&r, "priv", Verdict::Contradicted, "declared without `pub`");
    assert_verdict(
        &r,
        "hidden",
        Verdict::NotVerifiable,
        "method_without_modifier",
    );
    assert_verdict(&r, "dup", Verdict::NotVerifiable, "ambiguous_subject");
    assert_verdict(&r, "gone", Verdict::NotFound, "absent_from_graph");
}

#[test]
fn enum_variant_counts_are_complete_unless_a_variant_has_its_own_cfg() {
    let p = project();
    let line = "Three refusals";
    let gated = "Gated has 2";
    let r = run(
        &p,
        vec![
            claim("three", (11, line), "enum_variant_count", ("Refusal", "3")),
            claim("four", (11, line), "enum_variant_count", ("Refusal", "4")),
            claim(
                "two",
                (11, line),
                "enum_variant_count",
                ("src/lib.rs::Refusal", "2"),
            ),
            claim("g2", (12, gated), "enum_variant_count", ("Gated", "2")),
            claim("g3", (12, gated), "enum_variant_count", ("Gated", "3")),
            claim("g1", (12, gated), "enum_variant_count", ("Gated", ">=1")),
            claim(
                "gone",
                (12, gated),
                "enum_variant_count",
                ("NoSuchEnum", "2"),
            ),
        ],
    );
    assert_verdict(&r, "three", Verdict::Supported, "");
    assert_verdict(&r, "four", Verdict::Contradicted, "variant_count_differs");
    assert_verdict(&r, "two", Verdict::Contradicted, "variant_count_differs");
    assert_verdict(&r, "g2", Verdict::NotVerifiable, "build_dependent");
    assert_verdict(&r, "g3", Verdict::Contradicted, "variant_count_differs");
    assert_verdict(&r, "g1", Verdict::Supported, "");
    assert_verdict(&r, "gone", Verdict::NotFound, "absent_from_graph");
}

#[test]
fn existence_is_supported_or_not_found_never_contradicted() {
    let p = project();
    let files = "Files: src/lib.rs";
    let syms = "Symbols: src/lib.rs::api";
    let r = run(
        &p,
        vec![
            claim("lib", (13, files), "file_exists", ("src/lib.rs", "")),
            claim("license", (13, files), "file_exists", ("LICENSE", "")),
            claim("nope", (13, files), "file_exists", ("nope.txt", "")),
            claim("escape", (13, files), "file_exists", ("../outside", "")),
            claim("api", (14, syms), "symbol_exists", ("src/lib.rs::api", "")),
            claim("ts", (14, syms), "symbol_exists", ("TaskSet", "")),
            claim("missing", (14, syms), "symbol_exists", ("Missing", "")),
            claim("sh", (14, syms), "symbol_exists", ("src/gone.sh::x", "")),
            claim(
                "mod",
                (14, syms),
                "module_exists",
                ("src/lib.rs::tests", ""),
            ),
            claim(
                "notmod",
                (14, syms),
                "module_exists",
                ("src/lib.rs::api", ""),
            ),
        ],
    );
    assert_verdict(&r, "lib", Verdict::Supported, "");
    assert_verdict(&r, "license", Verdict::Supported, "");
    assert_verdict(&r, "nope", Verdict::NotFound, "absent");
    assert_verdict(&r, "escape", Verdict::NotVerifiable, "subject_path_refused");
    assert_verdict(&r, "api", Verdict::Supported, "");
    assert_verdict(&r, "ts", Verdict::Supported, "");
    assert_verdict(&r, "missing", Verdict::NotFound, "absent_from_graph");
    assert_verdict(&r, "sh", Verdict::NotVerifiable, "language not indexed");
    assert_verdict(&r, "mod", Verdict::Supported, "");
    assert_verdict(&r, "notmod", Verdict::NotFound, "absent_from_graph");
    assert!(r
        .rows
        .iter()
        .all(|row| row.outcome.verdict != Verdict::Contradicted));
}

#[test]
fn an_empty_subject_or_a_count_that_is_not_a_number_is_not_verifiable() {
    let p = project();
    let line = (13, "Files: src/lib.rs");
    let r = run(
        &p,
        vec![
            claim("sym", line, "symbol_exists", (" ", "")),
            claim("file", line, "file_exists", ("", "")),
            claim("pub", line, "is_public", ("", "")),
            claim("enum", line, "enum_variant_count", ("", "2")),
            claim("many", line, "test_count", ("", "many")),
            claim("variants", line, "enum_variant_count", ("Refusal", "some")),
        ],
    );
    for id in ["sym", "file", "pub", "enum"] {
        assert_verdict(&r, id, Verdict::NotVerifiable, "subject_empty");
    }
    assert_verdict(&r, "many", Verdict::NotVerifiable, "expected_not_a_count");
    assert_verdict(
        &r,
        "variants",
        Verdict::NotVerifiable,
        "expected_not_a_count",
    );
}

#[test]
fn an_anchor_that_does_not_hold_rejects_the_claim_before_any_rule() {
    let p = project();
    let mut outside = claim("outside", (1, "secret"), "file_exists", ("LICENSE", ""));
    outside.file = "../outside".into();
    let r = run(
        &p,
        vec![
            claim("wrongline", (2, "The 3 tests"), "test_count", ("", "3")),
            claim(
                "paraphrase",
                (3, "The three tests"),
                "test_count",
                ("", "3"),
            ),
            claim("far", (99, "The 3 tests"), "test_count", ("", "3")),
            outside,
            claim("kind", (15, "0 panics"), "no_panic", ("", "")),
        ],
    );
    assert_verdict(&r, "wrongline", Verdict::RejectedAnchor, "text_not_at_line");
    assert_verdict(
        &r,
        "paraphrase",
        Verdict::RejectedAnchor,
        "text_not_at_line",
    );
    assert_verdict(&r, "far", Verdict::RejectedAnchor, "line_out_of_range");
    assert_verdict(
        &r,
        "outside",
        Verdict::RejectedAnchor,
        "path_leaves_repo_root",
    );
    assert_verdict(&r, "kind", Verdict::NotVerifiable, "kind_not_supported_yet");
}

#[test]
fn the_report_is_sorted_counted_and_identical_across_runs() {
    let p = project();
    let claims = vec![
        claim("z", (14, "Missing"), "symbol_exists", ("Missing", "")),
        claim("a", (3, "3 tests"), "test_count", ("", "3")),
        claim("b", (3, "3 tests"), "test_count", ("", ">=1")),
    ];
    let first = run(&p, claims.clone());
    let second = run(&p, claims);
    let ids: Vec<&str> = first.rows.iter().map(|r| r.claim.id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "z"]);
    let render = |r: &Report| -> String {
        let rows: Vec<_> = r.rows.iter().map(|row| row.to_json()).collect();
        serde_json::to_string(&rows).expect("json")
    };
    assert_eq!(render(&first), render(&second));
    let counts = first.counts();
    assert_eq!(counts.len(), 5);
    assert_eq!(counts["contradicted"], 1);
    assert_eq!(counts["supported"], 1);
    assert_eq!(counts["not_found"], 1);
}
