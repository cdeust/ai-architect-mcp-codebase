#!/usr/bin/env python3
"""Fake LSP server used by
src/lsp_client_tests.rs::lsp_client_waits_for_work_done_progress_end_before_first_definition.

Speaks just enough of LSP 3.17 Base Protocol + Progress over
Content-Length-framed stdio to prove `LspClient::initialize_with_probe`
genuinely blocks on workDoneProgress END, not merely on BEGIN or on the
server's `create` request: this script can only reach "begin_sent" /
"end_sent" in its own log AFTER reading the client's ack of
`window/workDoneProgress/create` on stdin — so if `initialize_with_probe`
returns at all, the wire round trip already completed in order:
create -> ack -> begin -> end. The Rust test asserts on that log's
CONTENT, never on elapsed time (a wall-clock verdict is a load sensor).

source: LSP Specification 3.17 §Base Protocol, §Progress —
https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/

Usage: fake_workdone_progress_server.py <log_path>
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


def main():
    log_path = sys.argv[1]

    # 1. initialize -> respond with definitionProvider + workDoneProgress
    #    capability advertised.
    req = read_message()
    write_message(
        {
            "jsonrpc": "2.0",
            "id": req["id"],
            "result": {"capabilities": {"definitionProvider": True}},
        }
    )
    log(log_path, "initialize_answered")

    # 2. initialized (notification — no response expected).
    notif = read_message()
    log(log_path, f"received:{notif.get('method')}")

    # 3. Server-initiated: announce a workDoneProgress token.
    write_message(
        {
            "jsonrpc": "2.0",
            "id": 999,
            "method": "window/workDoneProgress/create",
            "params": {"token": "readiness-test"},
        }
    )
    log(log_path, "create_sent")

    # 4. Block until the CLIENT's ack of id=999 arrives. This blocking read
    #    is what makes "begin_sent"/"end_sent" impossible in the log unless
    #    the client already processed `create` and wrote its ack back.
    ack = read_message()
    if ack is None or ack.get("id") != 999:
        log(log_path, f"unexpected_ack:{ack}")
        sys.exit(1)
    log(log_path, "create_acked")

    # 5. Only now announce begin, then end.
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {
                "token": "readiness-test",
                "value": {"kind": "begin", "title": "Indexing"},
            },
        }
    )
    log(log_path, "begin_sent")
    write_message(
        {
            "jsonrpc": "2.0",
            "method": "$/progress",
            "params": {"token": "readiness-test", "value": {"kind": "end"}},
        }
    )
    log(log_path, "end_sent")


if __name__ == "__main__":
    main()
