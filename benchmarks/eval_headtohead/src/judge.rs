//! Blinded LLM-as-a-Judge leg (§5, §7) — CONFIG-GATED.
//!
//! The judge model is external and model-agnostic: the env var
//! `AP_EVAL_JUDGE_CMD` names a shell command that reads one JSON judging request
//! on stdin and writes `{"score_a": N, "score_b": M}` (each 0–4) on stdout. When
//! the var is unset the judge is SKIPPED and its absence is reported loudly —
//! never a silent stub (parent directive). The blinding, the prompt contract,
//! and this dispatch are committed and unit-tested regardless.
//!
//! Blinding (§5): for each question the two answers are presented as `answer_a`
//! / `answer_b` in a seed-deterministic order; the judge never learns which
//! condition produced which. The un-blinding is applied here, after the command
//! returns, and the map is recorded in `raw_results.json`.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

/// Whether the judge leg ran or was budget-gated off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JudgeStatus {
    Ran,
    SkippedBudgetGated,
}

/// One question's un-blinded judge scores.
#[derive(Debug, Clone, Copy)]
pub struct JudgeScore {
    pub graph: f64,
    pub explorer: f64,
    /// True when the GRAPH answer was presented to the judge as `answer_a`.
    pub graph_is_a: bool,
}

/// Config-gated judge dispatcher.
pub struct Judge {
    cmd: Option<String>,
    seed: u64,
}

impl Judge {
    /// Build from the environment. Absent `AP_EVAL_JUDGE_CMD` ⇒ gated off.
    pub fn from_env(seed: u64) -> Self {
        let cmd = std::env::var("AP_EVAL_JUDGE_CMD")
            .ok()
            .filter(|s| !s.trim().is_empty());
        Judge { cmd, seed }
    }

    pub fn status(&self) -> JudgeStatus {
        match self.cmd {
            Some(_) => JudgeStatus::Ran,
            None => JudgeStatus::SkippedBudgetGated,
        }
    }

    /// Grade one question. Returns `None` when gated off or on any command error
    /// (the deterministic legs still stand; the judge leg is best-effort).
    pub fn grade(
        &self,
        index: usize,
        question: &str,
        graph_text: &str,
        explorer_text: &str,
        source: &BTreeMap<String, String>,
    ) -> Option<JudgeScore> {
        let cmd = self.cmd.as_ref()?;
        let graph_is_a = self.graph_is_a(index);
        let (answer_a, answer_b) = if graph_is_a {
            (graph_text, explorer_text)
        } else {
            (explorer_text, graph_text)
        };
        let request = build_request(question, answer_a, answer_b, source);
        let (score_a, score_b) = run_command(cmd, &request)?;
        let (graph, explorer) = if graph_is_a {
            (score_a, score_b)
        } else {
            (score_b, score_a)
        };
        Some(JudgeScore {
            graph,
            explorer,
            graph_is_a,
        })
    }

    /// Deterministic A/B assignment (blinding) from seed + question index.
    /// A cheap splitmix64 step keeps the order reproducible without a rand dep.
    fn graph_is_a(&self, index: usize) -> bool {
        let mut z = self
            .seed
            .wrapping_add((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) & 1 == 0
    }
}

/// The judging request contract fed to the external model command.
fn build_request(
    question: &str,
    answer_a: &str,
    answer_b: &str,
    source: &BTreeMap<String, String>,
) -> Value {
    json!({
        "rubric": "Score each answer 0-4 against the source: 4 complete+correct; \
                   3 correct with <=1 spurious/missing; 2 partially correct; \
                   1 mostly wrong; 0 wrong/empty. Reply ONLY {\"score_a\":N,\"score_b\":M}.",
        "question": question,
        "answer_a": answer_a,
        "answer_b": answer_b,
        "source": source,
    })
}

/// Spawn the judge command, pipe the request to stdin, parse `score_a`/`score_b`.
fn run_command(cmd: &str, request: &Value) -> Option<(f64, f64)> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let payload = serde_json::to_vec(request).ok()?;
    child.stdin.take()?.write_all(&payload).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let parsed: Value = serde_json::from_slice(&out.stdout).ok()?;
    let a = parsed.get("score_a")?.as_f64()?;
    let b = parsed.get("score_b")?.as_f64()?;
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_env_is_budget_gated() {
        let j = Judge {
            cmd: None,
            seed: 64,
        };
        assert_eq!(j.status(), JudgeStatus::SkippedBudgetGated);
        let empty = BTreeMap::new();
        assert!(j.grade(0, "q", "g", "e", &empty).is_none());
    }

    #[test]
    fn from_env_enables_on_nonempty_and_gates_on_blank() {
        // Pins the empty-string filter: a real command enables the judge; a
        // whitespace-only value is treated as "not configured". (Edition 2021:
        // set_var/remove_var are safe. No other test reads this var.)
        std::env::set_var("AP_EVAL_JUDGE_CMD", "echo test");
        assert_eq!(Judge::from_env(64).status(), JudgeStatus::Ran);
        std::env::set_var("AP_EVAL_JUDGE_CMD", "   ");
        assert_eq!(
            Judge::from_env(64).status(),
            JudgeStatus::SkippedBudgetGated
        );
        std::env::remove_var("AP_EVAL_JUDGE_CMD");
        assert_eq!(
            Judge::from_env(64).status(),
            JudgeStatus::SkippedBudgetGated
        );
    }

    #[test]
    fn blinding_is_deterministic_and_index_sensitive() {
        let j = Judge {
            cmd: None,
            seed: 64,
        };
        // Same index ⇒ same assignment (reproducible blinding).
        assert_eq!(j.graph_is_a(3), j.graph_is_a(3));
        // Across indices the assignment varies (not a constant).
        let flags: Vec<bool> = (0..12).map(|i| j.graph_is_a(i)).collect();
        assert!(flags.iter().any(|&b| b) && flags.iter().any(|&b| !b));
    }

    #[test]
    fn blinding_matches_recorded_golden_vector() {
        // Pins the exact splitmix64 output for the published seed (64), so any
        // change to the hash arithmetic (^, >>, ==, the multipliers) is caught.
        // Golden vector computed independently, recorded here as the invariant.
        let j = Judge {
            cmd: None,
            seed: 64,
        };
        let got: Vec<bool> = (0..8).map(|i| j.graph_is_a(i)).collect();
        assert_eq!(
            got,
            vec![false, false, false, true, true, true, false, false]
        );
    }

    #[test]
    fn build_request_carries_question_answers_and_source() {
        let mut src = BTreeMap::new();
        src.insert("core.py".to_string(), "def f(): pass".to_string());
        let req = build_request("Q?", "AAA", "BBB", &src);
        assert_eq!(req["question"], "Q?");
        assert_eq!(req["answer_a"], "AAA");
        assert_eq!(req["answer_b"], "BBB");
        assert_eq!(req["source"]["core.py"], "def f(): pass");
        assert!(req["rubric"].as_str().unwrap().contains("score_a"));
    }

    #[test]
    fn different_seed_changes_assignment() {
        let a = Judge { cmd: None, seed: 1 };
        let b = Judge { cmd: None, seed: 2 };
        let differ = (0..16).any(|i| a.graph_is_a(i) != b.graph_is_a(i));
        assert!(differ);
    }

    #[test]
    fn grade_unblinds_scores_when_graph_is_a() {
        // Force graph_is_a(0)=true by picking a seed, then verify the mapping:
        // a real `sh` command echoes fixed scores; graph must get score_a.
        let seed = pick_seed_with_graph_is_a(0, true);
        let j = Judge {
            cmd: Some("cat >/dev/null; printf '{\"score_a\":4,\"score_b\":1}'".into()),
            seed,
        };
        let src = BTreeMap::new();
        let s = j.grade(0, "q", "GRAPH", "EXPLORER", &src).unwrap();
        assert!(s.graph_is_a);
        assert_eq!(s.graph, 4.0);
        assert_eq!(s.explorer, 1.0);
    }

    #[test]
    fn grade_unblinds_scores_when_graph_is_b() {
        let seed = pick_seed_with_graph_is_a(0, false);
        let j = Judge {
            cmd: Some("cat >/dev/null; printf '{\"score_a\":4,\"score_b\":1}'".into()),
            seed,
        };
        let src = BTreeMap::new();
        let s = j.grade(0, "q", "GRAPH", "EXPLORER", &src).unwrap();
        assert!(!s.graph_is_a);
        // graph presented as B ⇒ graph gets score_b (1), explorer score_a (4).
        assert_eq!(s.graph, 1.0);
        assert_eq!(s.explorer, 4.0);
    }

    #[test]
    fn failing_command_yields_none() {
        let j = Judge {
            cmd: Some("exit 1".into()),
            seed: 64,
        };
        let src = BTreeMap::new();
        assert!(j.grade(0, "q", "g", "e", &src).is_none());
    }

    /// Find a seed whose index-0 assignment matches `want` (blinding is a pure
    /// function of seed+index, so this is a deterministic search).
    fn pick_seed_with_graph_is_a(index: usize, want: bool) -> u64 {
        (0..1000)
            .find(|&s| Judge { cmd: None, seed: s }.graph_is_a(index) == want)
            .expect("a seed exists")
    }
}
