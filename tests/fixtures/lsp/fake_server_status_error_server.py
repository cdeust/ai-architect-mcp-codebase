#!/usr/bin/env python3
"""Fake LSP server used by
src/lsp_client_tests.rs::initialize_with_probe_reports_server_health_error and
src/lsp_resolver/health_gate_tests.rs.

Copy of fake_server_status_server.py's shape, but sends `health: "error"` on
its FINAL `experimental/serverStatus` message instead of `"ok"` — the exact
signal sonde B (issue #282 investigation) observed from rust-analyzer when
the analyzed crate sits under a parent Cargo workspace that does not list it
as a member: `{health:"error", quiescent:true, message:"Failed to load
workspaces."}`. `quiescent: false` is sent first (health "warning", per
sonde D's observed warning-then-error order) so a test can also confirm the
LAST health observed wins, not the first.

Also logs `definition_requested` if it ever receives a
`textDocument/definition` request — the health-gate fix this fixture backs
must stop the resolver BEFORE any such request is issued, so a passing test
that never sees this line in the log is direct proof of that, not an
inference from absence of resolved edges.

Usage: fake_server_status_error_server.py <log_path>
"""
import json
import sys


def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().rstrip("\r\n")
        if line == "":
            break
        key, _, value = line.partition(":")
        headers[key.strip().lower()] = value.strip()
    length = int(headers["content-length"])
    body = sys.stdin.buffer.read(length)
    return json.loads(body)


def write_message(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


def log(path, line):
    with open(path, "a") as f:
        f.write(line + "\n")
        f.flush()


def do_handshake(log_path):
    """Steps 1-2: initialize -> respond, then read the initialized notif."""
    req = read_message()
    log(log_path, "initialize_answered")
    write_message(
        {
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {"capabilities": {"definitionProvider": True}},
        }
    )
    notif = read_message()
    log(log_path, f"received:{notif.get('method')}")


def send_status(log_path, tag, status):
    """`status` is (health, quiescent, message) — a parameter object so this
    stays at 3 arguments (max 4)."""
    health, quiescent, message = status
    log(log_path, tag)
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": {"health": health, "quiescent": quiescent, "message": message},
        }
    )


def drain_until_exit(log_path):
    """Keep reading so a caller that (incorrectly) issues a
    textDocument/definition request anyway is observed, and so
    `shutdown`'s request/notification pair does not hang this process
    waiting on a read that never comes."""
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        if method == "textDocument/definition":
            log(log_path, "definition_requested")
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "shutdown":
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "exit":
            return


def main():
    log_path = sys.argv[1]
    do_handshake(log_path)
    # Warning first (sonde D order): activity, not yet the final verdict.
    send_status(
        log_path, "warning_sent", ("warning", False, "Failed to discover workspace")
    )
    # Error + quiescent:true — sonde B's exact shape. The client's readiness
    # wait must treat this as the deterministic end signal (the fixture never
    # sends a single workDoneProgress message), while the health it carries
    # must survive to the caller as `Error`.
    send_status(
        log_path, "error_sent", ("error", True, "Failed to load workspaces.")
    )
    drain_until_exit(log_path)


if __name__ == "__main__":
    main()
