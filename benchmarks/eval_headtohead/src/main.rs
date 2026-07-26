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
use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let corpus_root = manifest.join("corpus");
    let set = questions::load(&manifest.join("questions.json")).expect("load questions.json");

    let hash = corpus::corpus_hash(&corpus_root);
    println!("== eval_headtohead_bench ==");
    println!("corpus_sha256: {hash}");
    println!("n_questions:   {}", set.questions.len());

    let judge = Judge::from_env(set.blinding_seed);
    if judge.status() == JudgeStatus::SkippedBudgetGated {
        eprintln!("\n################################################################");
        eprintln!("# ANSWER-QUALITY JUDGE LEG: SKIPPED (BUDGET-GATED)             #");
        eprintln!("# AP_EVAL_JUDGE_CMD is unset — no LLM judge was run.           #");
        eprintln!("# Deterministic legs (tokens, tool calls, precision/recall/F1  #");
        eprintln!("# vs ground truth) ran fully and produced the published        #");
        eprintln!("# numbers. See PRE_REGISTRATION.md §7 to enable the judge.     #");
        eprintln!("################################################################\n");
    }

    let outcomes = run_all(&set, &corpus_root, &judge);

    let results = report::build_results(&outcomes, judge.status());
    let raw = report::build_raw(&outcomes);
    write_json(&manifest.join("results.json"), &results);
    write_json(&manifest.join("raw_results.json"), &raw);

    print_summary(&results, judge.status());
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

/// Print the headline aggregate + hypothesis verdicts.
fn print_summary(results: &serde_json::Value, status: JudgeStatus) {
    let agg = &results["aggregate"];
    println!("\n-- aggregate (mean ± stdev over all questions) --");
    for cond in ["graph", "explorer"] {
        let c = &agg[cond];
        println!(
            "  {cond:8} precision {}±{}  recall {}±{}  tokens {}±{}  tool_calls {}±{}",
            c["precision"]["mean"],
            c["precision"]["stdev"],
            c["recall"]["mean"],
            c["recall"]["stdev"],
            c["tokens"]["mean"],
            c["tokens"]["stdev"],
            c["tool_calls"]["mean"],
            c["tool_calls"]["stdev"],
        );
    }
    println!(
        "  token ratio (explorer/graph):    {}",
        agg["token_ratio_explorer_over_graph"]["mean"]
    );
    println!(
        "  toolcall ratio (explorer/graph): {}",
        agg["toolcall_ratio_explorer_over_graph"]["mean"]
    );
    println!("\n-- hypotheses --");
    for (name, h) in results["hypotheses"].as_object().unwrap() {
        println!("  {name}: {}", h["result"]);
    }
    println!(
        "\n-- answer-quality judge: {} --",
        results["answer_quality_judge"]["status"]
    );
    let _ = status;
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
}
