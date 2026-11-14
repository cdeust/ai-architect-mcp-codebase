//! `export_wiki_skeleton` — the MCP tool over `wiki::export` (issue #196,
//! slice 8).
//!
//! The tool reads two things and writes one: it opens the graph at
//! `graph_path` (the cached read handle every read tool shares), loads the
//! coverage record the index wrote beside it (`index_coverage.json` in the
//! graph's parent), runs `wiki::export`, and writes the canonical
//! `WikiSkeleton::to_json` bytes to `wiki_skeleton.json` in the same
//! directory, atomically. The response names the artifact, its schema
//! version, its size and SHA-256, the page count per kind, and what the
//! skeleton could not hold (`ExportReport`), each list bounded by the
//! response budget with its full count.
//!
//! The artifact goes to a file, not into the response: a skeleton of a large
//! graph is larger than a host's tool-result cap, and the byte-identical
//! artifact (criterion 6) is the file, which a caller hashes or diffs.
//!
//! No coverage record is an error, not an empty coverage block: an empty
//! block means "no known gap" (criterion 5), and without the record the gaps
//! are unknown, so a skeleton built without it would claim a completeness it
//! cannot back. A graph with no community is exported (its skeleton is
//! valid) with a warning, since an unclustered graph and a clustered graph
//! with nothing in it cannot be told apart from the store.
//!
//! The path opens no socket and starts no program: no network call, no model
//! client (criteria 1 and 2). `wiki_handlers_tests` asserts both on this
//! handler under a seccomp filter that kills the process on either.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::atomic_file::atomic_write;
use crate::graph_cache;
use crate::indexer::coverage;
use crate::response_budget;
use crate::verification_core::hex_lower;
use crate::wiki::{self, ExportReport, PageKind, WikiSkeleton};

/// The artifact's file name, beside the graph directory.
pub(crate) const WIKI_SKELETON_FILE: &str = "wiki_skeleton.json";

/// The lists of `ExportReport` (six), bounded together within one response
/// section: each gets an equal share of it.
const REPORT_LISTS: usize = 6;

pub(crate) fn run_export_wiki_skeleton(arguments: &Value) -> Value {
    match do_export_wiki_skeleton(arguments) {
        Ok(v) => v,
        Err(msg) => json!({
            "status": "error", "reason": "wiki_export_failed", "message": msg
        }),
    }
}

pub(crate) fn do_export_wiki_skeleton(arguments: &Value) -> Result<Value, String> {
    let args = arguments.as_object().ok_or("arguments must be an object")?;
    let graph_str = args
        .get("graph_path")
        .and_then(Value::as_str)
        .ok_or("missing required field 'graph_path'")?;
    let graph_path = Path::new(graph_str);
    if !graph_path.exists() {
        return Err(format!("graph_path does not exist: {graph_str}"));
    }
    let output_dir = output_dir_of(graph_path)?;
    let record = coverage_record(&output_dir)?;

    let store = graph_cache::open_cached(graph_path)?;
    let out = wiki::export(&store, &record)?;
    let json = out
        .skeleton
        .to_json()
        .map_err(|e| format!("wiki: serialise: {e}"))?;
    let artifact = output_dir.join(WIKI_SKELETON_FILE);
    atomic_write(&artifact, json.as_bytes())?;

    let counts = page_counts(&out.skeleton);
    let mut response = json!({
        "status": "ok",
        "tool": "export_wiki_skeleton",
        "schema_version": out.skeleton.schema_version(),
        "artifact_path": artifact.to_string_lossy(),
        "artifact_bytes": json.len(),
        "artifact_sha256": hex_lower(&Sha256::digest(json.as_bytes())),
        "page_count": out.skeleton.pages().len(),
        "pages_by_kind": counts,
        "not_in_skeleton": report_view(&out.report),
    });
    if counts["community"] == 0 {
        response["warnings"] = json!([no_community_warning()]);
    }
    Ok(response)
}

/// The coverage record the index wrote in `output_dir`; its absence is an
/// error, never an empty record.
fn coverage_record(output_dir: &Path) -> Result<coverage::CoverageReport, String> {
    let path = coverage::coverage_path(output_dir);
    coverage::load(&path).ok_or_else(|| {
        format!(
            "no usable coverage record at {}: without it the gaps of every page are \
             unknown, and an empty coverage block would read as none. Re-index the \
             graph (index_codebase or analyze_codebase) to write it",
            path.display()
        )
    })
}

fn no_community_warning() -> Value {
    json!({
        "code": "no_community",
        "message": "the graph holds no community, so the skeleton has no community \
                    and no god-node page: either cluster_graph has not run on this \
                    graph or it clustered nothing. Run cluster_graph (or \
                    analyze_codebase) and export again",
    })
}

/// The directory that holds the graph and its sidecars.
fn output_dir_of(graph_path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(graph_path)
        .map_err(|e| format!("graph_path {}: {e}", graph_path.display()))?;
    absolute
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("graph_path has no parent: {}", graph_path.display()))
}

fn page_counts(skeleton: &WikiSkeleton) -> Value {
    let count = |kind: PageKind| skeleton.pages().iter().filter(|p| p.kind == kind).count();
    json!({
        "community": count(PageKind::Community),
        "process": count(PageKind::Process),
        "god_node": count(PageKind::GodNode),
    })
}

/// `report` as JSON: every count as is, every list bounded with its total.
fn report_view(report: &ExportReport) -> Value {
    let budget = response_budget::per_section_chars() / REPORT_LISTS;
    let list = |items: &[String]| {
        let bounded =
            response_budget::bound_values(items.iter().map(|s| json!(s)).collect(), budget);
        json!({
            "total_count": bounded.total_count,
            "truncated": bounded.truncated,
            "items": bounded.items,
        })
    };
    let fence = report.god_node_fence.map(|f| {
        json!({
            "q1_quarters": f.q1_quarters(),
            "q3_quarters": f.q3_quarters(),
            "limit_quarters": f.limit_quarters(),
        })
    });
    json!({
        "community_unanchored_members": list(&report.community_unanchored_members),
        "community_unanchored_edges": report.community_unanchored_edges,
        "process_unanchored_members": list(&report.process_unanchored_members),
        "process_unanchored_edges": report.process_unanchored_edges,
        "god_node_unanchored_members": list(&report.god_node_unanchored_members),
        "god_node_unanchored_edges": report.god_node_unanchored_edges,
        "god_node_fence": fence,
        "parse_incomplete_unattributed": list(&report.parse_incomplete_unattributed),
        "missed_unattributed": list(&report.missed_unattributed),
        "missed_unanchored": list(&report.missed_unanchored),
    })
}

#[cfg(test)]
#[path = "wiki_handlers_tests.rs"]
mod tests;

#[cfg(all(
    test,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[path = "wiki_handlers_egress_tests.rs"]
mod egress_tests;
