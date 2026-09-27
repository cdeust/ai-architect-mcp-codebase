// doc_claims::symbols: existence and declared-visibility claims.
//
// A subject is either file-qualified (`src/lib.rs::TaskSet::new`, the graph's
// own qualified-name form) and must match exactly, or written the way a README
// writes it (`TaskSet::new`, `response_of`) and matches every symbol whose
// qualified name ends in `::<subject>`. `#[cfg]` twins (issue #353) match
// through their plain name. The lookup never falls back to a fuzzy match: a
// symbol of another file is not evidence about this one.

use serde_json::{json, Value};

use super::repo_files::RepoFiles;
use super::{graph_evidence, repo_evidence, Outcome, Verdict};
use crate::graph_store::{cypher_str, strip_cfg_gates, GraphStore};
use crate::prd_validator::unverifiable_reason;
use crate::search::{resolve_qualified_name, SEARCHABLE_LABELS};

/// Every label a symbol claim can name.
pub(super) const ANY_SYMBOL: &[&str] = SEARCHABLE_LABELS;

/// The labels whose nodes carry a declared `visibility` (graph_store::ddl).
const VISIBILITY_LABELS: [&str; 5] = ["Function", "Method", "Struct", "Enum", "Trait"];

/// A graph node the subject names.
#[derive(Debug, Clone)]
pub(super) struct Candidate {
    pub id: String,
    /// The qualified name with every `#cfg(..)` twin suffix removed.
    pub plain: String,
    pub label: &'static str,
    pub visibility: String,
    pub language: String,
    pub trait_name: String,
    pub start_line: u64,
}

/// The nodes a subject names, with the queries that found them.
pub(super) struct Lookup {
    pub candidates: Vec<Candidate>,
    pub evidence: Vec<Value>,
}

impl Lookup {
    /// The distinct plain qualified names: twins of one item count once.
    pub(super) fn distinct_items(&self) -> Vec<&str> {
        let mut plains: Vec<&str> = self.candidates.iter().map(|c| c.plain.as_str()).collect();
        plains.sort_unstable();
        plains.dedup();
        plains
    }
}

/// `src/lib.rs::x` names its file; `Type::x` and `x` do not.
pub(super) fn file_qualified(subject: &str) -> bool {
    subject
        .find("::")
        .is_some_and(|i| subject[..i].contains('.'))
}

/// A column of `label` as a RETURN expression: coalesced when the graph has
/// it, the neutral literal when a graph written before it lacks it.
fn column(store: &GraphStore, label: &str, name: &str, neutral: &str) -> String {
    if store.node_column_exists(label, name).unwrap_or(false) {
        format!("coalesce(n.{name}, {neutral})")
    } else {
        neutral.to_string()
    }
}

/// One label's rows named `leaf`, with the columns a candidate needs.
fn lookup_query(store: &GraphStore, label: &str, leaf: &str) -> String {
    format!(
        "MATCH (n:{label}) WHERE n.name = {} RETURN n.id, coalesce(n.qualified_name, n.id), \
         {}, {}, {}, {} ORDER BY n.id",
        cypher_str(leaf),
        column(store, label, "visibility", "''"),
        column(store, label, "language", "''"),
        column(store, label, "trait_name", "''"),
        column(store, label, "start_line", "0"),
    )
}

/// A lookup row `[id, qualified_name, visibility, language, trait, start_line]`
/// as a candidate, its qualified name stripped of `#cfg(..)` suffixes.
fn candidate(row: Vec<String>, label: &'static str) -> Option<Candidate> {
    let [id, qualified, visibility, language, trait_name, start_line]: [String; 6] =
        row.try_into().ok()?;
    Some(Candidate {
        plain: strip_cfg_gates(&qualified),
        id,
        label,
        visibility: visibility.trim().to_string(),
        language,
        trait_name,
        start_line: start_line.parse().unwrap_or(0),
    })
}

pub(super) fn lookup(
    store: &GraphStore,
    subject: &str,
    labels: &[&'static str],
) -> Result<Lookup, String> {
    let leaf = subject.rsplit("::").next().unwrap_or(subject);
    let exact = file_qualified(subject);
    let suffix = format!("::{subject}");
    let mut candidates = Vec::new();
    let mut evidence = Vec::new();
    for &label in labels {
        let query = lookup_query(store, label, leaf);
        let found: Vec<Candidate> = store
            .execute_query(&query)?
            .rows
            .into_iter()
            .filter_map(|row| candidate(row, label))
            .filter(|c| {
                c.plain == subject || c.id == subject || (!exact && c.plain.ends_with(&suffix))
            })
            .collect();
        let matched: Vec<String> = found.iter().map(|c| c.id.clone()).collect();
        candidates.extend(found);
        if !matched.is_empty() {
            let kept = "rows whose qualified name, #cfg(..) suffixes removed, equals the \
                        subject or ends with ::subject";
            evidence.push(graph_evidence(
                &query,
                json!({ "kept": kept, "matches": matched }),
            ));
        }
    }
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Lookup {
        candidates,
        evidence,
    })
}

/// The outcome for a subject no node matches: `not_verifiable` when its file
/// is outside the indexed graph, else `not_found`, never `contradicted`.
pub(super) fn absent(store: &GraphStore, subject: &str, mut evidence: Vec<Value>) -> Outcome {
    if file_qualified(subject) {
        if let Some(reason) = unverifiable_reason(store, subject) {
            return Outcome::not_verifiable(reason, evidence);
        }
    }
    let did_you_mean = match resolve_qualified_name(store, subject) {
        Ok(qn) => vec![qn],
        Err(miss) => miss.did_you_mean,
    };
    evidence.push(json!({ "source": "graph", "did_you_mean": did_you_mean }));
    Outcome::new(
        Verdict::NotFound,
        Some(
            "absent_from_graph: no node has this name; absence from the graph is not proof of \
             absence from the code (macro output and unsupported languages are not indexed)"
                .into(),
        ),
        evidence,
    )
}

pub(super) fn symbol_exists(store: &GraphStore, subject: &str, labels: &[&'static str]) -> Outcome {
    if subject.trim().is_empty() {
        return Outcome::not_verifiable("subject_empty", Vec::new());
    }
    match lookup(store, subject, labels) {
        Err(e) => Outcome::not_verifiable(format!("graph_query_failed: {e}"), Vec::new()),
        Ok(found) if found.candidates.is_empty() => absent(store, subject, found.evidence),
        Ok(found) => Outcome::new(Verdict::Supported, None, found.evidence),
    }
}

pub(super) fn file_exists(store: &GraphStore, files: &RepoFiles, subject: &str) -> Outcome {
    if subject.trim().is_empty() {
        return Outcome::not_verifiable("subject_empty", Vec::new());
    }
    let s = cypher_str(subject);
    let query =
        format!("MATCH (f:File) WHERE f.id = {s} OR f.path = {s} RETURN f.id ORDER BY f.id");
    let rows = match store.execute_query(&query) {
        Ok(qr) => qr.rows,
        Err(e) => return Outcome::not_verifiable(format!("graph_query_failed: {e}"), Vec::new()),
    };
    if !rows.is_empty() {
        let ids: Vec<&String> = rows.iter().map(|r| &r[0]).collect();
        return Outcome::new(
            Verdict::Supported,
            None,
            vec![graph_evidence(&query, json!({ "files": ids }))],
        );
    }
    let graph = graph_evidence(&query, json!({ "files": [] }));
    match files.exists(subject) {
        Ok(true) => Outcome::new(
            Verdict::Supported,
            None,
            vec![
                graph,
                repo_evidence(
                    subject,
                    json!(
                        "a regular file under repo_root; not in the graph (a language it does \
                           not parse, or excluded from indexing)"
                    ),
                ),
            ],
        ),
        Ok(false) => Outcome::new(
            Verdict::NotFound,
            Some("absent: neither the graph nor repo_root holds this file".into()),
            vec![graph],
        ),
        Err(e) => Outcome::not_verifiable(format!("subject_path_refused: {}", e.0), vec![graph]),
    }
}

/// What one candidate's declaration says about `pub`.
enum Declared {
    Pub,
    NotPub(String),
    Unknown(String),
}

fn declared(c: &Candidate) -> Declared {
    if !VISIBILITY_LABELS.contains(&c.label) {
        return Declared::Unknown(format!(
            "no_visibility_recorded: the graph records no visibility for a {}",
            c.label
        ));
    }
    if c.language != "rust" {
        return Declared::Unknown(format!(
            "visibility_not_checked_for_language: '{}'; this version reads Rust declarations only",
            c.language
        ));
    }
    match c.visibility.as_str() {
        "pub" => Declared::Pub,
        v if v.starts_with("pub(") => Declared::NotPub(format!("declared `{v}`")),
        "" if c.label == "Method" && !c.trait_name.is_empty() => Declared::Unknown(format!(
            "trait_method: a method of `impl {} for ..` has the trait's visibility",
            c.trait_name
        )),
        "" if c.label == "Method" => Declared::Unknown(
            "method_without_modifier: private in an inherent impl, public in a trait \
             declaration; the graph does not tell the two apart"
                .into(),
        ),
        "" => Declared::NotPub("declared without `pub`: private to its module".into()),
        v => Declared::Unknown(format!("unrecognised_visibility: '{v}'")),
    }
}

/// How many candidates are declared `pub`, why the others are not, and the
/// first reason one cannot be decided; each candidate's visibility query is
/// added to `evidence`.
fn tally(
    candidates: &[Candidate],
    evidence: &mut Vec<Value>,
) -> (usize, Vec<String>, Option<String>) {
    let mut public = 0;
    let mut not_public = Vec::new();
    let mut unknown = None;
    for c in candidates {
        evidence.push(graph_evidence(
            &format!(
                "MATCH (n:{}) WHERE n.id = {} RETURN n.visibility",
                c.label,
                cypher_str(&c.id)
            ),
            json!({ "id": c.id, "visibility": c.visibility, "language": c.language }),
        ));
        match declared(c) {
            Declared::Pub => public += 1,
            Declared::NotPub(why) => not_public.push(why),
            Declared::Unknown(why) => unknown = unknown.or(Some(why)),
        }
    }
    (public, not_public, unknown)
}

/// `is_public`: the item is declared with a bare `pub`. Whether the crate root
/// re-exports it is not checked, and the evidence says so.
pub(super) fn is_public(store: &GraphStore, subject: &str) -> Outcome {
    if subject.trim().is_empty() {
        return Outcome::not_verifiable("subject_empty", Vec::new());
    }
    let found = match lookup(store, subject, ANY_SYMBOL) {
        Ok(found) => found,
        Err(e) => return Outcome::not_verifiable(format!("graph_query_failed: {e}"), Vec::new()),
    };
    if found.candidates.is_empty() {
        return absent(store, subject, found.evidence);
    }
    let mut evidence = found.evidence.clone();
    let items = found.distinct_items();
    if items.len() > 1 {
        return Outcome::not_verifiable(
            format!(
                "ambiguous_subject: {} items match; name one by its qualified name",
                items.len()
            ),
            evidence,
        );
    }
    let (public, not_public, unknown) = tally(&found.candidates, &mut evidence);
    let total = found.candidates.len();
    if public == total {
        evidence.push(json!({ "source": "graph", "note":
            "declared `pub`; whether the crate root re-exports it is not checked" }));
        Outcome::new(Verdict::Supported, None, evidence)
    } else if not_public.len() == total {
        Outcome::new(
            Verdict::Contradicted,
            not_public.into_iter().next(),
            evidence,
        )
    } else {
        let reason = unknown.unwrap_or_else(|| {
            "twins_disagree: the #[cfg] twins of this item differ in visibility".into()
        });
        Outcome::not_verifiable(reason, evidence)
    }
}
