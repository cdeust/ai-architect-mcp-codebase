//! Result assembly (§Move 5, §10): per-question rows (including the ones GRAPH
//! loses — AC6) and aggregate mean ± stdev with the four hypotheses evaluated
//! against their pre-registered thresholds.

use crate::judge::{JudgeScore, JudgeStatus};
use crate::metrics::{self, round2, Retrieval};
use crate::questions::Question;
use serde_json::{json, Value};

/// One condition's measured result for a question.
#[derive(Debug, Clone)]
pub struct CondResult {
    pub files: Vec<String>,
    pub tokens: f64,
    pub tool_calls: usize,
    pub retrieval: Retrieval,
}

/// A question's full outcome across both conditions plus the optional judge.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub q: Question,
    pub graph: CondResult,
    pub explorer: CondResult,
    pub judge: Option<JudgeScore>,
}

/// Mean ± sample-stdev of a scalar extracted from every outcome.
fn agg(outcomes: &[Outcome], f: impl Fn(&Outcome) -> f64) -> (f64, f64) {
    let xs: Vec<f64> = outcomes.iter().map(&f).collect();
    (round2(metrics::mean(&xs)), round2(metrics::stdev(&xs)))
}

/// `{mean, stdev}` JSON for a scalar over the outcomes.
fn stat(outcomes: &[Outcome], f: impl Fn(&Outcome) -> f64) -> Value {
    let (m, s) = agg(outcomes, f);
    json!({ "mean": m, "stdev": s })
}

/// Select the graph or explorer result of an outcome.
fn pick(o: &Outcome, graph: bool) -> &CondResult {
    if graph {
        &o.graph
    } else {
        &o.explorer
    }
}

/// Aggregate block for one condition (precision/recall/f1/tokens/tool_calls).
fn condition_block(outcomes: &[Outcome], graph: bool) -> Value {
    json!({
        "precision": stat(outcomes, |o| pick(o, graph).retrieval.precision),
        "recall": stat(outcomes, |o| pick(o, graph).retrieval.recall),
        "f1": stat(outcomes, |o| pick(o, graph).retrieval.f1),
        "tokens": stat(outcomes, |o| pick(o, graph).tokens),
        "tool_calls": stat(outcomes, |o| pick(o, graph).tool_calls as f64),
    })
}

/// Per-question token/tool-call ratios (explorer / graph), guarded against zero.
fn token_ratio(o: &Outcome) -> f64 {
    o.explorer.tokens / o.graph.tokens.max(1.0)
}
fn toolcall_ratio(o: &Outcome) -> f64 {
    o.explorer.tool_calls as f64 / (o.graph.tool_calls.max(1) as f64)
}

/// Mean precision on a single dimension for one condition.
fn dim_precision(outcomes: &[Outcome], dim: &str, graph: bool) -> f64 {
    let xs: Vec<f64> = outcomes
        .iter()
        .filter(|o| o.q.dimension == dim)
        .map(|o| {
            if graph {
                o.graph.retrieval.precision
            } else {
                o.explorer.retrieval.precision
            }
        })
        .collect();
    round2(metrics::mean(&xs))
}

/// Evaluate the four pre-registered hypotheses against their thresholds.
fn hypotheses(outcomes: &[Outcome]) -> Value {
    let (tok_ratio, _) = agg(outcomes, token_ratio);
    let (call_ratio, _) = agg(outcomes, toolcall_ratio);
    let g_prec_d2 = dim_precision(outcomes, "D2", true);
    let e_prec_d2 = dim_precision(outcomes, "D2", false);
    let (g_recall, _) = agg(outcomes, |o| o.graph.retrieval.recall);
    let (e_recall, _) = agg(outcomes, |o| o.explorer.retrieval.recall);
    json!({
        "H1_tokens": verdict(
            "GRAPH uses fewer tokens; mean explorer/graph ratio > 1.5x",
            tok_ratio > 1.5,
            json!({ "mean_token_ratio_explorer_over_graph": tok_ratio, "threshold": 1.5 }),
        ),
        "H2_tool_calls": verdict(
            "GRAPH issues fewer tool calls; mean ratio > 2x",
            call_ratio > 2.0,
            json!({ "mean_toolcall_ratio_explorer_over_graph": call_ratio, "threshold": 2.0 }),
        ),
        "H3_precision_D2": verdict(
            "GRAPH precision >= EXPLORER on D2 by >= 0.15",
            g_prec_d2 >= e_prec_d2 + 0.15,
            json!({ "graph_D2_precision": g_prec_d2, "explorer_D2_precision": e_prec_d2, "margin": 0.15 }),
        ),
        "H4_recall_no_regression": verdict(
            "GRAPH recall not worse than EXPLORER by more than 0.10",
            g_recall >= e_recall - 0.10,
            json!({ "graph_recall": g_recall, "explorer_recall": e_recall, "max_regression": 0.10 }),
        ),
    })
}

/// A hypothesis verdict object.
fn verdict(claim: &str, supported: bool, evidence: Value) -> Value {
    json!({
        "claim": claim,
        "result": if supported { "SUPPORTED" } else { "FALSIFIED" },
        "evidence": evidence,
    })
}

/// Per-dimension aggregate (precision/recall/tokens for both conditions).
fn per_dimension(outcomes: &[Outcome]) -> Value {
    let mut dims: Vec<String> = outcomes.iter().map(|o| o.q.dimension.clone()).collect();
    dims.sort();
    dims.dedup();
    let mut map = serde_json::Map::new();
    for d in dims {
        let sub: Vec<Outcome> = outcomes
            .iter()
            .filter(|o| o.q.dimension == d)
            .cloned()
            .collect();
        map.insert(
            d,
            json!({
                "graph": condition_block(&sub, true),
                "explorer": condition_block(&sub, false),
            }),
        );
    }
    Value::Object(map)
}

/// Per-language aggregate.
fn per_language(outcomes: &[Outcome]) -> Value {
    let mut langs: Vec<String> = outcomes.iter().map(|o| o.q.language.clone()).collect();
    langs.sort();
    langs.dedup();
    let mut map = serde_json::Map::new();
    for l in langs {
        let sub: Vec<Outcome> = outcomes
            .iter()
            .filter(|o| o.q.language == l)
            .cloned()
            .collect();
        map.insert(
            l,
            json!({
                "graph": condition_block(&sub, true),
                "explorer": condition_block(&sub, false),
            }),
        );
    }
    Value::Object(map)
}

/// Judge aggregate block (present only when the judge leg ran).
fn judge_block(outcomes: &[Outcome], status: JudgeStatus) -> Value {
    if status == JudgeStatus::SkippedBudgetGated {
        return json!({
            "status": "SKIPPED_BUDGET_GATED",
            "note": "answer-quality judge not run (no AP_EVAL_JUDGE_CMD). Deterministic \
                     retrieval precision/recall/F1 vs ground truth stand as the offline \
                     answer-quality evidence. See PRE_REGISTRATION.md §7."
        });
    }
    let graded: Vec<&Outcome> = outcomes.iter().filter(|o| o.judge.is_some()).collect();
    let g: Vec<f64> = graded.iter().map(|o| o.judge.unwrap().graph).collect();
    let e: Vec<f64> = graded.iter().map(|o| o.judge.unwrap().explorer).collect();
    json!({
        "status": "RAN",
        "graded_questions": graded.len(),
        "graph_score": { "mean": round2(metrics::mean(&g)), "stdev": round2(metrics::stdev(&g)) },
        "explorer_score": { "mean": round2(metrics::mean(&e)), "stdev": round2(metrics::stdev(&e)) },
    })
}

/// Build `results.json`.
pub fn build_results(outcomes: &[Outcome], status: JudgeStatus) -> Value {
    json!({
        "experiment": "issue #64 head-to-head — AP graph tools vs Grep/Glob/Read baseline",
        "pre_registration": "PRE_REGISTRATION.md",
        "n_questions": outcomes.len(),
        "aggregate": {
            "graph": condition_block(outcomes, true),
            "explorer": condition_block(outcomes, false),
            "token_ratio_explorer_over_graph": stat(outcomes, token_ratio),
            "toolcall_ratio_explorer_over_graph": stat(outcomes, toolcall_ratio),
        },
        "hypotheses": hypotheses(outcomes),
        "per_dimension": per_dimension(outcomes),
        "per_language": per_language(outcomes),
        "answer_quality_judge": judge_block(outcomes, status),
    })
}

/// One raw per-question row (AC6 — including losses and the blinding map).
fn raw_row(o: &Outcome) -> Value {
    json!({
        "id": o.q.id,
        "language": o.q.language,
        "dimension": o.q.dimension,
        "question": o.q.question,
        "ground_truth_files": o.q.ground_truth_files,
        "graph": raw_cond(&o.graph),
        "explorer": raw_cond(&o.explorer),
        "graph_precision_minus_explorer": round2(
            o.graph.retrieval.precision - o.explorer.retrieval.precision,
        ),
        "graph_recall_minus_explorer": round2(
            o.graph.retrieval.recall - o.explorer.retrieval.recall,
        ),
        "judge": o.judge.map(|j| json!({
            "graph_score": j.graph,
            "explorer_score": j.explorer,
            "graph_presented_as": if j.graph_is_a { "answer_a" } else { "answer_b" },
        })),
    })
}

fn raw_cond(c: &CondResult) -> Value {
    json!({
        "retrieved_files": c.files,
        "precision": round2(c.retrieval.precision),
        "recall": round2(c.retrieval.recall),
        "f1": round2(c.retrieval.f1),
        "tokens_est": round2(c.tokens),
        "tool_calls": c.tool_calls,
    })
}

/// Build `raw_results.json`.
pub fn build_raw(outcomes: &[Outcome]) -> Value {
    let rows: Vec<Value> = outcomes.iter().map(raw_row).collect();
    json!({
        "note": "Raw per-question data, including every question GRAPH does not win. \
                 graph_*_minus_explorer < 0 marks a GRAPH loss on that metric.",
        "rows": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::judge::JudgeStatus;
    use crate::questions::GraphOp;

    fn ret(p: f64, r: f64) -> Retrieval {
        Retrieval {
            precision: p,
            recall: r,
            f1: if p + r == 0.0 {
                0.0
            } else {
                2.0 * p * r / (p + r)
            },
        }
    }

    fn outcome(dim: &str, g: (f64, f64, f64, usize), e: (f64, f64, f64, usize)) -> Outcome {
        Outcome {
            q: Question {
                id: format!("{dim}-x"),
                language: "python".into(),
                dimension: dim.into(),
                question: "q".into(),
                graph_op: GraphOp::ImpactCallers,
                target: "t".into(),
                explorer_keywords: vec![],
                ground_truth_files: vec!["core.py".into()],
            },
            graph: CondResult {
                files: vec!["core.py".into()],
                tokens: g.2,
                tool_calls: g.3,
                retrieval: ret(g.0, g.1),
            },
            explorer: CondResult {
                files: vec!["core.py".into(), "x.py".into()],
                tokens: e.2,
                tool_calls: e.3,
                retrieval: ret(e.0, e.1),
            },
            judge: None,
        }
    }

    #[test]
    fn hypotheses_all_supported_when_graph_dominates() {
        // graph: prec 1, rec 1, 40 tokens, 1 call; explorer: prec .4, rec 1,
        // 800 tokens, 7 calls; dimension D2.
        let outs = vec![outcome("D2", (1.0, 1.0, 40.0, 1), (0.4, 1.0, 800.0, 7))];
        let h = hypotheses(&outs);
        assert_eq!(h["H1_tokens"]["result"], "SUPPORTED");
        assert_eq!(h["H2_tool_calls"]["result"], "SUPPORTED");
        assert_eq!(h["H3_precision_D2"]["result"], "SUPPORTED");
        assert_eq!(h["H4_recall_no_regression"]["result"], "SUPPORTED");
    }

    #[test]
    fn h4_falsified_when_graph_recall_drops() {
        // graph recall 0.5 vs explorer 1.0 ⇒ 0.5 regression > 0.10.
        let outs = vec![outcome("D2", (1.0, 0.5, 40.0, 1), (0.4, 1.0, 800.0, 7))];
        let h = hypotheses(&outs);
        assert_eq!(h["H4_recall_no_regression"]["result"], "FALSIFIED");
    }

    #[test]
    fn h1_falsified_when_tokens_close() {
        // token ratio 1.2 < 1.5 threshold.
        let outs = vec![outcome("D2", (1.0, 1.0, 100.0, 1), (0.4, 1.0, 120.0, 7))];
        let h = hypotheses(&outs);
        assert_eq!(h["H1_tokens"]["result"], "FALSIFIED");
    }

    #[test]
    fn h3_falsified_without_precision_margin() {
        // graph D2 precision equals explorer ⇒ no 0.15 margin.
        let outs = vec![outcome("D2", (0.5, 1.0, 40.0, 1), (0.5, 1.0, 800.0, 7))];
        let h = hypotheses(&outs);
        assert_eq!(h["H3_precision_D2"]["result"], "FALSIFIED");
    }

    #[test]
    fn results_carry_dispersion_and_counts() {
        // D1 uses graph tool_calls=2 so the ratio arithmetic (÷ vs ×) is
        // exercised (a graph value of 1 makes ÷ and × indistinguishable).
        let outs = vec![
            outcome("D2", (1.0, 1.0, 40.0, 1), (0.4, 1.0, 800.0, 7)),
            outcome("D1", (1.0, 1.0, 20.0, 2), (0.2, 1.0, 900.0, 6)),
        ];
        let res = build_results(&outs, JudgeStatus::SkippedBudgetGated);
        assert_eq!(res["n_questions"], 2);
        // stdev present and non-zero for the differing explorer token counts.
        assert!(
            res["aggregate"]["explorer"]["tokens"]["stdev"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        assert_eq!(
            res["answer_quality_judge"]["status"],
            "SKIPPED_BUDGET_GATED"
        );
        // Tool-call ratio: mean of (7/1, 6/2) = mean(7, 3) = 5.0. The '×' mutant
        // would give mean(7, 12) = 9.5.
        assert_eq!(
            res["aggregate"]["toolcall_ratio_explorer_over_graph"]["mean"],
            5.0
        );
        // Per-dimension breakdown must isolate the right rows (kills the ==
        // filter and the Default::default() mutants).
        assert_eq!(res["per_dimension"]["D2"]["graph"]["tokens"]["mean"], 40.0);
        assert_eq!(res["per_dimension"]["D1"]["graph"]["tokens"]["mean"], 20.0);
        // Per-language breakdown present with the expected aggregate.
        assert_eq!(
            res["per_language"]["python"]["explorer"]["tool_calls"]["mean"],
            6.5
        );
    }

    #[test]
    fn raw_row_marks_graph_losses_and_wins() {
        let outs = vec![outcome("D2", (1.0, 0.5, 40.0, 1), (0.4, 1.0, 800.0, 7))];
        let raw = build_raw(&outs);
        let row = &raw["rows"][0];
        assert_eq!(row["graph_precision_minus_explorer"], 0.6); // graph wins precision
        assert_eq!(row["graph_recall_minus_explorer"], -0.5); // graph loses recall
                                                              // Nested condition blocks are real (kills raw_cond -> Default::default()).
        assert_eq!(row["graph"]["precision"], 1.0);
        assert_eq!(row["graph"]["recall"], 0.5);
        assert_eq!(row["graph"]["tool_calls"], 1);
        assert_eq!(row["explorer"]["precision"], 0.4);
        assert_eq!(row["explorer"]["tokens_est"], 800.0);
    }
}
