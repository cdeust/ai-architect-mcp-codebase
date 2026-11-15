// detect_changes_snapshots_319 — issue #319: compare two tagged snapshots of
// a repository line by line, each side read against the graph indexed at its
// own tag. Real git repository, real indexer, no LSP.
//
// Between `v1` and `v2`: `keep.py` keeps `stays` (body changed), loses `goes`
// and gains `arrives`; `old.py` is deleted; `new.py` is added.

use ai_architect_mcp::git_diff::{compare_snapshots, ChangedSymbol, RefRange};
use ai_architect_mcp::graph_store::GraphStore;
use ai_architect_mcp::indexer;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
mod common;
use common::TempDirExt;

const KEEP_V1: &str = "def stays():\n    return 1\n\n\ndef goes():\n    return 2\n";
const KEEP_V2: &str = "def stays():\n    return 10\n\n\ndef arrives():\n    return 4\n";
const KEEP_V4: &str = "def stays():\n    return 10\n\n\ndef middle():\n    return 6\n\n\ndef arrives():\n    return 4\n";
const OLD_PY: &str = "def legacy():\n    return 3\n";
const NEW_PY: &str = "def fresh():\n    return 5\n";

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("git must be available");
    assert!(status.success(), "git {args:?} failed");
}

struct Fixture {
    repo: PathBuf,
    base_graph: PathBuf,
    head_graph: PathBuf,
    /// Tag `v3`: `new.py` replaced by a symlink to `keep.py`.
    link_graph: PathBuf,
    /// Tag `v4`: `middle` added between `stays` and `arrives`.
    middle_graph: PathBuf,
    _root: common::TestTempDir,
}

/// The repository with tags `v1` to `v4`, and one graph per tag, each
/// indexed from a worktree checked out at that tag (repository-relative
/// paths, as `git diff` prints them).
fn fixture() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("snapshots_319_")
        .tempdir()
        .expect("create temp dir")
        .keep_managed();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Test"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("keep.py"), KEEP_V1).unwrap();
    std::fs::write(repo.join("old.py"), OLD_PY).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "v1"]);
    git(&repo, &["tag", "v1"]);
    std::fs::write(repo.join("keep.py"), KEEP_V2).unwrap();
    std::fs::remove_file(repo.join("old.py")).unwrap();
    std::fs::write(repo.join("new.py"), NEW_PY).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "v2"]);
    git(&repo, &["tag", "v2"]);
    std::fs::remove_file(repo.join("new.py")).unwrap();
    std::os::unix::fs::symlink("keep.py", repo.join("new.py")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "v3"]);
    git(&repo, &["tag", "v3"]);
    std::fs::write(repo.join("keep.py"), KEEP_V4).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "v4"]);
    git(&repo, &["tag", "v4"]);
    let mut graphs = Vec::new();
    for tag in ["v1", "v2", "v3", "v4"] {
        let tree = root.join(format!("tree-{tag}"));
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                tree.to_str().unwrap(),
                tag,
            ],
        );
        let graph = root.join(format!("graph-{tag}"));
        indexer::index_codebase(&tree, &graph).expect("index_codebase");
        graphs.push(graph);
    }
    Fixture {
        repo,
        middle_graph: graphs.pop().unwrap(),
        link_graph: graphs.pop().unwrap(),
        head_graph: graphs.pop().unwrap(),
        base_graph: graphs.pop().unwrap(),
        _root: root,
    }
}

fn by_name(symbols: &[ChangedSymbol]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = symbols
        .iter()
        .map(|s| (s.name.clone(), s.change_type.clone()))
        .collect();
    out.sort();
    out
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    expected
        .iter()
        .map(|(n, c)| (n.to_string(), c.to_string()))
        .collect()
}

#[test]
fn each_side_of_two_tags_is_read_against_its_own_graph() {
    let f = fixture();
    let head = GraphStore::open_or_create(&f.head_graph).unwrap();
    let base = GraphStore::open_or_create(&f.base_graph).unwrap();
    let cmp = compare_snapshots(
        &head,
        &base,
        &RefRange {
            codebase_path: &f.repo,
            base_ref: "v1",
            head_ref: "v2",
        },
    )
    .expect("compare");

    assert_eq!(
        by_name(&cmp.head.symbols_affected),
        pairs(&[
            ("arrives", "added"),
            ("fresh", "added"),
            ("stays", "modified")
        ]),
    );
    assert_eq!(
        by_name(&cmp.base.symbols_affected),
        pairs(&[
            ("goes", "deleted"),
            ("legacy", "deleted"),
            ("stays", "modified")
        ]),
    );
    assert_eq!(cmp.head.files_changed, 3);
    assert_eq!(cmp.base.files_changed, 3);
    // The deleted file is the base side's and the added file the head side's:
    // neither is a coverage gap of the other side.
    assert!(
        !cmp.head
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("old.py")),
        "{:?}",
        cmp.head.epistemic_reasons
    );
    assert!(
        !cmp.base
            .epistemic_reasons
            .iter()
            .any(|r| r.contains("new.py")),
        "{:?}",
        cmp.base.epistemic_reasons
    );
    assert_eq!(
        cmp.head.epistemic, "exact",
        "{:?}",
        cmp.head.epistemic_reasons
    );
    assert_eq!(
        cmp.base.epistemic, "exact",
        "{:?}",
        cmp.base.epistemic_reasons
    );
}

#[test]
fn the_head_graph_alone_cannot_see_the_deleted_file() {
    // The single-graph answer this issue starts from: the deleted file is a
    // coverage gap and no base symbol is named.
    let f = fixture();
    let head = GraphStore::open_or_create(&f.head_graph).unwrap();
    let alone = ai_architect_mcp::git_diff::analyze_git_diff(&head, &f.repo, "v1", "v2")
        .expect("analyze_git_diff");
    assert_eq!(alone.epistemic, "lower-bound");
    assert!(
        alone
            .epistemic_reasons
            .iter()
            .any(|r| r.starts_with("old.py")),
        "{:?}",
        alone.epistemic_reasons
    );
    assert!(!alone
        .symbols_affected
        .iter()
        .any(|s| s.name == "goes" || s.name == "legacy"));
}

fn call_detect_changes(arguments: Value) -> Value {
    let bin = std::env::var("AP_TEST_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_ai-architect-mcp-codebase").to_string());
    let mut child = Command::new(bin)
        .args(["--profile", "full"])
        .env_remove("AP_PROFILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the server");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "detect_changes", "arguments": arguments}});
    writeln!(stdin, "{request}").unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let _ = child.kill();
    let _ = child.wait();
    let envelope: Value = serde_json::from_str(&line).expect("a JSON-RPC response");
    assert_eq!(envelope["id"], 1, "{envelope}");
    let text = envelope["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no content text: {envelope}"));
    serde_json::from_str(text).expect("a JSON tool result")
}

#[test]
fn detect_changes_with_base_graph_path_over_stdio() {
    let f = fixture();
    let result = call_detect_changes(json!({
        "graph_path": f.head_graph,
        "base_graph_path": f.base_graph,
        "codebase_path": f.repo,
        "base_ref": "v1",
        "head_ref": "v2",
    }));
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["files_changed"], 3, "{result}");
    let base = &result["base_snapshot"];
    assert_eq!(base["graph_path"], json!(f.base_graph), "{result}");
    assert_eq!(base["symbols_affected_count"], 3, "{result}");
    assert_eq!(base["symbols_affected_truncated"], false, "{result}");
    let mut names: Vec<(String, String)> = base["symbols_affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["change_type"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        pairs(&[
            ("goes", "deleted"),
            ("legacy", "deleted"),
            ("stays", "modified")
        ])
    );
    assert_eq!(result["epistemic"], "exact", "{result}");

    // Without base_graph_path the response keeps its shape.
    let alone = call_detect_changes(json!({
        "graph_path": f.head_graph,
        "codebase_path": f.repo,
        "base_ref": "v1",
        "head_ref": "v2",
    }));
    assert_eq!(alone["status"], "ok", "{alone}");
    assert!(alone.get("base_snapshot").is_none(), "{alone}");
    assert_eq!(alone["epistemic"], "lower-bound", "{alone}");
}

#[test]
fn base_graph_path_is_refused_with_diff_text_or_without_a_repository() {
    let f = fixture();
    let with_text = call_detect_changes(json!({
        "graph_path": f.head_graph,
        "base_graph_path": f.base_graph,
        "diff_text": "diff --git a/keep.py b/keep.py\n",
    }));
    assert_eq!(with_text["status"], "error", "{with_text}");
    assert!(
        with_text["message"]
            .as_str()
            .unwrap()
            .contains("not diff_text"),
        "{with_text}"
    );
    let no_repo = call_detect_changes(json!({
        "graph_path": f.head_graph,
        "base_graph_path": f.base_graph,
    }));
    assert_eq!(no_repo["status"], "error", "{no_repo}");
    assert!(
        no_repo["message"]
            .as_str()
            .unwrap()
            .contains("requires codebase_path"),
        "{no_repo}"
    );
}

#[test]
fn a_file_replaced_by_a_symlink_keeps_its_removed_symbols() {
    // Codex review of PR #540: git prints a type change as one deleting and
    // one adding section for the same path; merging them before dropping the
    // deleting sections lost the whole file on both sides.
    let f = fixture();
    let head = GraphStore::open_or_create(&f.link_graph).unwrap();
    let base = GraphStore::open_or_create(&f.head_graph).unwrap();
    let cmp = compare_snapshots(
        &head,
        &base,
        &RefRange {
            codebase_path: &f.repo,
            base_ref: "v2",
            head_ref: "v3",
        },
    )
    .expect("compare");
    assert_eq!(cmp.head.files_changed, 1);
    assert_eq!(
        by_name(&cmp.base.symbols_affected),
        pairs(&[("fresh", "deleted")]),
        "{:?}",
        cmp.base.epistemic_reasons
    );
}

#[test]
fn a_function_added_between_two_others_is_no_base_side_gap() {
    // PR #540 review, note 1: in the reversed diff the addition is a pure
    // removal between two base symbols, which no base symbol holds.
    let f = fixture();
    let head = GraphStore::open_or_create(&f.middle_graph).unwrap();
    let base = GraphStore::open_or_create(&f.link_graph).unwrap();
    let cmp = compare_snapshots(
        &head,
        &base,
        &RefRange {
            codebase_path: &f.repo,
            base_ref: "v3",
            head_ref: "v4",
        },
    )
    .expect("compare");
    assert_eq!(
        by_name(&cmp.head.symbols_affected),
        pairs(&[("middle", "added")])
    );
    assert!(
        cmp.base.symbols_affected.is_empty(),
        "{:?}",
        cmp.base.symbols_affected
    );
    assert_eq!(
        cmp.base.epistemic, "exact",
        "{:?}",
        cmp.base.epistemic_reasons
    );
    assert_eq!(
        cmp.head.epistemic, "exact",
        "{:?}",
        cmp.head.epistemic_reasons
    );
}
