// parser::spec::structural_derive_tests — issue #232's proof obligations:
// (1) a CONTROL result on languages the engine already handles (derivation
// must rediscover the known-correct rule, or no result on SQL/PowerShell is
// trustworthy), and (2) the measured resolution rates on SQL and
// PowerShell, the two languages #224/#231 found entirely unreached.
//
// All four corpora are REAL, small, permissively-licensed repositories,
// fetched (not vendored — see `scripts/fetch-derivation-corpus.sh`'s module
// doc for URLs/commit SHAs/licences) into a scratch directory read here via
// `AP_DERIVE_CORPUS_DIR` (default `/tmp/ap-derive-corpus`). These are
// `#[ignore]`d by default (network + local-fetch dependency, consistent with
// this workspace's other network-dependent tests) and are NOT part of the
// `cargo test --lib` gate — run explicitly:
//   scripts/fetch-derivation-corpus.sh
//   cargo test --lib structural_derive_tests -- --ignored --nocapture

use std::fs;
use std::path::PathBuf;

use super::structural_derive::{parse_corpus, DerivationOutcome, Source};
use super::structural_derive_import::derive_import_rule;
use super::structural_derive_pairing::{derive_definition_and_call_rules, PairOutcome};

fn report_import(label: &str, outcome: &DerivationOutcome) {
    match outcome {
        DerivationOutcome::Resolved(sc) => eprintln!(
            "{label}: RESOLVED kind={:?} source={:?} rate={:.2} ({}/{})",
            sc.candidate.node_kind, sc.candidate.source, sc.rate, sc.matched, sc.total
        ),
        DerivationOutcome::Unresolved {
            candidates_tried,
            best_rate,
        } => eprintln!(
            "{label}: UNRESOLVED — {candidates_tried} candidates tried, best_rate={best_rate:.2} (< threshold)"
        ),
    }
}

fn corpus_dir() -> PathBuf {
    std::env::var("AP_DERIVE_CORPUS_DIR")
        .unwrap_or_else(|_| "/tmp/ap-derive-corpus".to_string())
        .into()
}

/// Reads every regular file directly under `dir` (non-recursive — each
/// language's corpus is a flat handful of files) except `COMMIT_SHA.txt`,
/// returning `(relative_path, contents)` owned pairs. `None` when `dir`
/// itself is absent — the honest "corpus not fetched" signal, distinct from
/// an empty corpus.
fn read_flat_corpus(dir: &std::path::Path) -> Option<Vec<(String, String)>> {
    if !dir.is_dir() {
        return None;
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        if name == "COMMIT_SHA.txt" {
            continue;
        }
        let contents = fs::read_to_string(&path).ok()?;
        out.push((name, contents));
    }
    Some(out)
}

fn report_pairs(
    label: &str,
    outcome: &PairOutcome,
    top: &[super::structural_derive_pairing::PairCandidate],
) {
    match outcome {
        PairOutcome::Resolved(p) => eprintln!(
            "{label}: RESOLVED def=({:?},{:?}) rate={:.2} ({}/{})  call=({:?},{:?}) rate={:.2} ({}/{})",
            p.def.node_kind, p.def.source, p.def_rate, p.def_matched, p.def_total,
            p.call.node_kind, p.call.source, p.call_rate, p.call_matched, p.call_total,
        ),
        PairOutcome::Unresolved { pairs_tried, best_min_rate } => eprintln!(
            "{label}: UNRESOLVED — {pairs_tried} pairs tried, best_min_rate={best_min_rate:.2} (< threshold)"
        ),
    }
    for p in top.iter().take(5) {
        eprintln!(
            "  pair def=({:?},{:?}) def_rate={:.2}  call=({:?},{:?}) call_rate={:.2}",
            p.def.node_kind, p.def.source, p.def_rate, p.call.node_kind, p.call.source, p.call_rate
        );
    }
}

/// CONTROL — Go (`dustin/go-humanize`, MIT). The known-correct rule, per
/// `structural.rs`'s own hand-verified global table: definitions use the
/// `name` field on `function_declaration`; calls use the `function` field on
/// `call_expression`. Derivation must rediscover both from resolution alone.
#[test]
#[ignore = "network + local corpus fetch — run scripts/fetch-derivation-corpus.sh first"]
fn derives_known_correct_rule_for_go() {
    let dir = corpus_dir().join("go");
    let Some(files) = read_flat_corpus(&dir) else {
        eprintln!("SKIPPED (corpus not fetched): {}", dir.display());
        return;
    };
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let corpus = parse_corpus(|| tree_sitter_go::LANGUAGE.into(), &refs);
    assert!(!corpus.is_empty(), "go corpus failed to parse");

    let (outcome, pairs) = derive_definition_and_call_rules(&corpus);
    let mut sorted = pairs;
    sorted.sort_by(|a, b| {
        b.def_rate
            .min(b.call_rate)
            .partial_cmp(&a.def_rate.min(a.call_rate))
            .unwrap()
    });
    report_pairs("go", &outcome, &sorted);

    let PairOutcome::Resolved(winner) = &outcome else {
        panic!("control failure: Go definitions/calls did not resolve jointly — derivation is broken, no SQL/PowerShell result is trustworthy");
    };
    assert_eq!(winner.def.node_kind, "function_declaration");
    assert_eq!(winner.def.source, Source::Field("name".to_string()));
    assert_eq!(winner.call.source, Source::Field("function".to_string()));
    // NOT 1.0: most call sites in real code call EXTERNAL (stdlib/imported)
    // functions, not ones defined in this small corpus — measured at 0.35
    // def_rate / 0.54 call_rate here, still the highest-ranked pair among
    // every candidate (see the `sorted` report above).
    assert!(winner.def_rate >= super::structural_derive::RESOLUTION_THRESHOLD);
    assert!(winner.call_rate >= super::structural_derive::RESOLUTION_THRESHOLD);
}

/// CONTROL — Python (`tartley/colorama`, BSD-3-Clause). Known-correct rule:
/// definitions use `name` on `function_definition`; calls use `function` on
/// `call`.
#[test]
#[ignore = "network + local corpus fetch — run scripts/fetch-derivation-corpus.sh first"]
fn derives_known_correct_rule_for_python() {
    let dir = corpus_dir().join("python");
    let Some(files) = read_flat_corpus(&dir) else {
        eprintln!("SKIPPED (corpus not fetched): {}", dir.display());
        return;
    };
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let corpus = parse_corpus(|| tree_sitter_python::LANGUAGE.into(), &refs);
    assert!(!corpus.is_empty(), "python corpus failed to parse");

    let (outcome, pairs) = derive_definition_and_call_rules(&corpus);
    let mut sorted = pairs;
    sorted.sort_by(|a, b| {
        b.def_rate
            .min(b.call_rate)
            .partial_cmp(&a.def_rate.min(a.call_rate))
            .unwrap()
    });
    report_pairs("python", &outcome, &sorted);

    let PairOutcome::Resolved(winner) = &outcome else {
        panic!("control failure: Python definitions/calls did not resolve jointly");
    };
    assert_eq!(winner.def.node_kind, "function_definition");
    assert_eq!(winner.def.source, Source::Field("name".to_string()));
    assert_eq!(winner.call.source, Source::Field("function".to_string()));
    assert!(winner.def_rate >= super::structural_derive::RESOLUTION_THRESHOLD);
    assert!(winner.call_rate >= super::structural_derive::RESOLUTION_THRESHOLD);
}

/// TARGET — SQL (`chlordk/pg_get_tabledef`, MIT). #224/#231 measured this
/// language as reaching NEITHER definitions nor calls generically (see
/// `structural_held_out_sample_tests.rs`'s own "why SQL misses BOTH columns"
/// note). This test reports whatever the derivation mechanism actually
/// measures — it does not assert a particular verdict either way, per #232's
/// "report UNRESOLVED... never a plausible-but-unvalidated rule."
#[test]
#[ignore = "network + local corpus fetch — run scripts/fetch-derivation-corpus.sh first"]
fn measures_sql_resolution() {
    let dir = corpus_dir().join("sql");
    let Some(files) = read_flat_corpus(&dir) else {
        eprintln!("SKIPPED (corpus not fetched): {}", dir.display());
        return;
    };
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let corpus = parse_corpus(|| tree_sitter_sequel::LANGUAGE.into(), &refs);
    assert!(!corpus.is_empty(), "sql corpus failed to parse");

    eprintln!("\n== SQL derivation report (chlordk/pg_get_tabledef) ==");
    let (outcome, pairs) = derive_definition_and_call_rules(&corpus);
    let mut sorted = pairs;
    sorted.sort_by(|a, b| {
        b.def_rate
            .min(b.call_rate)
            .partial_cmp(&a.def_rate.min(a.call_rate))
            .unwrap()
    });
    report_pairs("sql defs+calls", &outcome, &sorted);

    let (import_outcome, import_scored) = derive_import_rule(&corpus);
    report_import("sql imports", &import_outcome);
    for sc in import_scored.iter().take(3) {
        eprintln!(
            "  import candidate kind={:?} source={:?} rate={:.2} ({}/{})",
            sc.candidate.node_kind, sc.candidate.source, sc.rate, sc.matched, sc.total
        );
    }
}

/// TARGET — PowerShell (`dahlbyk/posh-git`, MIT). Same non-asserting,
/// report-everything shape as `measures_sql_resolution`.
#[test]
#[ignore = "network + local corpus fetch — run scripts/fetch-derivation-corpus.sh first"]
fn measures_powershell_resolution() {
    let dir = corpus_dir().join("powershell");
    let Some(files) = read_flat_corpus(&dir) else {
        eprintln!("SKIPPED (corpus not fetched): {}", dir.display());
        return;
    };
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, s)| (p.as_str(), s.as_str()))
        .collect();
    let corpus = parse_corpus(|| tree_sitter_powershell::LANGUAGE.into(), &refs);
    assert!(!corpus.is_empty(), "powershell corpus failed to parse");

    eprintln!("\n== PowerShell derivation report (dahlbyk/posh-git) ==");
    let (outcome, pairs) = derive_definition_and_call_rules(&corpus);
    let mut sorted = pairs;
    sorted.sort_by(|a, b| {
        b.def_rate
            .min(b.call_rate)
            .partial_cmp(&a.def_rate.min(a.call_rate))
            .unwrap()
    });
    report_pairs("powershell defs+calls", &outcome, &sorted);

    let (import_outcome, import_scored) = derive_import_rule(&corpus);
    report_import("powershell imports", &import_outcome);
    for sc in import_scored.iter().take(3) {
        eprintln!(
            "  import candidate kind={:?} source={:?} rate={:.2} ({}/{})",
            sc.candidate.node_kind, sc.candidate.source, sc.rate, sc.matched, sc.total
        );
    }
}

#[test]
fn thresholds_are_measured_not_arbitrary() {
    // Pin the two constants this module's own doc cites as
    // measured-from-control-run so a silent future edit is caught here,
    // not just in the (ignored, network-dependent) reports above.
    assert_eq!(super::structural_derive::RESOLUTION_THRESHOLD, 0.25);
    assert_eq!(
        super::structural_derive::DEFINITION_DISTINCTNESS_THRESHOLD,
        0.5
    );
}
