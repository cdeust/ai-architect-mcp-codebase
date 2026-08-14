// exclude_dirs_and_unreadable — issue #249 end-to-end gate (real stdio path).
//
// Two compounding gaps made the indexer unusable on a repo containing a
// locked-down secrets directory: no user-facing exclusion parameter, and a
// single `PermissionDenied` subdirectory aborting the WHOLE walk. This file
// spawns the real compiled binary and drives it over the actual JSON-RPC
// stdio wire (`tools/call` → `index_codebase`), not the library functions
// directly — the acceptance criteria are about the MCP contract, and a
// wire-level regression is a regression we ship (CONTRIBUTING.md's testing
// policy).
//
// `locked_directory_no_longer_aborts_the_index_issue_249` is THE regression
// test for issue #249, part 2: pre-fix, `walk_dir_recursive`
// (`src/indexer/walk.rs`) propagated ANY `read_dir` error — including
// `PermissionDenied` — as a hard `Err`, so `do_index_codebase` returned
// `status: "error"` and every file already collected was discarded. Post-fix,
// the locked directory is recorded as `unreadable` in the coverage report and
// the index completes with `status: "ok"`.
//
// `exclude_dirs_prunes_bare_name_and_relative_path_no_file_nodes_persisted`
// covers issue #249 part 1: the new `exclude_dirs` contract on both
// `index_codebase` and the coverage receipt.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

/// A live MCP server subprocess, driven over its real JSON-RPC stdio wire —
/// no library-function shortcuts. Killed on drop so a panicking assertion
/// never leaks a hung process.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(BIN)
            .args(["--profile", "full"])
            .env_remove("AP_PROFILE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ai-architect-mcp-codebase");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    /// Sends one JSON-RPC request line and reads back exactly one response
    /// line (the server's stdio loop is one-request-per-line, source:
    /// `src/main.rs`'s `for line in handle.lines()` dispatch).
    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{req}").expect("write request line");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        assert!(
            !line.is_empty(),
            "server closed stdout before responding to {method}"
        );
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response line is not valid JSON ({e}): {line:?}"))
    }

    /// Calls an MCP tool and unwraps the double-encoded result: the
    /// JSON-RPC envelope's `result.content[0].text` is itself a JSON string
    /// (the tool's actual response), per `handle_tool_call` in `main.rs`.
    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool response has no content[0].text: {resp:?}"));
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("tool response text is not valid JSON ({e}): {text}"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every File-node id in the graph the index run persisted at `out/graph` —
/// the index completed iff this opens and queries cleanly.
fn graph_file_ids(out: &std::path::Path) -> Vec<String> {
    let store = ai_architect_mcp::graph_store::GraphStore::open_or_create(&out.join("graph"))
        .expect("open graph");
    let qr = store
        .execute_query("MATCH (f:File) RETURN f.id")
        .expect("query File nodes");
    qr.rows
        .into_iter()
        .filter_map(|r| r.into_iter().next())
        .collect()
}

/// The coverage receipt's skipped entries carrying `reason`, as a path set —
/// pruned/unreadable directories must be named, never silently dropped.
fn skipped_paths_with_reason(resp: &Value, reason: &str) -> std::collections::BTreeSet<String> {
    resp["coverage"]["skipped"]["files"]
        .as_array()
        .expect("skipped files list")
        .iter()
        .filter(|f| f["reason"] == json!(reason))
        .filter_map(|f| f["path"].as_str().map(str::to_string))
        .collect()
}

/// Fixture for the exclusion test: `app.rs` at the root; `secrets/` (the
/// bare-name target), `config/tokens/` (the relative-path target), and
/// `other/tokens/` — the same bare name at a DIFFERENT path, which must
/// survive the relative-path exclusion.
fn build_exclude_repo(tmp: &std::path::Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join("secrets")).expect("mk secrets");
    std::fs::create_dir_all(repo.join("config/tokens")).expect("mk config/tokens");
    std::fs::create_dir_all(repo.join("other/tokens")).expect("mk other/tokens");
    std::fs::write(repo.join("app.rs"), "fn main() {}\n").expect("write app.rs");
    std::fs::write(repo.join("secrets/key.rs"), "fn key() {}\n").expect("write key.rs");
    std::fs::write(repo.join("config/tokens/tok.rs"), "fn tok() {}\n").expect("write tok.rs");
    std::fs::write(repo.join("other/tokens/kept.rs"), "fn kept() {}\n").expect("write kept.rs");
    repo
}

#[test]
fn exclude_dirs_prunes_bare_name_and_relative_path_no_file_nodes_persisted() {
    let tmp = tempfile::Builder::new()
        .prefix("exclude_dirs_it_")
        .tempdir()
        .expect("temp dir");
    let repo = build_exclude_repo(tmp.path());
    let out = tmp.path().join("out");

    let mut server = Server::spawn();
    let resp = server.call_tool(
        1,
        "index_codebase",
        json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "exclude_dirs": ["secrets", "config/tokens"],
            "cochange": false,
        }),
    );

    assert_eq!(resp["status"], json!("ok"), "index must succeed: {resp:?}");
    // The receipt carries a distinct, exact count for user-excluded dirs.
    assert_eq!(
        resp["coverage"]["skipped"]["user_excluded_count"],
        json!(2),
        "exactly 2 directories were excluded: {resp:?}"
    );
    assert_eq!(
        skipped_paths_with_reason(&resp, "user_excluded"),
        std::collections::BTreeSet::from(["secrets".to_string(), "config/tokens".to_string()]),
        "coverage must name exactly the two excluded dirs, not silently drop them"
    );

    // No File node exists anywhere under either excluded subtree — pruned at
    // the walk, never reaching persist.
    let ids = graph_file_ids(&out);
    assert!(
        ids.iter().any(|id| id == "app.rs"),
        "app.rs must still be indexed: {ids:?}"
    );
    assert!(
        ids.iter().any(|id| id == "other/tokens/kept.rs"),
        "the same-named but non-excluded tokens/ dir must still be indexed: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("secrets/")),
        "no File node under the excluded bare-name dir: {ids:?}"
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("config/tokens/")),
        "no File node under the excluded relative-path dir: {ids:?}"
    );
}

/// Restores 0o755 on drop so the tempdir's cleanup (which must traverse and
/// delete `locked/`) never fails — including on a panicking assertion.
#[cfg(unix)]
struct RestorePerms(std::path::PathBuf);

#[cfg(unix)]
impl Drop for RestorePerms {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Fixture for the permission-denied test: `app.rs` at the root plus a
/// `locked/` subdirectory (containing one source file) chmod'd to 000.
/// Returns the repo root and the locked directory's path.
#[cfg(unix)]
fn build_locked_repo(tmp: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let repo = tmp.join("repo");
    std::fs::create_dir_all(repo.join("locked")).expect("mk locked");
    std::fs::write(repo.join("app.rs"), "fn main() {}\n").expect("write app.rs");
    std::fs::write(repo.join("locked/secret.rs"), "fn secret() {}\n").expect("write secret.rs");
    let locked_dir = repo.join("locked");
    std::fs::set_permissions(&locked_dir, std::fs::Permissions::from_mode(0o000))
        .expect("chmod 000");
    (repo, locked_dir)
}

/// THE regression test for issue #249 (permission-denied degrade). Gated
/// `#[cfg(unix)]` — the chmod(2) mode-bits mechanism this test exercises is
/// POSIX-specific. Pre-fix: `walk_dir_recursive` propagates the EACCES as a
/// hard `Err`, `do_index_codebase` returns it, and `run_index_codebase` maps
/// it to `status: "error"` — the first assertion below fails on pre-fix code.
#[test]
#[cfg(unix)]
fn locked_directory_no_longer_aborts_the_index_issue_249() {
    let tmp = tempfile::Builder::new()
        .prefix("issue249_chmod000_")
        .tempdir()
        .expect("temp dir");
    let (repo, locked_dir) = build_locked_repo(tmp.path());
    let _restore = RestorePerms(locked_dir.clone());

    // Precondition guard: root (or a filesystem that ignores the mode bit)
    // makes read_dir succeed regardless — skip with an explanatory eprintln
    // rather than asserting a false failure in that environment.
    if std::fs::read_dir(&locked_dir).is_ok() {
        eprintln!(
            "locked_directory_no_longer_aborts_the_index_issue_249: read_dir on a \
             chmod-000 dir unexpectedly succeeded (running as root?) — skipping"
        );
        return;
    }

    let out = tmp.path().join("out");
    let mut server = Server::spawn();
    let resp = server.call_tool(
        1,
        "index_codebase",
        json!({
            "path": repo.to_string_lossy(),
            "output_dir": out.to_string_lossy(),
            "cochange": false,
        }),
    );

    assert_eq!(
        resp["status"],
        json!("ok"),
        "a locked-down subdirectory must not abort the whole index: {resp:?}"
    );
    assert!(
        resp["files_indexed"].as_u64().unwrap_or(0) >= 1,
        "app.rs must still be indexed despite the locked sibling dir: {resp:?}"
    );
    assert!(
        skipped_paths_with_reason(&resp, "unreadable").contains("locked"),
        "the locked directory must be reported in coverage with reason 'unreadable': {resp:?}"
    );
    // The graph itself must exist, be openable, and carry app.rs — the index
    // completed despite the unreadable sibling.
    assert!(
        graph_file_ids(&out).iter().any(|id| id == "app.rs"),
        "app.rs must have a File node in the completed graph"
    );
}
