// second_call — `analyze_codebase` called again on an unchanged tree (issue #443).
//
// The issue asks that a second call on an unchanged repository be at least 5
// times faster than the first. A wall-clock ratio is not a correctness gate
// here (issue #74, MANIFEST.md), so it is measured by this binary and recorded
// with its hardware instead.
//
// The binary drives the server itself over stdio, as a client does: it starts
// the given server binary, sends `initialize`, then calls `analyze_codebase`
// `CALLS` times with the same `path` and `output_dir`. The first call builds
// the graph (`index.mode` = "full"); the next ones find nothing changed and
// refresh it (`index.mode` = "incremental"). Each round starts from an empty
// `output_dir`; the minimum of `ROUNDS` is kept per call. The corpus is only
// read.
//
// Usage: `cargo run -p incremental-speed-bench --release --bin second_call --
// <server_binary> <corpus_dir>`. Build the server first (`cargo build
// --release --bin ai-architect-mcp-codebase`). Prints one JSON object.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

/// One full build, then two calls on the unchanged tree: the second shows
/// whether the first refresh left anything the next one redoes.
const CALLS: usize = 3;
/// Rounds per corpus, each from an empty `output_dir`; the minimum is kept.
const ROUNDS: usize = 3;

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(binary: &Path) -> Server {
        let mut child = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the server binary");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Server {
            child,
            stdin,
            stdout,
        };
        server.request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "second_call", "version": "0"},
            }),
        );
        server.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        server
    }

    fn send(&mut self, msg: &Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and returns its response, skipping notifications.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line).unwrap();
            assert!(n > 0, "the server closed its output");
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["id"] == id {
                return msg;
            }
        }
    }

    /// Calls `analyze_codebase` and returns the tool's JSON body.
    fn analyze(&mut self, id: u64, corpus: &Path, out: &Path) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({
                "name": "analyze_codebase",
                "arguments": {"path": corpus, "output_dir": out},
            }),
        );
        assert!(resp["error"].is_null(), "analyze_codebase failed: {resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        let body: Value = serde_json::from_str(text).unwrap();
        assert!(
            resp["result"]["isError"] != true && body["status"] == "ok",
            "analyze_codebase failed: {body}"
        );
        body
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One call's figures: what the server measured and what the client waited.
#[derive(Clone)]
struct Call {
    mode: String,
    wall_ms: u64,
    total_ms: u64,
    index_ms: Option<u64>,
    search_ms: u64,
    files: u64,
}

fn read_call(body: &Value, wall_ms: u64) -> Call {
    let index = &body["index"];
    let mode = index["mode"].as_str().unwrap().to_owned();
    let files = if mode == "full" {
        index["files_indexed"].as_u64().unwrap()
    } else {
        index["unchanged"].as_u64().unwrap()
    };
    Call {
        mode,
        wall_ms,
        total_ms: body["total_elapsed_ms"].as_u64().unwrap(),
        // The full path reports no index time of its own.
        index_ms: index["elapsed_ms"].as_u64(),
        search_ms: body["search_index"]["elapsed_ms"].as_u64().unwrap(),
        files,
    }
}

fn round(binary: &Path, corpus: &Path, out: &Path) -> Vec<Call> {
    let mut server = Server::start(binary);
    (0..CALLS)
        .map(|i| {
            let t = Instant::now();
            let body = server.analyze(i as u64 + 2, corpus, out);
            read_call(&body, t.elapsed().as_millis() as u64)
        })
        .collect()
}

/// Runs `ROUNDS` rounds and keeps, per call, the round with the least total.
fn best_of_rounds(binary: &Path, corpus: &Path, tmp: &Path) -> Vec<Call> {
    let mut best: Vec<Call> = Vec::new();
    for r in 0..ROUNDS {
        let calls = round(binary, corpus, &tmp.join(format!("round{r}")));
        assert_eq!(calls[0].mode, "full", "the first call must build the graph");
        for c in &calls[1..] {
            assert_eq!(c.mode, "incremental", "an unchanged tree must be refreshed");
        }
        for (i, c) in calls.iter().enumerate() {
            eprintln!(
                "round {r} call {}: {} {} ms (index {:?} ms, search {} ms)",
                i + 1,
                c.mode,
                c.total_ms,
                c.index_ms,
                c.search_ms
            );
            match best.get_mut(i) {
                Some(b) if b.total_ms <= c.total_ms => {}
                Some(b) => *b = c.clone(),
                None => best.push(c.clone()),
            }
        }
    }
    best
}

fn report(corpus: &Path, best: &[Call]) -> Value {
    let first = best[0].total_ms as f64;
    let rows: Vec<Value> = best
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({
                "call": i + 1,
                "mode": c.mode,
                "files": c.files,
                "total_ms": c.total_ms,
                "wall_ms": c.wall_ms,
                "index_ms": c.index_ms,
                "search_ms": c.search_ms,
                // What the server measured in total, less the search phase and,
                // on a refresh, the index phase: resolve and cluster on a
                // refresh; the index as well on the full call.
                "rest_ms": c.total_ms
                    .saturating_sub(c.index_ms.unwrap_or(0))
                    .saturating_sub(c.search_ms),
                "speedup_vs_first": format!("{:.2}", first / c.total_ms as f64),
            })
        })
        .collect();
    json!({
        "corpus": corpus.file_name().map(|n| n.to_string_lossy().into_owned()),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "rounds_min_of": ROUNDS,
        "calls": rows,
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [binary, corpus] = args.as_slice() else {
        eprintln!("usage: second_call <server_binary> <corpus_dir>");
        std::process::exit(2);
    };
    let binary = PathBuf::from(binary);
    let corpus = std::fs::canonicalize(corpus).expect("corpus directory");
    let tmp = tempfile::Builder::new()
        .prefix("second_call_")
        .tempdir()
        .unwrap();
    let best = best_of_rounds(&binary, &corpus, tmp.path());
    let report = report(&corpus, &best);
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
