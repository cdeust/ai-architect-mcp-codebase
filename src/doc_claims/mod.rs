// doc_claims: check what a document says about a codebase against the code
// graph, one anchored claim at a time.
//
// The caller (a model reading a README, a changelog, an audit) supplies each
// claim with the exact text it read, the file and line it read it at, a kind
// and a subject. The checker never reads prose for meaning: it confirms the
// text is at that line, then applies the one rule its kind names, and answers
// with a verdict and the evidence behind it (a Cypher query `query_graph` can
// replay, or a `file:line` of the repository).
//
// Verdicts, and the rule no kind may break:
//   supported       the graph (or the repository) shows what the claim says;
//   contradicted    the graph shows the opposite, from declarations it holds
//                   completely (a declared visibility, a count whose compiled
//                   floor already exceeds the claim, an enum's variants);
//   not_found       the subject is not in the graph;
//   not_verifiable  the graph cannot decide, with the reason;
//   rejected_anchor the text is not at the line the claim names.
// An absence, and a count the graph only holds as a lower bound, NEVER produce
// `contradicted`: the graph misses what its parsers cannot see (macro output,
// unsupported languages), so "not there" is `not_found` and "fewer" is
// `not_verifiable`.
//
// Deterministic: no clock, rows sorted by (file, line, id), evidence sorted.

mod counts;
mod repo_files;
mod symbols;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::graph_store::GraphStore;
use repo_files::{anchor_holds, RepoFiles};

/// The kinds this version checks. Any other kind is answered
/// `not_verifiable` with reason `kind_not_supported_yet`.
pub const SUPPORTED_KINDS: [&str; 7] = [
    "symbol_exists",
    "module_exists",
    "file_exists",
    "is_public",
    "test_count",
    "proof_count",
    "enum_variant_count",
];

/// One claim as the caller read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Caller's id, or `c<index>` in input order.
    pub id: String,
    /// The exact text read; must appear verbatim at `file:line`.
    pub text: String,
    /// Document path relative to the repository root.
    pub file: String,
    /// 1-based line the text starts on.
    pub line: u64,
    pub kind: String,
    /// What the claim is about: a qualified name, a path, or a count scope.
    pub subject: String,
    /// The value claimed, for the kinds that compare one (`"26"`, `">=8"`).
    pub expected: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Supported,
    Contradicted,
    NotFound,
    NotVerifiable,
    RejectedAnchor,
}

impl Verdict {
    pub const ALL: [Verdict; 5] = [
        Verdict::Supported,
        Verdict::Contradicted,
        Verdict::NotFound,
        Verdict::NotVerifiable,
        Verdict::RejectedAnchor,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Supported => "supported",
            Verdict::Contradicted => "contradicted",
            Verdict::NotFound => "not_found",
            Verdict::NotVerifiable => "not_verifiable",
            Verdict::RejectedAnchor => "rejected_anchor",
        }
    }
}

/// A verdict, why, and what shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub verdict: Verdict,
    /// Always set for `not_verifiable` and `rejected_anchor`.
    pub reason: Option<String>,
    /// `{source: "graph", query, result}` or `{source: "repository", location, result}`.
    pub evidence: Vec<Value>,
}

impl Outcome {
    pub(crate) fn new(verdict: Verdict, reason: Option<String>, evidence: Vec<Value>) -> Self {
        Self {
            verdict,
            reason,
            evidence,
        }
    }

    pub(crate) fn not_verifiable(reason: impl Into<String>, evidence: Vec<Value>) -> Self {
        Self::new(Verdict::NotVerifiable, Some(reason.into()), evidence)
    }
}

/// A graph query that produced evidence, as `query_graph` can replay it.
pub(crate) fn graph_evidence(query: &str, result: Value) -> Value {
    json!({ "source": "graph", "query": query, "result": result })
}

/// A place in the repository that produced evidence.
pub(crate) fn repo_evidence(location: &str, result: Value) -> Value {
    json!({ "source": "repository", "location": location, "result": result })
}

/// One checked claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub claim: Claim,
    pub outcome: Outcome,
}

impl Row {
    pub fn to_json(&self) -> Value {
        let c = &self.claim;
        json!({
            "id": c.id,
            "file": c.file,
            "line": c.line,
            "kind": c.kind,
            "subject": c.subject,
            "expected": c.expected,
            "text": c.text,
            "verdict": self.outcome.verdict.as_str(),
            "reason": self.outcome.reason,
            "evidence": self.outcome.evidence,
        })
    }
}

/// Every claim's row, sorted by (file, line, id).
pub struct Report {
    pub rows: Vec<Row>,
}

impl Report {
    /// Rows per verdict; every verdict is present, zero included.
    pub fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts: BTreeMap<&'static str, usize> =
            Verdict::ALL.iter().map(|v| (v.as_str(), 0)).collect();
        for row in &self.rows {
            *counts.entry(row.outcome.verdict.as_str()).or_insert(0) += 1;
        }
        counts
    }
}

/// Checks every claim against `store`, reading anchors and source files under
/// `repo_root`. `repo_root` must be the tree `store` was indexed from.
pub fn check_claims(
    store: &GraphStore,
    repo_root: &Path,
    claims: Vec<Claim>,
) -> Result<Report, String> {
    let mut files = RepoFiles::new(repo_root)?;
    let mut rows: Vec<Row> = claims
        .into_iter()
        .map(|claim| {
            let outcome = check_one(store, &mut files, &claim);
            Row { claim, outcome }
        })
        .collect();
    rows.sort_by(|a, b| {
        (&a.claim.file, a.claim.line, &a.claim.id).cmp(&(&b.claim.file, b.claim.line, &b.claim.id))
    });
    Ok(Report { rows })
}

fn check_one(store: &GraphStore, files: &mut RepoFiles, claim: &Claim) -> Outcome {
    let location = format!("{}:{}", claim.file, claim.line);
    let anchored = files
        .read(&claim.file)
        .map_err(|e| e.0)
        .and_then(|content| anchor_holds(content, claim.line, &claim.text));
    if let Err(reason) = anchored {
        return Outcome::new(
            Verdict::RejectedAnchor,
            Some(reason),
            vec![repo_evidence(&location, json!("anchor not found"))],
        );
    }
    let anchor = repo_evidence(&location, json!("text found verbatim"));
    let mut outcome = match claim.kind.as_str() {
        "symbol_exists" => symbols::symbol_exists(store, &claim.subject, symbols::ANY_SYMBOL),
        "module_exists" => symbols::symbol_exists(store, &claim.subject, &["Module"]),
        "file_exists" => symbols::file_exists(store, files, &claim.subject),
        "is_public" => symbols::is_public(store, &claim.subject),
        "test_count" => counts::entry_count(store, files, claim, counts::TESTS),
        "proof_count" => counts::entry_count(store, files, claim, counts::PROOFS),
        "enum_variant_count" => counts::variant_count(store, files, claim),
        other => Outcome::not_verifiable(
            format!(
                "kind_not_supported_yet: '{other}'; this version checks {}",
                SUPPORTED_KINDS.join(", ")
            ),
            Vec::new(),
        ),
    };
    outcome.evidence.insert(0, anchor);
    outcome
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
