// Issue #447: a symbol that lives in a file no parser handles (PHP, C#, Lua)
// is not in the graph, and `search_codebase` answered "ok, 0 results" for it,
// which reads as absence. An answer with zero results now carries the
// coverage record's `no_extraction` tally and says how many files it covers.
// Driven over the real stdio wire, from `analyze_codebase` to the search.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_ai-architect-mcp-codebase");

/// A live MCP server subprocess on its JSON-RPC stdio wire, killed on drop.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
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
            next_id: 1,
        }
    }

    /// Calls a tool and decodes `result.content[0].text`, itself JSON.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        });
        writeln!(self.stdin, "{req}").expect("write request line");
        self.stdin.flush().expect("flush stdin");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        let resp: Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("response is not JSON ({e}): {line:?}"));
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no content[0].text: {resp}"));
        serde_json::from_str(text).unwrap_or_else(|e| panic!("tool text is not JSON ({e}): {text}"))
    }

    fn analyze(&mut self, repo: &Path, out: &Path, language: Option<&str>) -> Value {
        let mut args = json!({"path": repo, "output_dir": out, "dependency_scope": "none"});
        if let Some(language) = language {
            args["language"] = json!(language);
        }
        let response = self.call_tool("analyze_codebase", args);
        assert_eq!(response["status"], "ok", "{response}");
        response
    }

    fn search(&mut self, out: &Path, query: &str) -> Value {
        let response = self.call_tool(
            "search_codebase",
            json!({"graph_path": out.join("graph"), "query": query}),
        );
        assert_eq!(response["status"], "ok", "{response}");
        response
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One Rust file the indexer parses, and three it cannot: each declares a
/// symbol whose name appears nowhere else.
fn write_fixture(repo: &Path) {
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/lib.rs"),
        "pub fn indexed_entry_point() -> u32 {\n    7\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/billing.php"),
        "<?php\nfunction legacyInvoiceTotal($rows) {\n    return count($rows);\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/Ledger.cs"),
        "public class QuarterlyLedgerExporter {\n    public void Export() {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        repo.join("src/hooks.lua"),
        "local function reload_tariff_table()\n  return {}\nend\n",
    )
    .unwrap();
}

#[test]
fn an_empty_search_names_the_files_without_extraction() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, out) = (tmp.path().join("repo"), tmp.path().join("out"));
    write_fixture(&repo);
    let mut server = Server::spawn();
    let analyzed = server.analyze(&repo, &out, None);
    let recorded = &analyzed["coverage"]["no_extraction"];
    assert_eq!(recorded["count"], 3, "{analyzed}");

    // The symbol exists, in a PHP file: the graph cannot know it.
    let miss = server.search(&out, "legacyInvoiceTotal");
    assert_eq!(miss["total_count"], 0, "{miss}");
    let block = &miss["no_extraction"];
    assert_eq!(block["status"], "recorded", "{miss}");
    assert_eq!(block["count"], 3, "{miss}");
    assert_eq!(
        block["by_extension"],
        json!({"cs": 1, "lua": 1, "php": 1}),
        "{miss}"
    );
    assert_eq!(block["extensions_omitted"], 0, "{miss}");
    let note = block["note"].as_str().expect("a note on an empty search");
    assert!(
        note.starts_with("no extraction for 3 files in this repository"),
        "{note}"
    );
    // Same tally as the coverage report, apart from the search's own note.
    for key in ["status", "count", "by_extension", "extensions_omitted"] {
        assert_eq!(block[key], recorded[key], "{key}: {miss}");
    }

    // A search that finds something is unchanged: the block is not added.
    let hit = server.search(&out, "indexed_entry_point");
    assert!(hit["total_count"].as_u64().unwrap() > 0, "{hit}");
    assert!(hit.get("no_extraction").is_none(), "{hit}");
}

#[test]
fn an_empty_search_without_a_tally_says_not_recorded_never_zero() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let (repo, out) = (tmp.path().join("repo"), tmp.path().join("out"));
    write_fixture(&repo);
    let mut server = Server::spawn();

    // A `language`-filtered index walks only that language's files, so its
    // record has no tally: the search must not report "0 files".
    server.analyze(&repo, &out, Some("rust"));
    let filtered = server.search(&out, "legacyInvoiceTotal");
    assert_eq!(filtered["total_count"], 0, "{filtered}");
    let block = &filtered["no_extraction"];
    assert_eq!(block["status"], "not_recorded", "{filtered}");
    assert!(block.get("count").is_none(), "{filtered}");

    // No coverage record at all next to the graph: same status, own reason.
    std::fs::remove_file(out.join("index_coverage.json")).expect("drop the sidecar");
    let bare = server.search(&out, "legacyInvoiceTotal");
    assert_eq!(bare["total_count"], 0, "{bare}");
    assert_eq!(bare["no_extraction"]["status"], "not_recorded", "{bare}");
    assert!(bare["no_extraction"].get("count").is_none(), "{bare}");
    let detail = bare["no_extraction"]["detail"].as_str().expect("a reason");
    assert!(detail.contains("no coverage record"), "{detail}");
}
