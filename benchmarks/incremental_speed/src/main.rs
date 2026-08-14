// incremental_speed_bench — perf claim for #62/#55, re-homed out of the
// correctness suite (issue #74).
//
// A wall-clock ratio ("incremental/fill is much cheaper than full") is NOT a
// deterministic property — it varies with hardware and build profile, and on
// fast hardware the fixed-cost floor of the short operation makes the RATIO
// worse even as both absolute times improve (issue #74). So it does not belong
// in a green-everywhere correctness gate. It lives here instead, following the
// `benchmarks/token_surface/` pattern: drive the REAL library over a fixed git
// fixture, take the least-contended (minimum) of several runs, and write
// results.json + a MANIFEST recording hardware/toolchain/date/command.
//
// Two measurements on the same fixture:
//   * incremental re-index after a 4-file mutation vs a full re-index;
//   * artifact bootstrap-fill after a fresh clone vs a full index of HEAD.

use ai_architect_mcp::artifact;
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer::{self, manifest, IndexOptions};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Files in the fixture. Large enough that the full index has real, dominant
/// cost relative to a 4-file incremental/fill.
const N_MODULES: usize = 260;
/// Best-of-N: the minimum over several runs is the least-contended (truest)
/// measure of the algorithm's cost.
const ATTEMPTS: usize = 5;

fn module_body(i: usize) -> String {
    let mut s = String::new();
    for j in 0..8 {
        s.push_str(&format!(
            "def mod{i}_fn{j}(x):\n    return x + {i} + {j}\n\n"
        ));
    }
    for k in 0..3 {
        s.push_str(&format!(
            "class Widget{i}_{k}:\n    def spin(self):\n        return {i}\n    \
             def flip(self, y):\n        return y\n\n"
        ));
    }
    s
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git(repo: &Path) {
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "b@ap.dev"]);
    git(repo, &["config", "user.name", "AP Bench"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

/// Recursively copies a graph path (single-file or dir layout) so each best-of-N
/// incremental attempt starts from the same pristine state without re-indexing.
fn copy_path(src: &Path, dst: &Path) {
    let meta = std::fs::symlink_metadata(src).expect("stat");
    if meta.is_dir() {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            copy_path(&e.path(), &dst.join(e.file_name()));
        }
    } else {
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::copy(src, dst).unwrap();
    }
}

fn apply_mutation(repo: &Path) {
    std::fs::write(
        repo.join("src/mod000.py"),
        "def brand_new_fn():\n    return 999\n\ndef another_new():\n    return 0\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/mod_added.py"),
        "def added_symbol():\n    return 1\n",
    )
    .unwrap();
    std::fs::remove_file(repo.join("src/mod001.py")).unwrap();
    std::fs::rename(
        repo.join("src/mod002.py"),
        repo.join("src/mod002_renamed.py"),
    )
    .unwrap();
}

/// Builds the fixture: a git repo of N symbol-dense modules with a baseline
/// full index + manifest + exported artifact, then a 4-file mutation commit
/// (so the artifact/base is stale by exactly that diff). Returns
/// `(repo, base_graph, base_manifest)`.
fn build_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for i in 0..N_MODULES {
        std::fs::write(repo.join(format!("src/mod{i:03}.py")), module_body(i)).unwrap();
    }
    init_git(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let base_out = root.join("base");
    std::fs::create_dir_all(&base_out).unwrap();
    let base_graph = base_out.join("graph");
    let base_manifest = manifest::manifest_path(&base_out);
    let result =
        indexer::index_codebase_with_language(&repo, &base_graph, &IndexOptions::default())
            .unwrap();
    indexer::write_full_manifest(&repo, &base_manifest, &IndexOptions::default()).unwrap();
    artifact::export_artifact(
        &base_graph,
        &repo,
        result.node_count,
        result.edge_count,
        Some(&base_manifest),
        None,
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "artifact"]);

    apply_mutation(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "changes"]);
    (repo, base_graph, base_manifest)
}

/// Full index of the mutated tree, best-of-N minimum wall-clock.
fn measure_full(root: &Path, repo: &Path) -> u64 {
    let mut full_ms = u64::MAX;
    for a in 0..ATTEMPTS {
        let g = root.join(format!("full_{a}"));
        let t = Instant::now();
        indexer::index_codebase_with_language(repo, &g, &IndexOptions::default()).unwrap();
        full_ms = full_ms.min(t.elapsed().as_millis() as u64);
    }
    full_ms
}

/// Incremental re-index (copying the pristine pre-mutation graph each
/// attempt so classify runs against the same stale base), best-of-N.
fn measure_incremental(root: &Path, repo: &Path, base_graph: &Path, base_manifest: &Path) -> u64 {
    let mut inc_ms = u64::MAX;
    for a in 0..ATTEMPTS {
        let g = root.join(format!("inc_g_{a}"));
        let m = root.join(format!("inc_m_{a}.json"));
        copy_path(base_graph, &g);
        std::fs::copy(base_manifest, &m).unwrap();
        let prior = manifest::load(&m).unwrap();
        let t = Instant::now();
        indexer::index_incremental(repo, &g, &m, &IndexOptions::default(), &prior).unwrap();
        inc_ms = inc_ms.min(t.elapsed().as_millis() as u64);
    }
    inc_ms
}

/// Artifact bootstrap-fill after a fresh clone, best-of-N.
fn measure_bootstrap_fill(root: &Path, repo: &Path) -> u64 {
    let clone = root.join("clone");
    let out = Command::new("git")
        .args(["clone", "-q"])
        .arg(repo)
        .arg(&clone)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut fill_ms = u64::MAX;
    for a in 0..ATTEMPTS {
        let bo = root.join(format!("boot_{a}"));
        std::fs::create_dir_all(&bo).unwrap();
        let bg = bo.join("graph");
        let bm = manifest::manifest_path(&bo);
        let meta = artifact::import_artifact(&clone, &bg).unwrap();
        let imported = manifest::load(&bm);
        let t = Instant::now();
        indexer::fill_after_bootstrap(
            &clone,
            &bg,
            &bm,
            &meta.commit,
            imported.as_ref(),
            &IndexOptions::default(),
        )
        .unwrap();
        fill_ms = fill_ms.min(t.elapsed().as_millis() as u64);
    }
    fill_ms
}

fn main() {
    let tmp = tempfile::Builder::new()
        .prefix("incr_speed_")
        .tempdir()
        .expect("tmp");
    let root = tmp.path();

    let (repo, base_graph, base_manifest) = build_fixture(root);
    let full_ms = measure_full(root, &repo);
    let inc_ms = measure_incremental(root, &repo, &base_graph, &base_manifest);
    let fill_ms = measure_bootstrap_fill(root, &repo);
    report(&base_graph, full_ms, inc_ms, fill_ms);
}

/// Writes results.json and prints the human-readable summary.
fn report(base_graph: &Path, full_ms: u64, inc_ms: u64, fill_ms: u64) {
    let inc_ratio = pct(inc_ms, full_ms);
    let fill_ratio = pct(fill_ms, full_ms);
    let store = GraphStore::open_or_create(base_graph).unwrap();
    let nodes = store.node_count().unwrap_or(0);
    let edges = store.edge_count().unwrap_or(0);

    let results = json!({
        "fixture": { "files": N_MODULES, "nodes": nodes, "edges": edges, "diff_files": 4 },
        "attempts_min_of": ATTEMPTS,
        "profile_note": "run this bench with --release for the perf claim; the profile is recorded in MANIFEST",
        "full_index_ms": full_ms,
        "incremental_ms": inc_ms,
        "bootstrap_fill_ms": fill_ms,
        "incremental_vs_full_pct": inc_ratio,
        "bootstrap_fill_vs_full_pct": fill_ratio,
        "arch": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "caveat": "Wall-clock RATIOS are hardware- and profile-dependent: the short op (incremental/fill) has a fixed-cost floor (git diff, DB open, re-parse a few files), so on fast hardware where the full index is quick the RATIO gets WORSE even as both absolute times improve. That is why this is a benchmark with recorded hardware, NOT a correctness gate (issue #74).",
    });

    let out_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("results.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&results).unwrap()).unwrap();

    println!(
        "== incremental_speed_bench ({} {}) ==",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    println!(
        "fixture: {N_MODULES} files → {nodes} nodes, {edges} edges; 4-file diff; min of {ATTEMPTS}"
    );
    println!("full index:      {full_ms} ms");
    println!("incremental:     {inc_ms} ms  ({inc_ratio}% of full)");
    println!("bootstrap-fill:  {fill_ms} ms  ({fill_ratio}% of full)");
    println!("wrote {}", out_path.display());
}

fn pct(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    part * 100 / whole
}
