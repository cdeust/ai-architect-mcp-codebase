#!/usr/bin/env python3
"""Fake LSP server that answers `textDocument/definition` from a script.

Used by src/lsp_resolver/cfg_twin_pass_tests.rs (issue #366). Same healthy
handshake as fake_server_definition_logger.py (health "ok", quiescent true).
The script is a JSON object mapping "<uri suffix>:<0-based line>" to a
definition {"uri": ..., "line": <0-based>}; a request with no entry is answered
with null, so a test states exactly which sites the server resolves and where.

Usage: fake_server_scripted_definition.py <log_path> <script_path>
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


def scripted_answer(script, uri, line):
    """The Location the script gives for (uri, line), or None."""
    for key, target in script.items():
        suffix, _, want = key.rpartition(":")
        if uri.endswith(suffix) and int(want) == line:
            position = {"line": target["line"], "character": 0}
            return {"uri": target["uri"], "range": {"start": position, "end": position}}
    return None


def drain_until_exit(log_path, script):
    """Answers each definition request from `script`, logging every request."""
    while True:
        msg = read_message()
        if msg is None:
            return
        method = msg.get("method")
        if method == "textDocument/definition":
            uri = msg["params"]["textDocument"]["uri"]
            line = msg["params"]["position"]["line"]
            log(log_path, f"definition_requested:{uri}:{line}")
            answer = scripted_answer(script, uri, line)
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": answer})
        elif method == "shutdown":
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "exit":
            return
        elif "id" in msg:
            write_message({"jsonrpc": "2.0", "id": msg["id"], "result": None})


def main():
    log_path = sys.argv[1]
    with open(sys.argv[2]) as f:
        script = json.load(f)
    do_handshake(log_path)
    send_quiescent_ok(log_path)
    drain_until_exit(log_path, script)


if __name__ == "__main__":
    main()
