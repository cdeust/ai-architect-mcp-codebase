// crossover — where the incremental refresh stops paying (issue #443).
//
// `analyze_codebase` reuses the previous graph and runs the incremental pass
// whenever the reuse conditions hold, whatever the size of the change set.
// Each changed file costs a purge, an inbound-edge snapshot and a re-parse,
// and the resolver then retracts and decides again what those files touched;
// a full rebuild parses every file once into an empty graph. This binary
// measures, on a corpus, both paths for a growing share of edited files, so a
// switch-over threshold is read from numbers instead of chosen.
//
// For each share it: restores the corpus, builds the base graph the way
// analyze does (manifest snapshot, full index, coverage sidecar, resolve),
// edits that share of the files, then times
//   * incremental: copy of the base graph, `index_incremental`, `resolve_graph`;
//   * full: manifest snapshot, fresh graph, `index_codebase_with_language`,
//     `resolve_graph`;
// best of `ATTEMPTS`. Clustering and the search index run over the whole
// graph on both paths, so they are left out.
//
// Usage: `cargo run -p incremental-speed-bench --release --bin crossover -- <corpus_dir>`
// (default: the repository's own `src/`). Prints one JSON object.

use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, coverage, manifest, IndexOptions};
use ai_architect_mcp::resolver;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

const ATTEMPTS: usize = 3;
/// Shares of the corpus's files edited, in percent.
const SHARES: &[usize] = &[1, 5, 10, 20, 30, 40, 50, 75, 100];

fn copy_tree(src: &Path, dst: &Path) {
    let meta = std::fs::symlink_metadata(src).expect("stat");
    if meta.is_dir() {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            copy_tree(&e.path(), &dst.join(e.file_name()));
        }
    } else if meta.is_file() {
        std::fs::copy(src, dst).unwrap();
    }
}

/// Removes a graph path whatever its layout (lbug writes a single file or a
/// directory), so a reused name never hands the next index a populated graph.
fn remove_path(p: &Path) {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p).unwrap(),
        Ok(_) => std::fs::remove_file(p).unwrap(),
        Err(_) => {}
    }
    // lbug keeps a write-ahead log beside a single-file database.
    let _ = std::fs::remove_file(p.with_extension("wal"));
}

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            source_files(&p, out);
        } else if matches!(
            p.extension().and_then(|x| x.to_str()),
            Some("rs" | "py" | "go" | "ts" | "js" | "java")
        ) {
            out.push(p);
        }
    }
}

/// An edit every supported language parses: a comment line plus a new
/// function in the file's own syntax, so each edited file gains a symbol.
fn edit(path: &Path, i: usize) {
    let mut s = std::fs::read_to_string(path).unwrap_or_default();
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
    s.push_str(&match ext {
        "py" => format!("\n\ndef crossover_added_{i}(x):\n    return x\n"),
        "go" => format!("\n\nfunc crossoverAdded{i}(x int) int {{ return x }}\n"),
        "rs" => format!("\n\n#[allow(dead_code)]\nfn crossover_added_{i}(x: u32) -> u32 {{ x }}\n"),
        _ => format!("\n\nfunction crossoverAdded{i}(x) {{ return x; }}\n"),
    });
    std::fs::write(path, s).unwrap();
}

/// The base graph as analyze leaves it: manifest snapshot taken before the
/// index, coverage sidecar, resolved edges.
fn build_base(repo: &Path, out: &Path) -> PathBuf {
    remove_path(out);
    std::fs::create_dir_all(out).unwrap();
    let graph = out.join("graph");
    let opts = IndexOptions::default();
    let snapshot = indexer::full_manifest(repo, &opts).unwrap();
    let r = indexer::index_codebase_with_language(repo, &graph, &opts).unwrap();
    coverage::save(&coverage::coverage_path(out), &r.coverage).unwrap();
    manifest::save(&manifest::manifest_path(out), &snapshot).unwrap();
    let store = GraphStore::open_or_create(&graph).unwrap();
    resolver::resolve_graph(&store).unwrap();
    graph
}

fn time_incremental(repo: &Path, base_out: &Path, scratch: &Path) -> (u64, bool) {
    let mut best = u64::MAX;
    let mut scope_kept = true;
    for a in 0..ATTEMPTS {
        let out = scratch.join(format!("inc_{a}"));
        remove_path(&out);
        copy_tree(base_out, &out);
        let graph = out.join("graph");
        let mpath = manifest::manifest_path(&out);
        let prior = manifest::load(&mpath).unwrap();
        let t = Instant::now();
        let inc =
            indexer::index_incremental(repo, &graph, &mpath, &IndexOptions::default(), &prior)
                .unwrap();
        let store = GraphStore::open_or_create(&graph).unwrap();
        resolver::resolve_graph(&store).unwrap();
        best = best.min(t.elapsed().as_millis() as u64);
        scope_kept &= inc.scope_kept;
    }
    (best, scope_kept)
}

fn time_full(repo: &Path, scratch: &Path) -> u64 {
    let mut best = u64::MAX;
    for a in 0..ATTEMPTS {
        let graph = scratch.join(format!("full_{a}"));
        remove_path(&graph);
        let t = Instant::now();
        // analyze's full path snapshots the manifest before it indexes.
        indexer::full_manifest(repo, &IndexOptions::default()).unwrap();
        indexer::index_codebase_with_language(repo, &graph, &IndexOptions::default()).unwrap();
        let store = GraphStore::open_or_create(&graph).unwrap();
        resolver::resolve_graph(&store).unwrap();
        best = best.min(t.elapsed().as_millis() as u64);
    }
    best
}

fn main() {
    let corpus = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src"));
    let tmp = tempfile::Builder::new()
        .prefix("crossover_")
        .tempdir()
        .unwrap();
    let pristine = tmp.path().join("pristine");
    copy_tree(&corpus, &pristine);
    let mut files = Vec::new();
    source_files(&pristine, &mut files);
    files.sort();
    let n = files.len();
    let repo = tmp.path().join("repo");
    let mut rows = Vec::new();
    for &share in SHARES {
        remove_path(&repo);
        copy_tree(&pristine, &repo);
        let base_out = tmp.path().join("base");
        build_base(&repo, &base_out);
        let k = (n * share).div_ceil(100).max(1);
        // Spread the edits over the tree: every (n/k)-th file.
        for i in 0..k {
            let rel = files[i * n / k].strip_prefix(&pristine).unwrap();
            edit(&repo.join(rel), i);
        }
        let (inc_ms, scope_kept) = time_incremental(&repo, &base_out, tmp.path());
        let full_ms = time_full(&repo, tmp.path());
        eprintln!("share {share:>3}% ({k:>4} files): incremental {inc_ms} ms, full {full_ms} ms");
        rows.push(json!({
            "share_pct": share, "edited_files": k,
            "incremental_ms": inc_ms, "full_ms": full_ms, "scope_kept": scope_kept,
        }));
    }
    let report = json!({
        "corpus": corpus.display().to_string(), "files": n,
        "attempts_min_of": ATTEMPTS, "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS, "cpus": std::thread::available_parallelism().map(|p| p.get()).unwrap_or(0),
        "rows": rows,
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
