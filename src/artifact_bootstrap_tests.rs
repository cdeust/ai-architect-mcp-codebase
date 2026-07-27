//! Issue #55 staleness contract at the handler seam (`attempt_bootstrap`):
//!   (b) stale artifact, default policy → REFUSE + reindex, with an
//!       assertable `bootstrap_skipped` note carrying commits_behind;
//!   (c) stale artifact + accept_stale → import anyway, response carries a
//!       `stale_artifact` {artifact_sha, head_sha, commits_behind} report.
//! Each uses a real git fixture, a real index, and a real export, so the
//! whole path (git sha compare → rev-list count → JSON shaping) is covered.
use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SRC: &str = "fn main() { helper(); }\nfn helper() -> i32 { 1 }\n";

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Builds a git fixture repo, indexes it, exports the artifact, then makes
/// one further commit so the committed artifact is exactly 1 commit stale.
/// Returns (repo, fresh_output_dir, fresh_graph_dir).
fn stale_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::Builder::new()
        .prefix("artifact_stale_")
        .tempdir()
        .expect("tempdir");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(repo.join("src")).expect("mk src");
    fs::write(repo.join("src/main.rs"), SRC).expect("write src");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@ap.dev"]);
    git(&repo, &["config", "user.name", "AP"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "initial"]);

    let out1 = tmp.path().join("out1");
    let graph1 = out1.join("graph");
    fs::create_dir_all(&out1).expect("mk out1");
    // Index the repo ROOT (not repo/src): the bootstrap codebase must equal
    // the indexed root so File node ids line up with git-diff paths, exactly
    // as the real handler wires them (path == bootstrap codebase). The
    // manifest is bundled so the export mirrors a real `export_artifact=true`.
    let manifest_path = indexer::manifest::manifest_path(&out1);
    let result = indexer::index_codebase(&repo, &graph1).expect("index");
    indexer::write_full_manifest(&repo, &manifest_path, None, indexer::DependencyScope::None)
        .expect("manifest");
    artifact::export_artifact(
        &graph1,
        &repo,
        result.node_count,
        result.edge_count,
        Some(&manifest_path),
        None,
    )
    .expect("export");

    // Move HEAD forward by one commit → artifact is now 1 commit stale.
    fs::write(repo.join("src/extra.rs"), "pub fn added() {}\n").expect("write extra");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "--no-gpg-sign", "-m", "second"]);

    let fresh_out = tmp.path().join("fresh");
    let fresh_graph = fresh_out.join("graph");
    fs::create_dir_all(&fresh_out).expect("mk fresh");
    (tmp, repo, fresh_out, fresh_graph)
}

/// Convenience: `attempt_bootstrap` with the default (no language filter,
/// dependency scope None) wiring the two tests below share.
fn bootstrap(
    repo: &Path,
    fresh_out: &Path,
    fresh_graph: &Path,
    accept_stale: bool,
) -> BootstrapOutcome {
    let manifest_path = indexer::manifest::manifest_path(fresh_out);
    attempt_bootstrap(
        repo,
        fresh_out,
        fresh_graph,
        &manifest_path,
        accept_stale,
        None,
        indexer::DependencyScope::None,
    )
}

#[test]
fn stale_artifact_default_imports_and_fills() {
    // Evolved contract (issue #62 completes #55): the DEFAULT for a stale
    // artifact is bootstrap THEN incremental fill — no longer refuse.
    let (_tmp, repo, fresh_out, fresh_graph) = stale_fixture();

    let outcome = bootstrap(&repo, &fresh_out, &fresh_graph, false);
    match outcome {
        BootstrapOutcome::Imported(resp) => {
            assert_eq!(resp["source"], json!("artifact_bootstrap_fill"));
            assert_eq!(resp["graph_state"], json!("filled_to_working_tree"));
            assert_eq!(resp["fill_method"], json!("git_diff"));
            assert_eq!(
                resp["added"],
                json!(1),
                "the one commit added src/extra.rs → fill must add exactly one file"
            );
            assert_eq!(resp["artifact_commits_behind"], json!(1));
        }
        other => panic!("expected Imported(resp), got {other:?}"),
    }
    assert!(
        fresh_graph.exists(),
        "the default fill path must materialise the imported+filled graph"
    );
    // The filled graph must now contain the added file's symbol.
    let store = graph_store::GraphStore::open_or_create(&fresh_graph).expect("open");
    let q = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'added' RETURN f.name")
        .expect("query");
    assert!(
        !q.rows.is_empty(),
        "fill must have indexed src/extra.rs::added into the bootstrapped graph"
    );
}

#[test]
fn accept_stale_imports_as_is_and_reports_staleness() {
    // accept_stale is repurposed: import the stale snapshot AND SKIP the
    // fill, but always report the staleness (no silent staleness).
    let (_tmp, repo, fresh_out, fresh_graph) = stale_fixture();

    let outcome = bootstrap(&repo, &fresh_out, &fresh_graph, true);
    match outcome {
        BootstrapOutcome::Imported(resp) => {
            assert_eq!(resp["source"], json!("artifact_bootstrap"));
            assert_eq!(resp["graph_state"], json!("accepted_stale"));
            let stale = &resp["stale_artifact"];
            assert_eq!(
                stale["commits_behind"],
                json!(1),
                "accepted-stale response must carry the staleness report"
            );
            assert!(stale["artifact_sha"]
                .as_str()
                .is_some_and(|s| !s.is_empty()));
            assert!(stale["head_sha"].as_str().is_some_and(|s| !s.is_empty()));
        }
        other => panic!("expected Imported(resp), got {other:?}"),
    }
    assert!(
        fresh_graph.exists(),
        "accept_stale must materialise the imported graph"
    );
    // Skipping the fill means the added file is NOT yet in the graph.
    let store = graph_store::GraphStore::open_or_create(&fresh_graph).expect("open");
    let q = store
        .execute_query("MATCH (f:Function) WHERE f.name = 'added' RETURN f.name")
        .expect("query");
    assert!(
        q.rows.is_empty(),
        "accept_stale must NOT fill — the stale graph lacks the new commit's symbol"
    );
}
