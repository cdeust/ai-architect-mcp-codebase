//! Pre-registered question set model + loader (see `questions.json`).
//!
//! The question set is fixed BEFORE execution (§Move 1). This module only
//! deserializes it — it never derives questions from results.

use serde::Deserialize;
use std::path::Path;

/// Which AP graph tool answers a question in the GRAPH condition.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphOp {
    /// query_graph exact-name lookup — "where is X defined".
    Define,
    /// get_impact reverse-Calls — "who calls X".
    ImpactCallers,
    /// get_processes filtered to program entry points (kind == "main").
    ProcessesMain,
    /// get_impact users + importers — "what uses type X".
    ImpactUsers,
    /// search_codebase — "find symbols matching K".
    Search,
}

/// One pre-registered question with its ground-truth answer set.
#[derive(Debug, Clone, Deserialize)]
pub struct Question {
    pub id: String,
    pub language: String,
    pub dimension: String,
    pub question: String,
    pub graph_op: GraphOp,
    /// Symbol name (D1/D2/D4) or search keyword (D5); empty for D3.
    pub target: String,
    /// Substrings the EXPLORER baseline greps (case-insensitive).
    pub explorer_keywords: Vec<String>,
    /// The TRUE answer: files that constitute the correct answer, by
    /// construction of the committed corpus — independent of either condition.
    pub ground_truth_files: Vec<String>,
}

/// The whole pre-registered set.
#[derive(Debug, Clone, Deserialize)]
pub struct QuestionSet {
    pub blinding_seed: u64,
    pub token_proxy_chars_per_token: usize,
    pub questions: Vec<Question>,
}

/// Load and parse `questions.json`. Unknown JSON fields (`_comment`,
/// `dimensions`) are ignored by serde, so the human documentation in the file
/// does not have to be modelled here.
pub fn load(path: &Path) -> Result<QuestionSet, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))
}
