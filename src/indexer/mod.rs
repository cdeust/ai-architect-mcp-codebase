// indexer — Walk + Parse + Persist pipeline for multi-language codebases.
//
// Wires graph_store (step 1) and parser module (step 2) into an indexer
// that processes a full directory of source files. Supports Rust, Python,
// and TypeScript. Zero dependency on main.rs.

use crate::graph_store::{GraphStore, PropEdgeList};
use crate::parser::Language;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub mod coverage;
mod iac;
mod incremental;
mod light_link;
pub mod manifest;
mod persist;
mod walk;

use coverage::{CoverageCollector, CoverageReport};
pub use incremental::{
    fill_after_bootstrap, index_incremental, write_full_manifest, FillMethod, FillResult,
};
use persist::{
    index_single_file, insert_ancestor_dirs, insert_dir_file_edge, insert_file_node, ParseOutcome,
};
pub use walk::DependencyScope;
use walk::{collect_source_files, is_dependency_path, WalkOptions};

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

// source: security hardening — per-file byte cap BEFORE handing to tree-sitter.
// Even within MAX_FILE_BYTES, 1 MB is sufficient for any realistic source file
// and bounds tree-sitter parse work per file.
pub const MAX_PARSE_BYTES: u64 = 1_048_576;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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

// Flush the accumulated symbol batch once it holds this many node rows.
// source: measured April 2026 (Fermi scalability audit) — bulk-inserting
// symbols PER FILE issued ~15 small bulk calls per file (one per label/edge
// table), each paying prepare-lookup + FFI round-trip overhead; at 500 files
// that was ~131 s of the 140 s indexing time. Accumulating across files and
// flushing in large batches turns ~7500 small calls into a few dozen large
// ones. 5000 rows bounds peak batch memory (~1-2 MB) while fully amortizing
// the per-call overhead; the existing BULK_BATCH_SIZE (500) still chunks each
// bulk call internally.
const SYMBOL_FLUSH_THRESHOLD: usize = 5_000;

/// Accumulates parsed nodes and edges across many files so they can be
/// bulk-inserted in large batches instead of one small bulk call per file.
///
/// Safe because every edge the indexer emits is intra-file (Defines/HasMethod/
/// HasField/HasVariant, File→symbol) — there are no cross-file edges at index
/// time (Calls/Uses are resolved later). On flush, all nodes are inserted
/// before any edge, so every edge finds its endpoints. File/Directory nodes
/// (inserted eagerly per file) already exist when the symbol batch flushes.
#[derive(Default)]
struct SymbolBatch {
    nodes: HashMap<String, Vec<Vec<(String, String)>>>,
    edges: HashMap<String, PropEdgeList>,
    node_row_count: usize,
}

impl SymbolBatch {
    fn push_node(&mut self, label: &str, row: Vec<(String, String)>) {
        self.nodes.entry(label.to_string()).or_default().push(row);
        self.node_row_count += 1;
    }

    fn push_edge(&mut self, table: &str, from: String, to: String, props: Vec<(String, String)>) {
        self.edges
            .entry(table.to_string())
            .or_default()
            .push((from, to, props));
    }

    /// Inserts every accumulated node (all labels) and THEN every accumulated
    /// edge (all tables), so edges always resolve their endpoints. Empties
    /// the batch.
    fn flush(&mut self, store: &GraphStore) -> Result<(), String> {
        for (label, rows) in self.nodes.drain() {
            store.bulk_insert_nodes(&label, &rows)?;
        }
        for (table, edges) in self.edges.drain() {
            store.bulk_insert_edges(&table, &edges)?;
        }
        self.node_row_count = 0;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Indexes source files under `codebase_path` into a LadybugDB graph
/// at `graph_path`. Continues on per-file parse errors.
/// Convenience wrapper that auto-detects language by file extension.
#[allow(dead_code)]
pub fn index_codebase(codebase_path: &Path, graph_path: &Path) -> Result<IndexResult, String> {
    index_codebase_with_language(codebase_path, graph_path, None, DependencyScope::None)
}

/// Indexes source files with an optional language filter.
///
/// `dependency_scope` controls dependency-directory ingestion (see
/// `DependencyScope`): `None` prunes build/dependency dirs; `PublicApi`
/// descends into them but persists only publicly visible symbols from files
/// under them; `Full` descends and persists everything.
pub fn index_codebase_with_language(
    codebase_path: &Path,
    graph_path: &Path,
    language_filter: Option<Language>,
    dependency_scope: DependencyScope,
) -> Result<IndexResult, String> {
    let start = Instant::now();
    let store = GraphStore::open_or_create(graph_path)?;
    store.create_schema()?;

    let walk_opts = WalkOptions {
        language_filter,
        dependency_scope,
    };
    let source_files = collect_source_files(codebase_path, walk_opts)?;
    // label_by_qn: qualified_name/id -> label, populated as nodes are created.
    // Used to resolve edge tables without probing the database.
    // source: Fermi audit — probe_node_label was firing up to 9 MATCH queries
    // per edge; the indexer already knows every node's label in memory.
    let mut label_by_qn: HashMap<String, String> = HashMap::new();
    let mut total_bytes: u64 = 0;
    let mut dir_nodes_inserted = std::collections::HashSet::<PathBuf>::new();
    // Coverage-honesty accounting (issue #57): note every File node, and record
    // the parse-incomplete / skipped / quarantined gaps as they occur.
    let mut collector = CoverageCollector::default();

    // Symbol nodes + edges accumulate here and flush in large batches; the
    // global id set dedups across the whole run so one duplicate id can never
    // abort a flush (it would take the whole batch down, not one file).
    let mut batch = SymbolBatch::default();
    let mut seen_node_ids = std::collections::HashSet::<String>::new();
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
        label_by_qn.insert(rel_str.to_string(), "File".into());
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
    // the import graph of files the AST parsers don't cover (.js family) as
    // Imports_File_File edges. Forward-reference safe (all File nodes present);
    // best-effort (unresolved specifiers skipped). source: all-file indexing.
    match light_link::link_loose_file_imports(&store, codebase_path, &source_files) {
        Ok(n) if n > 0 => eprintln!("indexer: light-linked {n} loose file imports (.js family)"),
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
mod tests {
    use super::*;

    #[test]
    fn test_index_own_project() {
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let tmp = tempfile::Builder::new()
            .prefix("indexer_test_graph_")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&tmp);
        // Ensure the directory is fully gone before creating a fresh DB.
        assert!(!tmp.exists(), "failed to clean temp dir: {}", tmp.display());

        let result = index_codebase(Path::new("src"), &tmp).unwrap();

        assert!(
            result.files_indexed >= 3,
            "should index at least main.rs + graph_store.rs + lib.rs, got {}",
            result.files_indexed
        );
        assert!(
            result.node_count > 50,
            "should have many nodes, got {}",
            result.node_count
        );
        assert!(
            result.edge_count > 30,
            "should have many edges, got {}",
            result.edge_count
        );

        // Verify a known function exists via Cypher
        let store = GraphStore::open_or_create(&tmp).unwrap();
        let qr = store
            .execute_query("MATCH (f:Function) WHERE f.name = 'main' RETURN f.name")
            .unwrap();
        assert!(!qr.rows.is_empty(), "should find main() in graph");
        assert_eq!(qr.rows[0][0], "main");

        // Verify file nodes exist
        let qr2 = store
            .execute_query("MATCH (f:File) RETURN count(f) AS cnt")
            .unwrap();
        assert!(
            !qr2.rows.is_empty() && qr2.rows[0][0].parse::<u64>().unwrap_or(0) > 0,
            "should have File nodes"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Shared fixture for the all-file-indexing tests below: code (AST) + JS
    /// (light-link) + plain docs + structured data + BINARY docs, indexed
    /// into a fresh graph. `tag` keeps each test's temp dirs distinct.
    /// Split out of a single 96-line test (coding-standards §4.2) into two
    /// focused tests that share this setup — one concern each (file-type
    /// coverage vs. light-link edges), not a behavior change.
    fn build_all_file_fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, IndexResult) {
        use std::io::Write;
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let root = tempfile::Builder::new()
            .prefix(&format!("indexer_allfile_test_{tag}_"))
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("js")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();

        std::fs::write(root.join("mod.py"), "def f():\n    return 1\n").unwrap();
        std::fs::write(
            root.join("js/app.js"),
            "import { u } from './util.js';\nconst x = require('./util');\n",
        )
        .unwrap();
        std::fs::write(root.join("js/util.js"), "export const u = 1;\n").unwrap();
        // Markdown doc that references the python module and another doc.
        std::fs::write(
            root.join("docs/guide.md"),
            "# Guide\nSee [the code](../mod.py) and [arch](./arch.md).\n",
        )
        .unwrap();
        std::fs::write(root.join("docs/arch.md"), "# Arch\n").unwrap();
        std::fs::write(root.join("config.json"), "{\"k\": 1}\n").unwrap();
        std::fs::write(root.join("notes.txt"), "plain text\n").unwrap();
        // Binary documents: arbitrary non-UTF8 bytes (a real .pdf/.docx header).
        std::fs::File::create(root.join("report.pdf"))
            .unwrap()
            .write_all(&[0x25, 0x50, 0x44, 0x46, 0x2d, 0x00, 0xff, 0xfe, 0x01])
            .unwrap();
        std::fs::File::create(root.join("spec.docx"))
            .unwrap()
            .write_all(&[0x50, 0x4b, 0x03, 0x04, 0x00, 0xff, 0x00, 0x12])
            .unwrap();

        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let tmp = tempfile::Builder::new()
            .prefix(&format!("indexer_allfile_graph_{tag}_"))
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&tmp);
        let result = index_codebase(&root, &tmp).unwrap();
        (root, tmp, result)
    }

    #[test]
    fn test_all_file_indexing_covers_every_file_type() {
        // All-file indexing: EVERY file becomes a File node — code, plain-text
        // docs, structured data, AND binary documents (.pdf/.docx).
        let (root, tmp, result) = build_all_file_fixture("counts");
        let store = GraphStore::open_or_create(&tmp).unwrap();

        // 9 files total — including the two BINARY documents.
        let files = store
            .execute_query("MATCH (f:File) RETURN count(f) AS n")
            .unwrap();
        assert_eq!(
            files.rows[0][0].parse::<u64>().unwrap(),
            9,
            "every file — code, text docs, data, AND binary .pdf/.docx — must be a File node"
        );

        // Each document type is a navigable File node (binary included).
        for id in [
            "docs/guide.md",
            "config.json",
            "notes.txt",
            "report.pdf",
            "spec.docx",
            "js/app.js",
        ] {
            let q = store
                .execute_query(&format!("MATCH (f:File) WHERE f.id = '{id}' RETURN f.id"))
                .unwrap();
            assert!(!q.rows.is_empty(), "missing File node for document: {id}");
        }

        assert!(result.node_count >= 9);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_all_file_indexing_light_links_markdown_and_js() {
        // Text docs get light links: Markdown `[..](path)` → References,
        // JS import/require → Imports.
        let (root, tmp, _result) = build_all_file_fixture("links");
        let store = GraphStore::open_or_create(&tmp).unwrap();

        // Markdown light-linking: guide.md references mod.py and arch.md.
        let refs = store
            .execute_query(
                "MATCH (a:File)-[:References_File_File]->(b:File) \
                 WHERE a.id = 'docs/guide.md' RETURN b.id",
            )
            .unwrap();
        let ref_targets: Vec<&str> = refs.rows.iter().map(|r| r[0].as_str()).collect();
        assert!(
            ref_targets.contains(&"mod.py") && ref_targets.contains(&"docs/arch.md"),
            "guide.md should reference mod.py + docs/arch.md, got {ref_targets:?}"
        );

        // JS light-linking: app.js imports util.js.
        let imp = store
            .execute_query(
                "MATCH (a:File)-[:Imports_File_File]->(b:File) \
                 WHERE a.id = 'js/app.js' RETURN b.id",
            )
            .unwrap();
        assert!(
            imp.rows.iter().any(|r| r[0] == "js/util.js"),
            "js/app.js should import js/util.js, got {:?}",
            imp.rows
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn test_symlink_skipped() {
        // source: C4 fix — symlinks in the walked tree must not be followed.
        // We build a small directory with one real .rs file and one symlink
        // that points to a file OUTSIDE the tree (a decoy `/etc/hostname`).
        // collect_source_files must return only the real file.
        use std::os::unix::fs::symlink;

        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let root = tempfile::Builder::new()
            .prefix("indexer_symlink_test_")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        // Real file: a simple Rust source.
        let real_file = root.join("real.rs");
        std::fs::write(&real_file, "fn main() {}\n").unwrap();

        // Symlink → /etc/hostname (exists on macOS, has the .rs extension
        // faked via the link name so the walker would pick it up if it
        // followed symlinks).
        let link = root.join("leaky.rs");
        // Guard: if the target doesn't exist, use another known file.
        let target = if Path::new("/etc/hostname").exists() {
            Path::new("/etc/hostname")
        } else {
            Path::new("/etc/passwd")
        };
        symlink(target, &link).unwrap();

        let files = collect_source_files(&root, WalkOptions::default()).unwrap();
        // Only the real file is indexed; the symlink is skipped.
        assert_eq!(files.len(), 1, "symlink must not be collected: {files:?}");
        assert_eq!(files[0].file_name().unwrap(), "real.rs");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_dependency_scope_walk() {
        // Proves DependencyScope toggles descent into build/dependency dirs
        // while always excluding `.git`.
        // Fixture: root/app.rs, root/node_modules/dep.rs, root/.git/hook.rs.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let root = tempfile::Builder::new()
            .prefix("indexer_include_deps_test_")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("app.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join("node_modules/dep.rs"), "fn dep() {}\n").unwrap();
        std::fs::write(root.join(".git/hook.rs"), "fn hook() {}\n").unwrap();

        let names = |opts: WalkOptions| -> Vec<String> {
            let mut v: Vec<String> = collect_source_files(&root, opts)
                .unwrap()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };

        // Default (DependencyScope::None): node_modules and .git are both pruned.
        assert_eq!(names(WalkOptions::default()), vec!["app.rs"]);

        // Full: node_modules is descended, .git stays out.
        let full = WalkOptions {
            language_filter: None,
            dependency_scope: DependencyScope::Full,
        };
        assert_eq!(names(full), vec!["app.rs", "dep.rs"]);

        // PublicApi also descends at the walk level — the visibility filter
        // is applied at persistence time (see persist::tests), not here.
        let public_api = WalkOptions {
            language_filter: None,
            dependency_scope: DependencyScope::PublicApi,
        };
        assert_eq!(names(public_api), vec!["app.rs", "dep.rs"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_public_api_scope_filters_dependency_symbols_only() {
        // Fixture: a project file (app.rs, one pub + one private fn) and a
        // dependency file under node_modules (dep.rs, one pub + one private
        // fn). PublicApi must drop the PRIVATE fn from dep.rs only — the
        // project file's private fn stays, and dep.rs's pub fn stays too.
        // source: ADR-4253701 §Decision 1.
        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let root = tempfile::Builder::new()
            .prefix("indexer_public_api_test_")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(
            root.join("app.rs"),
            "pub fn app_pub() {}\nfn app_private() {}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("node_modules/dep.rs"),
            "pub fn dep_pub() {}\nfn dep_private() {}\n",
        )
        .unwrap();

        // issue #25 audit: process::id() collides across processes under PID
        // reuse; tempfile's random suffix does not.
        let graph_path = tempfile::Builder::new()
            .prefix("indexer_public_api_graph_")
            .tempdir()
            .expect("create temp dir")
            .keep();
        let _ = std::fs::remove_dir_all(&graph_path);

        index_codebase_with_language(&root, &graph_path, None, DependencyScope::PublicApi)
            .expect("index should succeed");

        let store = GraphStore::open_or_create(&graph_path).unwrap();
        let qr = store
            .execute_query("MATCH (f:Function) RETURN f.name")
            .unwrap();
        let mut names: Vec<String> = qr.rows.into_iter().map(|r| r[0].clone()).collect();
        names.sort();

        assert_eq!(
            names,
            vec![
                "app_private".to_string(),
                "app_pub".to_string(),
                "dep_pub".to_string()
            ],
            "PublicApi must keep both project functions and only the pub dependency function"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&graph_path);
    }
}
