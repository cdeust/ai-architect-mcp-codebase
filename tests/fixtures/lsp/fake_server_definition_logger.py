#!/usr/bin/env python3
"""Fake LSP server used by
src/lsp_resolver/outside_targets_tests.rs.

Copy of fake_server_status_server.py's healthy handshake (health "ok",
quiescent true, no workDoneProgress message ever sent), extended to log every
`textDocument/didOpen` and `textDocument/definition` it receives, tagged with
the request's own URI. Issue #284 (lot 5)'s outside-target skip must never
even open the file, let alone ask for a definition in it — a fixture that
merely counts requests cannot distinguish "0 requests total" (a passing test
for the wrong reason, e.g. the pass crashed before reaching either file) from
"0 requests for THIS file, N for the other" (the actual claim). Logging the
URI on every request makes that distinction directly observable from the log
instead of inferred from a bare count.

Usage: fake_server_definition_logger.py <log_path>
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


def send_quiescent_ok(log_path):
    log(log_path, "quiescent_false_sent")
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": False, "message": None},
        }
    )
    log(log_path, "quiescent_true_sent")
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "experimental/serverStatus",
            "params": {"health": "ok", "quiescent": True, "message": None},
        }
    )


def drain_until_exit(log_path):
    """Logs and answers every didOpen/definition/shutdown it receives, tagged
    with the request's own URI so a test can assert on WHICH file was ever
    opened or queried, not merely on how many requests arrived."""
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        if method == "textDocument/didOpen":
            uri = msg["params"]["textDocument"]["uri"]
            log(log_path, f"did_open:{uri}")
        elif method == "textDocument/definition":
            uri = msg["params"]["textDocument"]["uri"]
            log(log_path, f"definition_requested:{uri}")
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "shutdown":
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "exit":
            return


def main():
    log_path = sys.argv[1]
    do_handshake(log_path)
    send_quiescent_ok(log_path)
    drain_until_exit(log_path)


if __name__ == "__main__":
    main()
