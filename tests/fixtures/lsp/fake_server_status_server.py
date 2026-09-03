#!/usr/bin/env python3
"""Fake LSP server used by
src/lsp_client_tests.rs::lsp_client_resolves_via_server_status_quiescent_not_progress.

Speaks just enough of LSP 3.17 Base Protocol + rust-analyzer's
`experimental/serverStatus` extension over Content-Length-framed stdio to
prove `LspClient::initialize_with_probe` honours the DETERMINISTIC
`quiescent: true` signal — not the workDoneProgress fallback, which this
script never sends a single message of. `quiescent: false` is sent first
(activity, not readiness) so the test also pins that it is not mistaken for
the `true` case.

source: rust-analyzer's serverStatus LSP extension; field names
(`health`, `quiescent`, `message`) verified 2026-09-03 against rust-analyzer
1.95.0 on the dy-wcet corpus (see src/lsp_client/readiness.rs module header).

Every `log()` call happens BEFORE the wire write whose completion it
records, not after — deliberately, to close a race the first version of
this fixture had: logging AFTER `write_message` let the CLIENT process the
bytes (and `initialize_with_probe` return) before this SINGLE-THREADED
script had executed its own next statement (the log write), so the Rust
test's `read_to_string` of the log could run ahead of this script actually
writing to it. Logging first makes the log line provably complete on disk
before the corresponding bytes can even reach the client's pipe, since
both are sequential statements in one thread.

Usage: fake_server_status_server.py <log_path>
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


def main():
    log_path = sys.argv[1]

    # 1. initialize -> respond.
    req = read_message()
    log(log_path, "initialize_answered")
    write_message(
        {
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {"capabilities": {"definitionProvider": True}},
        }
    )

    # 2. initialized (notification — no response expected).
    notif = read_message()
    log(log_path, f"received:{notif.get('method')}")

    # 3. Not-yet-quiescent status: activity, must NOT end the wait.
    log(log_path, "quiescent_false_sent")
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": False, "message": None},
        }
    )

    # 4. Quiescent: the deterministic readiness signal. No workDoneProgress
    #    message is EVER sent by this fixture — if `initialize_with_probe`
    #    returns at all, it did so via this signal, not the progress fallback.
    #    Logged BEFORE the write for the reason in the module docstring: the
    #    client may act on these bytes (and the Rust test may then read this
    #    log) before this process's next statement would otherwise run.
    log(log_path, "quiescent_true_sent")
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": True, "message": None},
        }
    )


if __name__ == "__main__":
    main()
