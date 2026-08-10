// installer — issue #59 end-to-end gate.
//
// Runs the REAL compiled binary as a subprocess with a temp $HOME + fixture host
// configs, and asserts exact outcomes: never-clobber (other servers survive),
// JSONC refusal (Zed comments preserved), idempotence, uninstall round-trip, hook
// registration, and the fail-open hook-augment contract (silent exit 0 on error).

use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Paths to both shipped binaries (Cargo provides these for integration tests).
const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");
const LEGACY_BIN: &str = env!("CARGO_BIN_EXE_automatised-pipeline");

fn run(home: &Path, args: &[&str]) -> (String, i32) {
    let out = Command::new(BIN)
        .env("HOME", home)
        // Isolate from the developer's real env vars.
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .args(args)
        .output()
        .expect("run binary");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn write(home: &Path, rel: &str, content: &str) {
    let p = home.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

fn read(home: &Path, rel: &str) -> String {
    std::fs::read_to_string(home.join(rel)).unwrap()
}

#[test]
fn install_never_clobbers_other_servers_and_is_idempotent() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_")
        .tempdir()
        .unwrap();
    let home = tmp.path();

    // Cursor with a pre-existing OTHER server that must survive.
    write(
        home,
        ".cursor/mcp.json",
        r#"{"mcpServers":{"other":{"command":"/bin/other","args":["--flag"]}}}"#,
    );

    let (out, code) = run(home, &["install", "--only", "cursor"]);
    assert_eq!(code, 0, "install exits 0: {out}");
    assert!(
        out.contains("Cursor: configured"),
        "cursor configured: {out}"
    );

    let v: Value = serde_json::from_str(&read(home, ".cursor/mcp.json")).unwrap();
    // The other server survived, deep-equal.
    assert_eq!(
        v["mcpServers"]["other"],
        json!({"command":"/bin/other","args":["--flag"]}),
        "the pre-existing 'other' server must survive untouched"
    );
    // Ours was added.
    assert_eq!(v["mcpServers"]["ai-architect"]["command"], json!(BIN));

    // Idempotence: a second install reports no change and leaves the file identical.
    let before = read(home, ".cursor/mcp.json");
    let (out2, _) = run(home, &["install", "--only", "cursor"]);
    assert!(
        out2.contains("already configured"),
        "second run is a no-op: {out2}"
    );
    assert_eq!(
        read(home, ".cursor/mcp.json"),
        before,
        "file unchanged on re-run"
    );
}

#[test]
fn legacy_executable_forwards_to_the_canonical_server() {
    let mut child = Command::new(LEGACY_BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run compatibility executable");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["serverInfo"]["name"], "ai-architect");
    assert_eq!(
        response["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn zed_jsonc_with_comments_is_refused_and_left_byte_for_byte() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_zed_")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    let jsonc = "{\n  // my Zed settings — keep these comments!\n  \"theme\": \"One Dark\",\n  \"context_servers\": {}\n}\n";
    write(home, ".config/zed/settings.json", jsonc);

    let (out, code) = run(home, &["install", "--only", "zed"]);
    assert_eq!(code, 0);
    assert!(
        out.contains("MANUAL STEP"),
        "Zed JSONC must be refused, not clobbered: {out}"
    );
    assert!(out.contains("context_servers"), "guidance names the key");
    // The file is untouched — byte for byte.
    assert_eq!(
        read(home, ".config/zed/settings.json"),
        jsonc,
        "Zed JSONC with comments must be preserved byte-for-byte"
    );
}

#[test]
fn codex_toml_preserves_comments_and_other_servers() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_codex_")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    write(
        home,
        ".codex/config.toml",
        "# keep me\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"/bin/other\"\nargs = []\n",
    );
    let (out, _) = run(home, &["install", "--only", "codex"]);
    assert!(out.contains("Codex CLI: configured"), "{out}");
    let content = read(home, ".codex/config.toml");
    assert!(content.contains("# keep me"), "comment preserved");
    assert!(
        content.contains("[mcp_servers.other]"),
        "other server preserved"
    );
    assert!(content.contains("[mcp_servers.ai-architect]"), "ours added");
}

#[test]
fn uninstall_removes_only_our_entry() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_uninst_")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    write(
        home,
        ".cursor/mcp.json",
        r#"{"mcpServers":{"other":{"command":"/bin/other","args":[]}}}"#,
    );
    run(home, &["install", "--only", "cursor"]);
    let (out, code) = run(home, &["uninstall", "--only", "cursor"]);
    assert_eq!(code, 0, "{out}");
    let v: Value = serde_json::from_str(&read(home, ".cursor/mcp.json")).unwrap();
    assert!(v["mcpServers"]["ai-architect"].is_null(), "ours removed");
    assert_eq!(
        v["mcpServers"]["other"]["command"],
        json!("/bin/other"),
        "other kept"
    );
}

#[test]
fn with_hooks_registers_pretooluse_and_preserves_existing_hooks() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_hooks_")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    write(home, ".cursor/mcp.json", "{}");
    // Pre-existing unrelated hook must survive.
    write(
        home,
        ".claude/settings.json",
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"/x/bash-guard"}]}]}}"#,
    );

    let (out, _) = run(home, &["install", "--only", "cursor", "--with-hooks"]);
    assert!(out.contains("PreToolUse hook: configured"), "{out}");
    let v: Value = serde_json::from_str(&read(home, ".claude/settings.json")).unwrap();
    let arr = v["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(
        arr.iter().any(|b| b["matcher"] == "Bash"),
        "existing hook survives"
    );
    assert!(
        arr.iter().any(|b| b["matcher"] == "Grep|Glob"
            && b["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook-augment")),
        "our Grep/Glob hook is registered: {v}"
    );
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::Builder::new()
        .prefix("installer_dry_")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    write(home, ".cursor/mcp.json", "{}");
    let (out, _) = run(home, &["install", "--only", "cursor", "--dry-run"]);
    assert!(out.contains("[dry-run]"), "{out}");
    assert!(out.contains("would write"), "{out}");
    assert_eq!(
        read(home, ".cursor/mcp.json"),
        "{}",
        "dry-run must not touch the file"
    );
}

// -- hook-augment fail-open contract ----------------------------------------

fn hook_augment(home: &Path, payload: &str) -> (String, i32) {
    let mut child = Command::new(BIN)
        .env("HOME", home)
        .arg("hook-augment")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn hook_augment_emits_suggestion_when_graph_present() {
    let tmp = tempfile::Builder::new()
        .prefix("hook_yes_")
        .tempdir()
        .unwrap();
    let proj = tmp.path().join("proj");
    // The graph marker: the committed artifact dir under the project cwd.
    std::fs::create_dir_all(proj.join(".ai-architect-mcp-codebase")).unwrap();
    let payload = json!({
        "tool_name": "Grep",
        "cwd": proj.to_string_lossy(),
        "tool_input": { "pattern": "handle_tool" }
    })
    .to_string();
    let (out, code) = hook_augment(tmp.path(), &payload);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(out.trim()).expect("emits JSON");
    assert!(v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("search_codebase"));
}

#[test]
fn hook_augment_is_silent_without_a_graph() {
    let tmp = tempfile::Builder::new()
        .prefix("hook_no_")
        .tempdir()
        .unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap(); // no .ai-architect-mcp-codebase marker
    let payload =
        json!({"tool_name":"Grep","cwd":proj.to_string_lossy(),"tool_input":{"pattern":"x"}})
            .to_string();
    let (out, code) = hook_augment(tmp.path(), &payload);
    assert_eq!(code, 0, "must never block");
    assert!(out.trim().is_empty(), "no graph → silent, got: {out:?}");
}

#[test]
fn hook_augment_migrates_and_recognizes_a_pre_rename_marker_dir() {
    // Issue #195: a repo indexed before the rename carries the graph under
    // the old directory name. The real compiled binary's hook-augment path
    // must self-heal it (via `artifact::migrate_legacy_dir`) on first touch,
    // not just recognize the new name.
    let tmp = tempfile::Builder::new()
        .prefix("hook_legacy_")
        .tempdir()
        .unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(proj.join(".automatised-pipeline")).unwrap();
    let payload = json!({
        "tool_name": "Grep",
        "cwd": proj.to_string_lossy(),
        "tool_input": { "pattern": "handle_tool" }
    })
    .to_string();
    let (out, code) = hook_augment(tmp.path(), &payload);
    assert_eq!(code, 0);
    let v: Value = serde_json::from_str(out.trim()).expect("emits JSON — legacy dir recognized");
    assert!(v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("search_codebase"));
    assert!(
        proj.join(".ai-architect-mcp-codebase").is_dir(),
        "legacy dir must be renamed in place, not merely tolerated"
    );
    assert!(
        !proj.join(".automatised-pipeline").exists(),
        "legacy dir must not be left behind after migration"
    );
}

#[test]
fn hook_augment_fails_open_on_malformed_stdin() {
    // The cardinal-rule test: garbage in → exit 0, print nothing, NEVER block.
    let tmp = tempfile::Builder::new()
        .prefix("hook_bad_")
        .tempdir()
        .unwrap();
    for bad in ["{ not json at all", "", "null", "[1,2,3]"] {
        let (out, code) = hook_augment(tmp.path(), bad);
        assert_eq!(code, 0, "malformed stdin {bad:?} must still exit 0");
        assert!(
            out.trim().is_empty(),
            "malformed stdin must be silent, got: {out:?}"
        );
    }
}
