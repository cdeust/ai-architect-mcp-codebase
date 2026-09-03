// indexer — Walk + Parse + Persist pipeline for multi-language codebases.
//
// Wires graph_store (step 1) and parser module (step 2) into an indexer
// that processes a full directory of source files. Supports Rust, Python,
// and TypeScript. Zero dependency on main.rs.

use crate::graph_store::GraphStore;
use crate::parser::Language;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

mod batch;
pub mod coverage;
mod iac;
mod incremental;
mod light_link;
pub mod manifest;
mod persist;
mod walk;

// Re-exported so sibling submodules (persist, incremental) keep referring to
// `super::SymbolBatch` after the Extract Class move into `batch`.
use batch::{SymbolBatch, SYMBOL_FLUSH_THRESHOLD};

use coverage::{CoverageCollector, CoverageReport};
pub use incremental::{
    fill_after_bootstrap, index_incremental, write_full_manifest, FillMethod, FillResult,
};
use persist::{
    index_single_file, insert_ancestor_dirs, insert_dir_file_edge, insert_file_node, ParseOutcome,
};
use walk::{collect_source_files, is_dependency_path, WalkOptions};
pub use walk::{DependencyScope, ExcludeSet};

// ---------------------------------------------------------------------------
// Resource limits — source: security hardening (H1).
// Bound the indexer's work to prevent DoS via oversized codebases.
// ---------------------------------------------------------------------------

// source: heuristic — 100k files covers the largest real-world monorepos
// (linux kernel ~80k .c files; chromium src/ ~70k). Larger inputs are
// almost certainly adversarial or accidental (e.g. indexing `/`).
const MAX_FILES: usize = 100_000;

// source: heuristic — 10 MB is ~10× the largest realistic hand-written source
// file (sqlite3.c is ~7 MB; this is the practical upper bound). Files above
// this are almost always generated/minified and bring no graph value.
const MAX_FILE_BYTES: u64 = 10_485_760;

// source: heuristic — 2 GB total reads caps peak RSS during indexing.
// macOS default ulimit -m is effectively unbounded, so we self-limit.
const MAX_TOTAL_BYTES: u64 = 2_147_483_648;

// source: heuristic — 64 is deeper than any realistic project tree
// (node_modules pathologies rarely exceed 30). Prevents stack-exhaustion
// via symlinked/pathological directory structures.
const MAX_DEPTH: usize = 64;

// Per-file byte cap before handing to tree-sitter. The canonical definition now
// lives at the parse boundary (`parser::MAX_PARSE_BYTES`, issue #148); it is
// re-exported so the indexer's read-path checks keep sharing one source of truth
// with the parser's own boundary check. Those checks remain as defense-in-depth:
// they map an oversized file to a `Skipped` coverage outcome (distinct from
// `parse_failed`) before the read.
pub use crate::parser::MAX_PARSE_BYTES;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Grouped indexing knobs threaded through every walk-configuring entry point
/// (full index, incremental index, artifact fill, manifest rebuild).
///
/// Bundled into one value so none of those functions needs a 5th/6th/7th
/// individual parameter for `exclude_dirs` (coding-standards §4.4's 4-param
/// cap) — `language_filter` and `dependency_scope` were already threaded as
/// two separate parameters everywhere a walk decision is made; adding a third
/// loose parameter for `exclude_dirs` would push several call sites further
/// over the cap they were already at. Replacing the pair with this struct
/// nets every touched signature a parameter, not costs it one.
#[derive(Debug, Clone, Default)]
pub struct IndexOptions {
    pub language_filter: Option<Language>,
    pub dependency_scope: DependencyScope,
    /// Issue #249 — user-specified directories to prune from the walk, in
    /// addition to the built-in build/dependency skip list. Wins over every
    /// `dependency_scope` tier.
    pub exclude_dirs: ExcludeSet,
}

pub struct IndexResult {
    pub graph_path: PathBuf,
    pub node_count: u64,
    pub edge_count: u64,
    pub files_indexed: u64,
    pub elapsed_ms: u64,
    /// Coverage-honesty report (issue #57): which files were parse-incomplete,
    /// skipped, or quarantined. The composition root persists it to the sidecar.
    pub coverage: CoverageReport,
}

/// Outcome of an incremental (changed-files-only) re-index (issue #62).
/// `changed`/`added`/`deleted`/`renamed`/`unchanged` partition the current file
/// tree by class; `files_reparsed` is the number of files whose symbols were
/// re-parsed (changed + added + renamed-new).
///
/// Deliberately carries NO whole-graph node/edge totals: counting every node
/// and edge is a full table scan (~0.3s on this machine, independent of the
/// change size) that would defeat the point of an incremental pass. The change
/// partition is the answer callers need; a caller that wants graph totals can
/// query them (or run with `full`). The one place totals are required — writing
/// the export sidecar — computes them lazily at the composition root.
pub struct IncrementalResult {
    pub graph_path: PathBuf,
    pub changed: u64,
    pub added: u64,
    pub deleted: u64,
    pub renamed: u64,
    pub unchanged: u64,
    pub files_reparsed: u64,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Indexes source files under `codebase_path` into a LadybugDB graph
/// at `graph_path`. Continues on per-file parse errors.
/// Convenience wrapper that auto-detects language by file extension.
#[allow(dead_code)]
pub fn index_codebase(codebase_path: &Path, graph_path: &Path) -> Result<IndexResult, String> {
    index_codebase_with_language(codebase_path, graph_path, &IndexOptions::default())
}

/// Indexes source files per `options` (language filter, dependency scope,
/// user directory exclusions — see `IndexOptions`).
///
/// `options.dependency_scope` controls dependency-directory ingestion (see
/// `DependencyScope`): `None` prunes build/dependency dirs; `PublicApi`
/// descends into them but persists only publicly visible symbols from files
/// under them; `Full` descends and persists everything. `options.exclude_dirs`
/// (issue #249) prunes user-specified directories independent of, and prior
/// to, that tier — it wins even under `Full`.
pub fn index_codebase_with_language(
    codebase_path: &Path,
    graph_path: &Path,
    options: &IndexOptions,
) -> Result<IndexResult, String> {
    let start = Instant::now();
    let store = GraphStore::open_or_create(graph_path)?;
    store.create_schema()?;

    // Coverage-honesty accounting (issue #57): note every File node, and record
    // the parse-incomplete / skipped / quarantined gaps as they occur. Created
    // before the walk so directories pruned by `exclude_dirs`/unreadable
    // permissions (issue #249) fold in immediately.
    let mut collector = CoverageCollector::default();
    let walk_opts = WalkOptions {
        language_filter: options.language_filter,
        dependency_scope: options.dependency_scope,
        exclude_dirs: options.exclude_dirs.clone(),
    };
    let walk_outcome = collect_source_files(codebase_path, walk_opts)?;
    for rel in &walk_outcome.excluded_dirs {
        collector.record_skipped(rel, "user_excluded".to_string());
    }
    for rel in &walk_outcome.unreadable_dirs {
        collector.record_skipped(rel, "unreadable".to_string());
    }
    let source_files = walk_outcome.files;
    let dependency_scope = options.dependency_scope;
    // label_by_qn: qualified_name/id -> label, populated as nodes are created.
    // Used to resolve edge tables without probing the database.
    // source: Fermi audit — probe_node_label was firing up to 9 MATCH queries
    // per edge; the indexer already knows every node's label in memory.
    let mut label_by_qn: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let mut total_bytes: u64 = 0;
    let mut dir_nodes_inserted = std::collections::HashSet::<PathBuf>::new();

    // Symbol nodes + edges accumulate here and flush in large batches; the
    // global id set dedups across the whole run so one duplicate id can never
    // abort a flush (it would take the whole batch down, not one file).
    let mut batch = SymbolBatch::default();
    let mut seen_node_ids = std::collections::HashSet::<(String, String)>::new();
    for file_path in &source_files {
        let rel = relative_path(codebase_path, file_path);
        let rel_str = rel.to_string_lossy();
        // Track cumulative bytes read; abort if we blow past MAX_TOTAL_BYTES.
        // source: H1 fix — prevents DoS by forcing the process to read gigabytes.
        let file_bytes = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        total_bytes = total_bytes.saturating_add(file_bytes);
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "total_bytes_exceeded: aborting walk after {total_bytes} bytes \
                 (MAX_TOTAL_BYTES {MAX_TOTAL_BYTES})"
            ));
        }
        insert_ancestor_dirs(
            &store,
            &mut batch,
            codebase_path,
            file_path,
            &mut dir_nodes_inserted,
            &mut label_by_qn,
        )?;
        insert_file_node(&store, file_path, &rel_str)?;
        label_by_qn
            .entry(rel_str.to_string())
            .or_default()
            .insert("File".into());
        insert_dir_file_edge(&mut batch, &rel);
        // PublicApi tier: filter to public-visibility symbols only for files
        // under dependency directories. Project files are never restricted.
        // source: ADR-4253701 §Decision 1.
        let restrict_to_public_api = dependency_scope == DependencyScope::PublicApi
            && is_dependency_path(codebase_path, file_path);
        let outcome = index_single_file(
            &store,
            &mut batch,
            file_path,
            &rel_str,
            &mut label_by_qn,
            &mut seen_node_ids,
            restrict_to_public_api,
        );
        record_outcome(&mut collector, &rel_str, outcome);
        // Flush once the batch is large enough to amortize the per-call cost,
        // bounding peak memory on large codebases.
        if batch.node_row_count >= SYMBOL_FLUSH_THRESHOLD {
            batch.flush(&store)?;
        }
    }
    batch.flush(&store)?;
    let files_indexed = collector.files_indexed();

    // All-file indexing post-pass: now that every File node exists, recover
    // the import/reference graph of files the AST parsers don't cover — .js
    // family (Imports_File_File) and markdown/shell doc/script cross-
    // references (References_File_File, issue #205). Forward-reference safe
    // (all File nodes present); best-effort (unresolved specifiers skipped).
    // source: all-file indexing.
    match light_link::link_loose_file_imports(&store, codebase_path, &source_files) {
        Ok(n) if n > 0 => {
            eprintln!("indexer: light-linked {n} loose file imports/references (.js/.md/.sh)")
        }
        Ok(_) => {}
        Err(e) => eprintln!("indexer: light-link pass skipped: {e}"),
    }

    // Infrastructure-as-code pass (issue #63): enrich the deployment surface with
    // IacResource/IacModule/IacImage nodes and their reference edges. Runs after
    // every File node exists (like light_link), so a manifest can point at
    // another file's node. Parse gaps flow into the same coverage sidecar as code
    // (issue #57 integration). A pass error is logged, never fatal — the code
    // graph is already complete.
    record_iac_gaps(&mut collector, &store, codebase_path, &source_files);

    let node_count = store.node_count()?;
    let edge_count = store.edge_count()?;
    let elapsed_ms = start.elapsed().as_millis() as u64;

    let mut coverage = CoverageReport::new("full", files_indexed);
    coverage.files = collector.into_files();

    Ok(IndexResult {
        graph_path: graph_path.to_path_buf(),
        node_count,
        edge_count,
        files_indexed,
        elapsed_ms,
        coverage,
    })
}

fn relative_path(root: &Path, file: &Path) -> PathBuf {
    file.strip_prefix(root).unwrap_or(file).to_path_buf()
}

/// Runs the IaC pass over every source file and folds its parse gaps into the
/// coverage collector. The files it touches already have a File node and were
/// already `note_indexed` by the main loop; the IaC pass only ADDS gap records
/// for IaC files it could not fully interpret (templated/malformed manifests,
/// FROM-less Dockerfiles). Best-effort: a pass-level error is logged, not
/// propagated, so IaC parsing can never fail a completed code index.
fn record_iac_gaps(
    collector: &mut CoverageCollector,
    store: &GraphStore,
    codebase_path: &Path,
    source_files: &[PathBuf],
) {
    match iac::run_iac_pass(store, codebase_path, source_files) {
        Ok(gaps) => fold_iac_gaps(collector, gaps),
        Err(e) => eprintln!("indexer: IaC pass skipped: {e}"),
    }
}

/// Records each IaC parse gap into the collector: incomplete parses become
/// ParsePartial (with line ranges), unreadable IaC files become Skipped. Shared
/// by the full and incremental callers so both surface IaC gaps identically.
fn fold_iac_gaps(collector: &mut CoverageCollector, gaps: iac::IacGaps) {
    for (rel, ranges) in gaps.partial {
        collector.record_partial(&rel, ranges);
    }
    for (rel, reason) in gaps.skipped {
        collector.record_skipped(&rel, reason);
    }
}

/// Feeds one file's `ParseOutcome` into the coverage collector, logging the
/// non-clean cases so they are visible in stderr as well as the sidecar.
fn record_outcome(collector: &mut CoverageCollector, rel: &str, outcome: ParseOutcome) {
    match outcome {
        ParseOutcome::Indexed => collector.note_indexed(),
        ParseOutcome::Partial(ranges) => {
            collector.note_indexed();
            collector.record_partial(rel, ranges);
        }
        ParseOutcome::Skipped(reason) => {
            eprintln!("indexer: skipping {rel}: {reason}");
            collector.record_skipped(rel, reason);
        }
        ParseOutcome::Quarantined(reason) => {
            eprintln!("indexer: QUARANTINED {rel}: {reason} (isolated; index continues)");
            collector.record_quarantined(rel, reason);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
