// lsp_resolver::edges — turning one `textDocument/definition` answer into a
// graph edge.
//
// Split from `lsp_resolver` when that file crossed the §4.1 500-line cap.
// This half owns the mapping from an LSP URI back to the indexer's key space
// and the label-pair rules that decide which relationship table an answer
// belongs in.

use super::sites::{NodePosition, UnresolvedCallSite};
use crate::graph_store::{call_rel_table, call_site_rel_table, is_known_rel_table, GraphStore};
use crate::lsp_client;
use crate::resolver::cfg_verdict::{is_compiled_out, TwinView};
use std::collections::HashMap;
use std::path::Path;

/// Read-only lookups every call site's edge insertion needs, grouped so
/// `LspPass::record` stays within the §4.4 parameter cap.
pub(super) struct SiteContext<'a> {
    pub(super) node_index: &'a HashMap<(String, u64), NodePosition>,
    pub(super) canonical_root: &'a Path,
    /// `cfg_active` of every `#[cfg]` twin, read once per pass (issue #366).
    pub(super) twins: &'a TwinView,
}

/// What one definition answer did to the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LspEdge {
    Added,
    /// The definition is a `#[cfg]` twin the build compiles out for this
    /// caller: no row, and the site keeps the reason `cfg_twins` (issue #366).
    CompiledOutTwin,
    NotAdded,
}

/// `lsp_edge` reduced to whether a row was written (the tests' view).
#[cfg(test)]
pub(super) fn try_add_lsp_edge(
    store: &GraphStore,
    site: &UnresolvedCallSite,
    def: &lsp_client::DefinitionResult,
    ctx: &SiteContext<'_>,
) -> bool {
    lsp_edge(store, site, def, ctx) == LspEdge::Added
}

/// Records one definition answer and says why when it did not. The
/// language server answers from its own cfg set, which may not be the default
/// features, so a twin the default build compiles out is refused: one site
/// then never holds a language-server row and a `cfg-selected` row to two
/// different twins. An undecided twin is kept: the server's cfg decided it.
pub(super) fn lsp_edge(
    store: &GraphStore,
    site: &UnresolvedCallSite,
    def: &lsp_client::DefinitionResult,
    ctx: &SiteContext<'_>,
) -> LspEdge {
    // Convert LSP URI to a codebase-root-relative file path — the key space
    // of `node_index` (fleet-watch#18).
    let Some(file_path) = uri_to_relative_path(&def.uri, ctx.canonical_root) else {
        return LspEdge::NotAdded;
    };

    // Look up the definition in our node index.
    // LSP line is 0-based, our graph stores 1-based line numbers.
    let target_line = def.start_line + 1;

    // Exact line match only — no nearby-line scan. See `find_node_at_position`
    // for the fail-closed rationale.
    let Some(target) = find_node_at_position(ctx.node_index, &file_path, target_line) else {
        return LspEdge::NotAdded;
    };
    if !resolved_target_matches(target, site) {
        return LspEdge::NotAdded;
    }
    if is_compiled_out(ctx.twins, &site.id, &target.id) {
        return LspEdge::CompiledOutTwin;
    }
    let Some(rel_type) = call_rel_table(&site.caller_label, &target.label) else {
        return LspEdge::NotAdded;
    };
    if insert_lsp_edge(store, &rel_type, site, target) {
        LspEdge::Added
    } else {
        LspEdge::NotAdded
    }
}

/// Schema-guards `rel_type` and inserts the caller -> target edge at
/// LSP-backed confidence (0.9).
///
/// Schema guard: dynamically formatted rel tables can outrun the schema
/// when a new caller/target label combination appears. Drop rather than
/// abort.
///
/// Idempotent by construction (review finding 3): this pass writes one edge
/// per call site as it goes and flips `is_resolved` only at end of run, so
/// an interrupted run would otherwise be replayed into duplicate edges on
/// the next run — and two sites in one caller reaching the same callee
/// duplicate within a single run. An edge that is already there counts as
/// resolved, which is what it is.
pub(super) fn insert_lsp_edge(
    store: &GraphStore,
    rel_type: &str,
    site: &UnresolvedCallSite,
    target: &NodePosition,
) -> bool {
    if !is_known_rel_table(rel_type) {
        eprintln!(
            "lsp_resolver: dropped edge with unknown rel table '{rel_type}' \
             ({} -> {}); add it to REL_TABLES in graph_store.rs",
            site.caller_qn, target.id
        );
        return false;
    }
    let props = [
        ("confidence", "0.9"),
        ("resolution_method", "'lsp-definition'"),
    ];
    if store
        .insert_edge_if_absent(rel_type, &site.caller_qn, &target.id, &props)
        .is_err()
    {
        return false;
    }
    // The per-site twin (issue #335), same provenance, same replay-safety.
    match call_site_rel_table(&target.label) {
        Some(site_rel) => store
            .insert_edge_if_absent(site_rel, &site.id, &target.id, &props)
            .is_ok(),
        None => true,
    }
}

/// Maps a definition URI onto the codebase-root-relative path the indexer
/// keyed the graph with (`file.strip_prefix(root)` — indexer/mod.rs).
///
/// fleet-watch#18 root cause: this used to return the ABSOLUTE path verbatim
/// ("the caller handles prefix matching" — no caller ever did), so every
/// node-index lookup missed and the LSP pass never inserted a single edge.
/// Percent-escapes are decoded (the server echoes the encoding
/// `path_to_file_uri` produced) and BOTH sides are canonicalized so a
/// macOS symlink alias (/var vs /private/var) cannot defeat the strip.
/// A definition outside the codebase root (stdlib, cargo registry,
/// node_modules) yields None — those targets are not in the graph.
fn uri_to_relative_path(uri: &str, canonical_root: &Path) -> Option<String> {
    let abs = lsp_client::file_uri_to_path(uri)?;
    let abs = std::fs::canonicalize(&abs).unwrap_or(abs);
    abs.strip_prefix(canonical_root)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Exact (file, line) match only — no nearby-line scan. The node index
/// carries no column, so an inexact match cannot be told apart from a
/// coincidental same-line collision; guessing produced a fabricated
/// `total -> total` self-edge when a bare-argument CallSite (#87) resolved
/// to a PARAMETER declared on the same line as its enclosing method (PR
/// #267 follow-up). Failing closed costs recall on a genuinely multi-line
/// declaration; a wrong edge is worse than a missing one.
fn find_node_at_position<'a>(
    index: &'a HashMap<(String, u64), NodePosition>,
    file_path: &str,
    line: u64,
) -> Option<&'a NodePosition> {
    index.get(&(file_path.to_string(), line))
}

/// Defense in depth against a same-line collision `find_node_at_position`
/// cannot rule out on line-only data: even an EXACT line match can land on
/// the enclosing symbol's own declaration line (a parameter's
/// `targetSelectionRange` shares its function signature's line — see
/// `find_node_at_position`'s doc comment for the fabricated `total -> total`
/// self-edge this closes). The resolved node's own name (the last `::`
/// segment of its `id`, which the indexer sets equal to `qualified_name`)
/// must equal the identifier the call site actually asked about, or this is
/// that same collision wearing an exact-match line instead of a fuzzy one.
pub(super) fn resolved_target_matches(target: &NodePosition, site: &UnresolvedCallSite) -> bool {
    // A twin item (issue #353) carries `#cfg(..)` on its last segment
    // (`pick#cfg(not(feature=fast))`); the call spells only `pick`.
    let last =
        crate::graph_store::strip_cfg_gates(target.id.rsplit("::").next().unwrap_or(&target.id));
    last == site.identifier_name()
}

#[cfg(test)]
#[path = "edges_tests.rs"]
mod tests;
