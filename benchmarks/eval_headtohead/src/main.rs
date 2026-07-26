// eval_headtohead_bench — issue #64 falsifiable head-to-head evaluation.
//
// Runs the PRE-REGISTERED question set (questions.json / PRE_REGISTRATION.md)
// under two conditions — GRAPH (AP tools) and EXPLORER (Grep/Glob/Read
// baseline) — over the committed multi-language corpus, driving the REAL
// library. It writes results.json (aggregate + hypotheses + per-dimension /
// per-language, all with dispersion) and raw_results.json (every per-question
// row, including the ones GRAPH loses). The blinded LLM-judge answer-quality
// leg is config-gated (AP_EVAL_JUDGE_CMD); its absence is reported loudly and
// the deterministic legs still produce published numbers.
//
// Reproducible: the corpus is committed and content-hashed; the only
// nondeterminism (judge A/B order) is seeded. See MANIFEST.md.

mod conditions;
mod corpus;
mod judge;
mod metrics;
mod questions;
mod report;

use conditions::{explorer, graph};
use judge::{Judge, JudgeStatus};
use metrics::score;
use report::{CondResult, Outcome};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Warning banner emitted (to stderr) when the answer-quality judge leg is
/// gated off. Kept as a named constant so [`budget_gate_banner`] is a pure,
/// directly-testable status→banner mapping.
const BUDGET_GATED_BANNER: &str = "\n\
    ################################################################\n\
    # ANSWER-QUALITY JUDGE LEG: SKIPPED (BUDGET-GATED)             #\n\
    # AP_EVAL_JUDGE_CMD is unset — no LLM judge was run.           #\n\
    # Deterministic legs (tokens, tool calls, precision/recall/F1  #\n\
    # vs ground truth) ran fully and produced the published        #\n\
    # numbers. See PRE_REGISTRATION.md §7 to enable the judge.     #\n\
    ################################################################\n";

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus_root = manifest.join("corpus");
    let out_dir = out_dir(manifest);
    let set = questions::load(&manifest.join("questions.json")).expect("load questions.json");

    let hash = corpus::corpus_hash(&corpus_root);
    println!("== eval_headtohead_bench ==");
    println!("corpus_sha256: {hash}");
    println!("n_questions:   {}", set.questions.len());

    let judge = Judge::from_env(set.blinding_seed);
    if let Some(banner) = budget_gate_banner(judge.status()) {
        eprintln!("{banner}");
    }

    let outcomes = run_all(&set, &corpus_root, &judge);

    let results = report::build_results(&outcomes, judge.status());
    let raw = report::build_raw(&outcomes);
    write_json(&out_dir.join("results.json"), &results);
    write_json(&out_dir.join("raw_results.json"), &raw);

    for line in summary_lines(&results, judge.status()) {
        println!("{line}");
    }
}

/// Directory the two result artifacts are written to. Defaults to the crate
/// manifest dir (where the committed baseline lives); the `AP_EVAL_OUT_DIR`
/// environment variable overrides it, read once at startup, so CI and the
/// end-to-end test can redirect the artifacts without mutating tracked files.
/// source: issue #88 — a testable IO seam for the composition root.
fn out_dir(manifest: &Path) -> PathBuf {
    std::env::var_os("AP_EVAL_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.to_path_buf())
}

/// Maps the judge status to the stderr banner: `Some` warning text iff the
/// judge leg was budget-gated off, `None` when it ran. Pure so the gating
/// decision is unit-testable without capturing process stderr.
fn budget_gate_banner(status: JudgeStatus) -> Option<&'static str> {
    match status {
        JudgeStatus::SkippedBudgetGated => Some(BUDGET_GATED_BANNER),
        JudgeStatus::Ran => None,
    }
}

/// Run every question, indexing each language corpus exactly once.
fn run_all(set: &questions::QuestionSet, corpus_root: &Path, judge: &Judge) -> Vec<Outcome> {
    let mut outcomes = Vec::with_capacity(set.questions.len());
    let mut languages: Vec<String> = set.questions.iter().map(|q| q.language.clone()).collect();
    languages.sort();
    languages.dedup();

    for lang in &languages {
        let indexed = corpus::index_language(corpus_root, lang)
            .unwrap_or_else(|e| panic!("index {lang}: {e}"));
        let source = corpus::source_map(corpus_root, lang);
        for (i, q) in set
            .questions
            .iter()
            .enumerate()
            .filter(|(_, q)| &q.language == lang)
        {
            let g = graph::run(q, &indexed.store, &indexed.graph_path);
            let e = explorer::run(q, &corpus_root.join(lang));
            let judged = judge.grade(i, &q.question, &g.text, &e.text, &source);
            outcomes.push(Outcome {
                q: q.clone(),
                graph: to_result(&g, q, set.token_proxy_chars_per_token),
                explorer: to_result(&e, q, set.token_proxy_chars_per_token),
                judge: judged,
            });
        }
    }
    outcomes.sort_by(|a, b| a.q.id.cmp(&b.q.id));
    outcomes
}

/// Score a condition's answer against ground truth and package a CondResult.
fn to_result(a: &conditions::Answer, q: &questions::Question, proxy: usize) -> CondResult {
    let truth: BTreeSet<String> = q.ground_truth_files.iter().cloned().collect();
    CondResult {
        files: a.files.iter().cloned().collect(),
        tokens: a.tokens(proxy),
        tool_calls: a.tool_calls,
        retrieval: score(&a.files, &truth),
    }
}

fn write_json(path: &Path, value: &serde_json::Value) {
    let pretty = serde_json::to_string_pretty(value).expect("encode json");
    std::fs::write(path, pretty).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!("wrote {}", path.display());
}

/// Build the headline aggregate + hypothesis-verdict lines. Returns the lines
/// (rather than printing) so the summary content is unit-testable; `main`
/// prints each line in order. source: issue #88 — testable summary seam.
fn summary_lines(results: &serde_json::Value, status: JudgeStatus) -> Vec<String> {
    let mut lines = Vec::new();
    let agg = &results["aggregate"];
    lines.push("-- aggregate (mean ± stdev over all questions) --".to_string());
    for cond in ["graph", "explorer"] {
        let c = &agg[cond];
        lines.push(format!(
            "  {cond:8} precision {}±{}  recall {}±{}  tokens {}±{}  tool_calls {}±{}",
            c["precision"]["mean"],
            c["precision"]["stdev"],
            c["recall"]["mean"],
            c["recall"]["stdev"],
            c["tokens"]["mean"],
            c["tokens"]["stdev"],
            c["tool_calls"]["mean"],
            c["tool_calls"]["stdev"],
        ));
    }
    lines.push(format!(
        "  token ratio (explorer/graph):    {}",
        agg["token_ratio_explorer_over_graph"]["mean"]
    ));
    lines.push(format!(
        "  toolcall ratio (explorer/graph): {}",
        agg["toolcall_ratio_explorer_over_graph"]["mean"]
    ));
    lines.push("-- hypotheses --".to_string());
    for (name, h) in results["hypotheses"].as_object().unwrap() {
        lines.push(format!("  {name}: {}", h["result"]));
    }
    lines.push(format!(
        "-- answer-quality judge: {} --",
        results["answer_quality_judge"]["status"]
    ));
    let _ = status;
    lines
}

#[cfg(test)]
mod tests {
    //! End-to-end harness correctness + determinism (the CI gate — no wall-clock
    //! assertions, per the issue #74 lesson; only known-answer metrics and
    //! reproducibility of the retrieved sets).
    use super::*;
    use std::collections::BTreeSet;

    fn manifest() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    fn question(set: &questions::QuestionSet, id: &str) -> questions::Question {
        set.questions.iter().find(|q| q.id == id).unwrap().clone()
    }

    #[test]
    fn python_impact_is_precise_and_complete() {
        let m = manifest();
        let set = questions::load(&m.join("questions.json")).unwrap();
        let idx = corpus::index_language(&m.join("corpus"), "python").unwrap();
        let q = question(&set, "py-D2"); // who calls process_order
        let g = graph::run(&q, &idx.store, &idx.graph_path);
        let truth: BTreeSet<String> = q.ground_truth_files.iter().cloned().collect();
        let gr = score(&g.files, &truth);
        // Graph resolves exactly the two real callers: precision AND recall 1.0.
        assert_eq!(gr.precision, 1.0);
        assert_eq!(gr.recall, 1.0);
        assert_eq!(g.tool_calls, 1);
    }

    #[test]
    fn explorer_over_retrieves_on_distractors() {
        let m = manifest();
        let set = questions::load(&m.join("questions.json")).unwrap();
        let q = question(&set, "py-D2");
        let e = explorer::run(&q, &m.join("corpus/python"));
        let truth: BTreeSet<String> = q.ground_truth_files.iter().cloned().collect();
        let er = score(&e.files, &truth);
        // Substring grep finds every occurrence (recall 1.0) but the comment /
        // string distractors drop precision below 1.0.
        assert_eq!(er.recall, 1.0);
        assert!(er.precision < 1.0, "expected distractor precision loss");
        // And the baseline consumes strictly more context + tool calls.
        assert!(e.chars > 0 && e.tool_calls > 1);
    }

    #[test]
    fn graph_condition_is_deterministic() {
        let m = manifest();
        let set = questions::load(&m.join("questions.json")).unwrap();
        let q = question(&set, "py-D5");
        let a = corpus::index_language(&m.join("corpus"), "python").unwrap();
        let b = corpus::index_language(&m.join("corpus"), "python").unwrap();
        let ra = graph::run(&q, &a.store, &a.graph_path);
        let rb = graph::run(&q, &b.store, &b.graph_path);
        assert_eq!(ra.files, rb.files);
        assert_eq!(ra.chars, rb.chars);
    }

    #[test]
    fn corpus_hash_is_stable() {
        let m = manifest();
        let h1 = corpus::corpus_hash(&m.join("corpus"));
        let h2 = corpus::corpus_hash(&m.join("corpus"));
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // sha256 hex
    }

    /// Each GRAPH dimension answers through its own tool and retrieves the
    /// ground truth. Pins graph.rs op_define / op_impact_users /
    /// op_processes_main / op_search (against their `vec![]` mutants),
    /// op_processes_main's `entry_kind == "main"` filter (against `!=`),
    /// query_qns, and tool_name (against `""` / `"xyzzy"`). D2 (impact_callers)
    /// is already pinned by `python_impact_is_precise_and_complete`.
    #[test]
    fn graph_condition_answers_every_dimension_with_its_tool() {
        let m = manifest();
        let set = questions::load(&m.join("questions.json")).unwrap();
        let idx = corpus::index_language(&m.join("corpus"), "python").unwrap();

        let check = |id: &str, tool: &str, exact: bool| {
            let q = question(&set, id);
            let a = graph::run(&q, &idx.store, &idx.graph_path);
            assert!(!a.files.is_empty(), "{id}: graph answer must be non-empty");
            let truth: BTreeSet<String> = q.ground_truth_files.iter().cloned().collect();
            let r = score(&a.files, &truth);
            if exact {
                // recall 1.0 ⇒ every truth file retrieved; the process filter
                // `!= "main"` would drop the main entry point → recall < 1.0.
                assert_eq!(r.recall, 1.0, "{id}: recall");
                assert_eq!(r.precision, 1.0, "{id}: precision");
            }
            let envelope: serde_json::Value = serde_json::from_str(&a.text).unwrap();
            assert_eq!(envelope["tool"], tool, "{id}: tool name in envelope");
        };

        check("py-D1", "query_graph", true); // op_define + query_qns
        check("py-D3", "get_processes", true); // op_processes_main + filter ==
        check("py-D4", "get_impact", true); // op_impact_users
        check("py-D5", "search_codebase", false); // op_search (non-empty)
    }

    /// The composition root runs the full eval end-to-end and writes both
    /// artifacts to `AP_EVAL_OUT_DIR`. Pins `main` (against `-> ()`),
    /// `write_json` (against `-> ()`), `out_dir` (the env seam), and `run_all`
    /// (against `vec![]` and the `q.language == lang` filter's `!=` mutant,
    /// both of which collapse graph precision/recall away from the real 1.0 /
    /// 0.83). Redirects output to a temp dir so no tracked file is touched.
    #[test]
    fn main_writes_results_to_out_dir_and_graph_is_precise() {
        let out = tempfile::Builder::new()
            .prefix("eval_h2h_main_e2e")
            .tempdir()
            .expect("tempdir");
        // Read-once-at-startup override consumed only by `out_dir`; no other
        // test reads this var. Removed immediately after `main` returns.
        std::env::set_var("AP_EVAL_OUT_DIR", out.path());
        main();
        std::env::remove_var("AP_EVAL_OUT_DIR");

        let results: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.path().join("results.json"))
                .expect("results.json written"),
        )
        .expect("results.json parses");
        // raw_results.json is written too (a second write_json call).
        let _raw: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.path().join("raw_results.json"))
                .expect("raw_results.json written"),
        )
        .expect("raw_results.json parses");

        let g = &results["aggregate"]["graph"];
        let precision = g["precision"]["mean"]
            .as_f64()
            .expect("graph precision mean");
        let recall = g["recall"]["mean"].as_f64().expect("graph recall mean");
        // The GRAPH condition is exact-precision by construction and retrieves
        // most of the ground truth. `run_all` returning `vec![]` yields no
        // outcomes (means degrade off 1.0 / away from the real 0.83); the
        // language-filter `!=` grades each question against the wrong corpus
        // (recall collapses). Both violate these bounds.
        assert_eq!(precision, 1.0, "graph precision mean must be 1.0");
        assert!(
            recall >= 0.75,
            "graph recall mean must reflect real retrieval (got {recall})"
        );
    }

    /// `run_all` produces exactly one outcome per question, each preserving its
    /// own question, sorted by id. A second, direct pin on the `q.language ==
    /// lang` filter and the `vec![]` mutant, on a single-language subset so it
    /// is fast and independent of `main`'s full run.
    #[test]
    fn run_all_yields_one_outcome_per_question_in_id_order() {
        let m = manifest();
        let full = questions::load(&m.join("questions.json")).unwrap();
        let py: Vec<questions::Question> = full
            .questions
            .iter()
            .filter(|q| q.language == "python")
            .cloned()
            .collect();
        let n = py.len();
        assert!(n > 1, "fixture must have >1 python question");
        let set = questions::QuestionSet {
            blinding_seed: full.blinding_seed,
            token_proxy_chars_per_token: full.token_proxy_chars_per_token,
            questions: py,
        };
        // Judge is gated off (AP_EVAL_JUDGE_CMD unset in the harness default);
        // run_all is judge-status-agnostic for the retrieval outcomes asserted.
        let judge = Judge::from_env(set.blinding_seed);
        let outcomes = run_all(&set, &m.join("corpus"), &judge);

        assert_eq!(outcomes.len(), n, "one outcome per python question");
        for o in &outcomes {
            assert_eq!(o.q.language, "python", "run_all mixed in a foreign corpus");
        }
        let ids: Vec<&str> = outcomes.iter().map(|o| o.q.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "outcomes must be id-sorted");
    }

    #[test]
    fn budget_gate_banner_only_when_skipped() {
        // Gated-off ⇒ a banner naming the gate; ran ⇒ none. Kills the `==`/`!=`
        // status comparison and the `None`/`Some(dummy)` return mutants.
        let banner = budget_gate_banner(JudgeStatus::SkippedBudgetGated)
            .expect("gated-off status must produce a banner");
        assert!(
            banner.contains("BUDGET-GATED"),
            "banner must name the gate, got {banner:?}"
        );
        assert_eq!(
            budget_gate_banner(JudgeStatus::Ran),
            None,
            "a judge that ran emits no gating banner"
        );
    }

    #[test]
    fn summary_lines_report_aggregate_and_hypotheses() {
        // A minimal results doc with exactly the fields `summary_lines` reads.
        let results = serde_json::json!({
            "aggregate": {
                "graph": {
                    "precision": {"mean": 1.0, "stdev": 0.0},
                    "recall": {"mean": 0.83, "stdev": 0.34},
                    "tokens": {"mean": 36.7, "stdev": 19.8},
                    "tool_calls": {"mean": 1.0, "stdev": 0.0},
                },
                "explorer": {
                    "precision": {"mean": 0.65, "stdev": 0.33},
                    "recall": {"mean": 1.0, "stdev": 0.0},
                    "tokens": {"mean": 550.36, "stdev": 330.28},
                    "tool_calls": {"mean": 5.2, "stdev": 1.64},
                },
                "token_ratio_explorer_over_graph": {"mean": 17.4},
                "toolcall_ratio_explorer_over_graph": {"mean": 5.2},
            },
            "hypotheses": {"H1_tokens": {"result": "SUPPORTED"}},
            "answer_quality_judge": {"status": "SKIPPED_BUDGET_GATED"},
        });
        let lines = summary_lines(&results, JudgeStatus::SkippedBudgetGated);
        let blob = lines.join("\n");
        // Non-empty, and carries both the graph aggregate and the hypothesis
        // verdict — kills `summary_lines -> vec![]` and the dummy-vec mutants.
        assert!(
            blob.contains("aggregate"),
            "summary must have the aggregate header"
        );
        assert!(blob.contains("graph"), "summary must include the graph row");
        assert!(
            blob.contains("H1_tokens: \"SUPPORTED\""),
            "summary must report the hypothesis verdict, got {blob}"
        );
        assert!(
            blob.contains("SKIPPED_BUDGET_GATED"),
            "summary must report the judge status"
        );
    }

    #[test]
    fn write_json_writes_parseable_file() {
        let dir = tempfile::Builder::new()
            .prefix("eval_h2h_write_json")
            .tempdir()
            .expect("tempdir");
        let path = dir.path().join("out.json");
        let value = serde_json::json!({"k": 1, "nested": ["a", "b"]});
        write_json(&path, &value);
        let read: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("file written")).unwrap();
        // Kills `write_json -> ()`: the mutant writes nothing, so the read
        // (and the round-trip equality) fails.
        assert_eq!(read, value, "written json must round-trip");
    }
}
