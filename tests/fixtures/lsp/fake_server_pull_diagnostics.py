#!/usr/bin/env python3
"""Fake LSP server used by src/lsp_resolver/unlinked_tests.rs (issue #292).

A healthy handshake (health "ok", quiescent true) that ADVERTISES
`diagnosticProvider`, answers `textDocument/diagnostic` with rust-analyzer's
`unlinked-file` shape for every URI ending in one of the given suffixes and
with an empty report otherwise, and logs every didOpen / definition /
diagnostic request tagged with its URI.

Usage: fake_server_pull_diagnostics.py <log_path> <unlinked_suffix>...
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
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))


def write_message(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    sys.stdout.buffer.flush()


def log(path, line):
    with open(path, "a") as f:
        f.write(line + "\n")


def unlinked_report(uri, suffixes):
    if not any(uri.endswith(s) for s in suffixes):
        return {"kind": "full", "resultId": "rust-analyzer", "items": []}
    return {"kind": "full", "resultId": "rust-analyzer", "items": [{
        "range": {"start": {"line": 0, "character": 0},
                  "end": {"line": 0, "character": 2}},
        "severity": 4, "code": "unlinked-file", "source": "rust-analyzer",
        "message": "This file is not included in any crates, so rust-analyzer "
                   "can't offer IDE services.",
    }]}


def handshake():
    req = read_message()
    write_message({"jsonrpc": "2.0", "id": req["id"], "result": {"capabilities": {
        "definitionProvider": True,
        "diagnosticProvider": {"identifier": "rust-analyzer",
                               "interFileDependencies": True,
                               "workspaceDiagnostics": False},
    }}})
    read_message()  # initialized
    for quiescent in (False, True):
        write_message({"jsonrpc": "2.0", "method": "experimental/serverStatus",
                       "params": {"health": "ok", "quiescent": quiescent}})


def serve(log_path, suffixes):
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        uri = msg.get("params", {}).get("textDocument", {}).get("uri") if msg.get("params") else None
        if method == "textDocument/didOpen":
            log(log_path, f"did_open:{uri}")
        elif method == "textDocument/definition":
            log(log_path, f"definition_requested:{uri}")
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "textDocument/diagnostic":
            log(log_path, f"diagnostic_requested:{uri}")
            write_message({"jsonrpc": "2.0", "id": msg["id"],
                           "result": unlinked_report(uri, suffixes)})
        elif method == "shutdown":
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "exit":
            return


def main():
    handshake()
    serve(sys.argv[1], sys.argv[2:])


if __name__ == "__main__":
    main()
