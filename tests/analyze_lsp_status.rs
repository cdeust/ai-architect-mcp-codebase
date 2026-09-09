//! Regression: requested LSP failure must not look like a disabled phase.
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

fn analyze(repo: &Path, output: &Path, lsp: bool, path: Option<&Path>) -> Value {
    let mut command = Command::new(BIN);
    command
        .args(["--profile", "full"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().unwrap();
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
    "params": {"name": "analyze_codebase", "arguments": {
        "path": repo, "output_dir": output, "language": "rust", "lsp": lsp
    }}});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let envelope: Value = serde_json::from_slice(&result.stdout).unwrap();
    serde_json::from_str(envelope["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

/// Issue #283 lot 4: `self.response_of()` inside `impl TaskSet` is now
/// bound by the static resolver itself (`resolver::receiver`,
/// `Evidence::ReceiverBound`), which would leave nothing for
/// `successful_requested_lsp_retains_counts_and_reports_completion`'s LSP
/// pass to resolve. `total` is a free function calling through an
/// index-expression receiver (`sets[0].response_of()`,
/// `ReceiverForm::None`) so the static resolver still cannot bind it and
/// the LSP-path assertions below keep exercising the LSP pass, not the
/// static one (tasks/plan-issues-282-283-284.md §2.3).
fn fixture(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"lsp_status_fixture\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(repo.join("src/lib.rs"),
        "pub struct TaskSet;\nimpl TaskSet {\n pub fn response_of(&self) -> u64 { 1 }\n}\npub fn total(sets: &[TaskSet]) -> u64 { sets[0].response_of() }\n").unwrap();
    repo
}

#[test]
fn missing_requested_lsp_is_reported_while_static_analysis_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(tmp.path());
    let empty_path = tmp.path().join("no-programs");
    std::fs::create_dir(&empty_path).unwrap();
    let out = tmp.path().join("out");
    let result = analyze(&repo, &out, true, Some(&empty_path));
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["lsp_status"]["state"], "failed", "{result}");
    assert_eq!(result["lsp_status"]["requested"], true);
    assert_eq!(result["lsp_status"]["fallback"], "available_graph");
    assert!(result["lsp_status"]["note"]
        .as_str()
        .unwrap()
        .contains("partial LSP"));
    assert!(result["lsp_status"]["error"]
        .as_str()
        .unwrap()
        .contains("lsp_not_found"));
    assert!(result["lsp_resolve"].is_null());
    assert_eq!(result["resolve"]["phase"], "static");
    assert!(out.join("graph").exists());
}

#[test]
fn disabled_lsp_is_distinct_from_a_failed_request() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(tmp.path());
    let result = analyze(&repo, &tmp.path().join("out"), false, Some(tmp.path()));
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(
        result["lsp_status"],
        json!({"requested": false, "state": "disabled"})
    );
    assert!(result["lsp_resolve"].is_null());
}

#[test]
#[ignore = "requires rust-analyzer; run explicitly with --ignored"]
fn successful_requested_lsp_retains_counts_and_reports_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture(tmp.path());
    let result = analyze(&repo, &tmp.path().join("out"), true, None);
    assert_eq!(result["status"], "ok", "{result}");
    // Issue #282: `lsp_status` now also carries `server_health` — adapted
    // from the previous strict-equality assertion, which pinned the exact
    // wire shape before that field existed.
    assert_eq!(result["lsp_status"]["requested"], true, "{result}");
    assert_eq!(result["lsp_status"]["state"], "completed", "{result}");
    assert_eq!(
        result["lsp_status"]["server_health"]["health"], "ok",
        "{result}"
    );
    assert!(
        result["lsp_resolve"]["resolved_count"].as_u64().unwrap() > 0,
        "{result}"
    );
    assert_eq!(result["resolve"]["phase"], "static");
}

/// Issue #282, the measured symptom: a crate that sits under a parent Cargo
/// workspace which does not list it as a member. `cargo metadata` refuses
/// the target ("current package believes it's in a workspace when it's
/// not"), rust-analyzer never loads a crate graph, and every
/// `textDocument/definition` request would answer `[]` — indistinguishable,
/// pre-fix, from 634 individually "not found" call sites. The phase must now
/// fail loudly instead of reporting `state: "completed"`.
#[test]
#[ignore = "requires rust-analyzer; run explicitly with --ignored"]
fn nested_under_a_parent_workspace_fails_the_lsp_phase_loudly() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\nresolver = \"2\"\n",
    )
    .unwrap();
    let repo = fixture(tmp.path());
    let out = tmp.path().join("out");
    let result = analyze(&repo, &out, true, None);
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["lsp_status"]["requested"], true, "{result}");
    assert_eq!(result["lsp_status"]["state"], "failed", "{result}");
    assert!(
        result["lsp_status"]["error"]
            .as_str()
            .unwrap()
            .starts_with("lsp_workspace_load_failed"),
        "{result}"
    );
    assert!(result["lsp_resolve"].is_null(), "{result}");
    assert_eq!(result["resolve"]["phase"], "static");
    assert!(out.join("graph").exists());
}
