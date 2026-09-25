//! Cross-repo bridge — links separate per-repo property graphs at QUERY TIME.
//!
//! Topology: there is NO super-graph merge and NO re-index. Each repository
//! keeps its own graph file; the bridge consults a caller-supplied set of
//! *sibling* graph paths on demand to resolve references that dangle outside
//! the queried graph. A reference dangles in repo A precisely when repo A has
//! no local definition for it — the very condition `resolver.rs` already marks
//! external (`is_external_import` / no-target-found). Those become the bridge's
//! resolution candidates against the siblings.
//!
//! Two directions:
//!   - FORWARD  (`resolve_definition`): a symbol unresolved in the queried
//!     graph may be DEFINED in a sibling — used by `get_symbol`'s miss path.
//!   - REVERSE  (`foreign_callers`): a symbol defined in the queried graph may
//!     be CALLED from a sibling — used by `get_impact` so blast radius
//!     distinguishes local callers from foreign (cross-repo) callers.
//!
//! Every consultation is deterministic and cursor-stable: siblings are visited
//! in sorted path order and each result list carries a tie-free total order, so
//! a fixed (graph set, query) pair yields byte-identical output across calls.
//!
//! source: cross-repo bridge spec — engineer memory 4197914 (decided 2026-06-13).

use crate::graph_store::{self, GraphStore};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Symbol-definition labels a cross-repo reference can resolve to. Mirrors the
/// definition labels `resolver::build_symbol_index` indexes (minus Module/File,
/// which are not call/use targets). source: resolver.rs build_symbol_index.
const DEF_LABELS: &[&str] = &[
    "Function",
    "Method",
    "Struct",
    "Enum",
    "Trait",
    "Constant",
    "TypeAlias",
];

// ---------------------------------------------------------------------------
// Sibling registry
// ---------------------------------------------------------------------------

/// A deduplicated, sorted set of sibling graph paths to consult.
///
/// Parsed from the optional `sibling_graphs` MCP argument. Missing paths are
/// dropped into `skipped` rather than failing the whole query — a sibling
/// service may legitimately be absent in some deployments, and a cross-repo
/// query must degrade to the local answer instead of erroring out. The queried
/// graph itself is never included (it would double-count local edges as
/// foreign).
pub struct SiblingGraphs {
    paths: Vec<PathBuf>,
    /// Sibling paths that were requested but do not exist on disk.
    pub skipped: Vec<String>,
}

impl SiblingGraphs {
    /// Parses the optional `sibling_graphs` argument (an array of graph-dir
    /// path strings). When the argument is absent or empty the set is empty and
    /// the bridge is a no-op — every tool then behaves exactly as before
    /// (backward compatible).
    pub fn from_arg(arguments: &Value, self_graph: &Path) -> Self {
        let mut paths = Vec::new();
        let mut skipped = Vec::new();
        if let Some(arr) = arguments.get("sibling_graphs").and_then(|v| v.as_array()) {
            for v in arr {
                let Some(s) = v.as_str() else { continue };
                let p = PathBuf::from(s);
                if same_path(&p, self_graph) {
                    continue; // never bridge a repo to itself
                }
                if p.exists() {
                    paths.push(p);
                } else {
                    skipped.push(s.to_string());
                }
            }
        }
        // Deterministic consultation order + idempotent de-duplication.
        paths.sort();
        paths.dedup();
        skipped.sort();
        skipped.dedup();
        Self { paths, skipped }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// A definition found in a sibling repo (forward resolution result). Carries
/// the owning `repo` (the sibling graph path) so the caller can re-query that
/// graph directly via get_symbol/get_context with `graph_path = repo`.
pub struct ForeignSymbol {
    pub repo: String,
    pub label: String,
    pub qualified_name: String,
    pub id: String,
}

impl ForeignSymbol {
    pub fn to_json(&self) -> Value {
        json!({
            "repo": self.repo,
            "label": self.label,
            "qualified_name": self.qualified_name,
            "id": self.id,
        })
    }
}

/// A call site in a sibling repo that references the queried symbol by name
/// (reverse resolution result). `repo` is the sibling graph path; `caller_qn`
/// is a re-queryable handle into that graph.
pub struct ForeignCaller {
    pub repo: String,
    pub caller_qn: String,
    pub callee_name: String,
    pub callsite_id: String,
}

impl ForeignCaller {
    pub fn to_json(&self) -> Value {
        json!({
            "repo": self.repo,
            "caller_qn": self.caller_qn,
            "callee_name": self.callee_name,
            "callsite_id": self.callsite_id,
            // Cross-repo edges are matched by symbol name without a shared
            // linker, so they are heuristic by construction.
            "source": "cross-repo",
            "confidence": "0.50",
        })
    }
}

/// A search hit from a sibling repo (federated search result), tagged with its
/// owning `repo` so the caller knows which graph to traverse next.
pub struct ForeignHit {
    pub repo: String,
    pub qualified_name: String,
    pub name: String,
    pub label: String,
    pub file_path: String,
    pub score: f64,
}

impl ForeignHit {
    pub fn to_json(&self) -> Value {
        json!({
            "repo": self.repo,
            "qualified_name": self.qualified_name,
            "name": self.name,
            "kind": self.label,
            "file_path": self.file_path,
            "score": format!("{:.4}", self.score),
        })
    }
}

// ---------------------------------------------------------------------------
// Forward resolution — an unresolved local ref defined in a sibling
// ---------------------------------------------------------------------------

/// Opens one sibling graph. A refusal (`graph_handle_in_use`, `graph_cache_busy`,
/// issue #352) or any other failure skips that sibling, and says so on stderr
/// with the path and the error, the way the server logs elsewhere.
fn open_sibling(path: &Path) -> Option<GraphStore> {
    match GraphStore::open_or_create(path) {
        Ok(store) => Some(store),
        Err(error) => {
            eprintln!(
                "[ap] bridge: skipped sibling graph {}: {error}",
                path.display()
            );
            None
        }
    }
}

/// Finds definitions of `input` (a qualified name or bare symbol name) across
/// the sibling graphs. Prefers an exact/stripped qualified-name match (the same
/// forgiving lookup `get_symbol` uses); falls back to a bare-name match so a
/// caller that only knows the callee's short name still resolves.
///
/// Result order is total and tie-free: (repo, qualified_name, id).
pub fn resolve_definition(siblings: &SiblingGraphs, input: &str) -> Vec<ForeignSymbol> {
    let mut out = Vec::new();
    let short = last_segment(input);
    for path in siblings.paths() {
        let Some(store) = open_sibling(path) else {
            continue;
        };
        let repo = path.display().to_string();
        match crate::search::resolve_qualified_name(&store, input) {
            Ok(qn) => push_defs(&store, &repo, "qualified_name", &qn, &mut out),
            Err(_) => push_defs(&store, &repo, "name", short, &mut out),
        }
    }
    out.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.dedup_by(|a, b| a.repo == b.repo && a.id == b.id);
    out
}

/// Pushes every `DEF_LABELS` node in `store` whose `field` equals `value`.
fn push_defs(
    store: &GraphStore,
    repo: &str,
    field: &str,
    value: &str,
    out: &mut Vec<ForeignSymbol>,
) {
    let lit = graph_store::cypher_str(value);
    for label in DEF_LABELS {
        let cypher =
            format!("MATCH (n:{label}) WHERE n.{field} = {lit} RETURN n.id, n.qualified_name");
        if let Ok(qr) = store.execute_query(&cypher) {
            for row in &qr.rows {
                if row.len() >= 2 {
                    out.push(ForeignSymbol {
                        repo: repo.to_string(),
                        label: (*label).to_string(),
                        qualified_name: row[1].clone(),
                        id: row[0].clone(),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reverse resolution — sibling call sites that reference the queried symbol
// ---------------------------------------------------------------------------

/// Finds call sites in sibling graphs that reference `target_name` AND that do
/// not resolve locally in that sibling — i.e. genuine cross-repo callers of the
/// queried symbol.
///
/// Homonym safety: if a sibling DEFINES a symbol of the same short name, its
/// calls bind locally there and are NOT cross-repo edges to the queried symbol,
/// so the sibling is skipped entirely. This is what makes the bridge precise
/// rather than a fuzzy name-broadcast.
///
/// Result order is total and tie-free: (repo, caller_qn, callsite_id).
pub fn foreign_callers(siblings: &SiblingGraphs, target_name: &str) -> Vec<ForeignCaller> {
    let mut out = Vec::new();
    let short = last_segment(target_name);
    if short.is_empty() {
        return out;
    }
    for path in siblings.paths() {
        let Some(store) = open_sibling(path) else {
            continue;
        };
        if defines_name(&store, short) {
            continue;
        }
        let repo = path.display().to_string();
        collect_external_callsites(&store, &repo, short, &mut out);
    }
    out.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then_with(|| a.caller_qn.cmp(&b.caller_qn))
            .then_with(|| a.callsite_id.cmp(&b.callsite_id))
    });
    out.dedup_by(|a, b| a.repo == b.repo && a.callsite_id == b.callsite_id);
    out
}

/// True if `store` defines any symbol whose short name is `short`.
fn defines_name(store: &GraphStore, short: &str) -> bool {
    let lit = graph_store::cypher_str(short);
    for label in DEF_LABELS {
        let cypher = format!("MATCH (n:{label}) WHERE n.name = {lit} RETURN n.id LIMIT 1");
        if let Ok(qr) = store.execute_query(&cypher) {
            if qr.rows.iter().any(|r| !r.is_empty()) {
                return true;
            }
        }
    }
    false
}

/// Collects call sites in `store` whose callee short name equals `short`. The
/// full CallSite scan mirrors `resolver::resolve_calls` (CallSite carries no
/// name index); the set is bounded by the sibling's call-site count.
fn collect_external_callsites(
    store: &GraphStore,
    repo: &str,
    short: &str,
    out: &mut Vec<ForeignCaller>,
) {
    let qr = match store.execute_query("MATCH (cs:CallSite) RETURN cs.id, cs.callee_name") {
        Ok(q) => q,
        Err(_) => return,
    };
    for row in &qr.rows {
        if row.len() < 2 {
            continue;
        }
        if last_segment(&row[1]) != short {
            continue;
        }
        out.push(ForeignCaller {
            repo: repo.to_string(),
            caller_qn: crate::resolver::extract_caller_from_callsite_id(&row[0]),
            callee_name: row[1].clone(),
            callsite_id: row[0].clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// resolve_graph annotation — how many dangling refs a sibling can resolve
// ---------------------------------------------------------------------------

/// Counts how many of the given unresolved reference targets a sibling can
/// define, returning `(resolvable_count, sample)` where `sample` lists up to
/// `SAMPLE_CAP` concrete resolutions for the caller to spot-check. Targets are
/// de-duplicated and visited in sorted order so the count and sample are
/// deterministic.
pub fn count_cross_repo_resolvable(
    siblings: &SiblingGraphs,
    targets: &[String],
) -> (usize, Vec<Value>) {
    const SAMPLE_CAP: usize = 10;
    let mut unique: Vec<&String> = targets.iter().collect();
    unique.sort();
    unique.dedup();

    let mut resolvable = 0usize;
    let mut sample = Vec::new();
    for target in unique {
        let defs = resolve_definition(siblings, target);
        if let Some(first) = defs.first() {
            resolvable += 1;
            if sample.len() < SAMPLE_CAP {
                sample.push(json!({
                    "target_text": target,
                    "repo": first.repo,
                    "qualified_name": first.qualified_name,
                    "label": first.label,
                }));
            }
        }
    }
    (resolvable, sample)
}

// ---------------------------------------------------------------------------
// Federated search — run the query across siblings, tag hits with their repo
// ---------------------------------------------------------------------------

/// Runs `query` against every sibling graph and returns the merged, repo-tagged
/// hits. Each sibling is searched with the same options as the local search;
/// per-sibling result count is bounded by `limit`. The merged list carries a
/// total order — (repo, score desc, qualified_name) — so it is cursor-stable.
pub fn federated_search(siblings: &SiblingGraphs, query: &str, limit: usize) -> Vec<ForeignHit> {
    let mut out = Vec::new();
    let options = crate::search::SearchOptions {
        limit,
        label_filter: None,
        min_score: 0.01,
    };
    for path in siblings.paths() {
        let Some(store) = open_sibling(path) else {
            continue;
        };
        let index_dir = path
            .parent()
            .map(|p| p.join("search_index"))
            .filter(|p| p.exists());
        let Ok(results) =
            crate::search::search_graph(&store, query, &options, index_dir.as_deref())
        else {
            continue;
        };
        let repo = path.display().to_string();
        for r in results {
            out.push(ForeignHit {
                repo: repo.clone(),
                qualified_name: r.qualified_name,
                name: r.name,
                label: r.label,
                file_path: r.file_path,
                score: r.score,
            });
        }
    }
    out.sort_by(|a, b| {
        a.repo
            .cmp(&b.repo)
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.qualified_name.cmp(&b.qualified_name))
    });
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The trailing identifier of a possibly-qualified name: the segment after the
/// last `::`, then after the last `.`. So `mod.rs::Type::method` → `method`,
/// `self.bar` → `bar`, `foo` → `foo`. Mirrors how `resolver::resolve_calls`
/// treats callee names (`callee.contains("::")`).
pub fn last_segment(s: &str) -> &str {
    let after_colon = s.rsplit("::").next().unwrap_or(s);
    after_colon.rsplit('.').next().unwrap_or(after_colon)
}

/// True when two paths denote the same graph directory. Canonicalizes when
/// possible (resolves `.`/`..`/symlinks); falls back to a literal compare when
/// either path cannot be canonicalized (e.g. does not yet exist).
fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_segment_strips_qualifiers() {
        assert_eq!(last_segment("mod.rs::Type::method"), "method");
        assert_eq!(last_segment("main.rs::foo"), "foo");
        assert_eq!(last_segment("self.bar"), "bar");
        assert_eq!(last_segment("foo"), "foo");
        assert_eq!(last_segment("a::b::c.d"), "d");
    }

    #[test]
    fn empty_arg_is_noop() {
        let args = json!({});
        let s = SiblingGraphs::from_arg(&args, Path::new("/tmp/self_graph"));
        assert!(s.is_empty());
        assert!(s.skipped.is_empty());
    }

    #[test]
    fn missing_paths_are_skipped_not_fatal() {
        let args =
            json!({ "sibling_graphs": ["/nonexistent/graph-aaa", "/nonexistent/graph-bbb"] });
        let s = SiblingGraphs::from_arg(&args, Path::new("/tmp/self_graph"));
        assert!(s.is_empty());
        assert_eq!(s.skipped.len(), 2);
        // skipped is sorted + deduped
        assert_eq!(s.skipped[0], "/nonexistent/graph-aaa");
    }

    #[test]
    fn self_path_is_excluded() {
        let args = json!({ "sibling_graphs": ["/tmp/self_graph"] });
        let s = SiblingGraphs::from_arg(&args, Path::new("/tmp/self_graph"));
        assert!(s.is_empty());
        assert!(s.skipped.is_empty());
    }

    #[test]
    fn non_array_arg_is_noop() {
        let args = json!({ "sibling_graphs": "not-an-array" });
        let s = SiblingGraphs::from_arg(&args, Path::new("/tmp/self_graph"));
        assert!(s.is_empty());
    }
}

#[cfg(test)]
#[path = "bridge_sibling_tests.rs"]
mod sibling_tests;
