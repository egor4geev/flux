"""A fake language server for the client tests.

Speaks just enough LSP: answers initialize, hover, completion (always "content modified"),
shutdown; sends server requests and progress after `initialized`; publishes a diagnostic on
didOpen; crashes on `flux/crash`. Every message it receives is appended to the log file given as
the first argument, one JSON per line ({"pid": ...} first).
"""

import json
import os
import sys

log_path = sys.argv[1]
fail_init = "--fail-init" in sys.argv
stdin = sys.stdin.buffer
stdout = sys.stdout.buffer


def log(message):
    with open(log_path, "a") as f:
        f.write(json.dumps(message) + "\n")


def send(message):
    message["jsonrpc"] = "2.0"
    body = json.dumps(message).encode()
    stdout.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    stdout.flush()


def read():
    length = None
    while True:
        line = stdin.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        name, _, value = line.partition(b":")
        if name.lower() == b"content-length":
            length = int(value)
    return json.loads(stdin.read(length))


log({"pid": os.getpid()})
# Noise before the protocol: the client must skip it.
stdout.write(b"fake server starting\n")
stdout.flush()

while True:
    message = read()
    if message is None:
        break
    log(message)
    method = message.get("method")
    id = message.get("id")
    if method == "initialize":
        if fail_init:
            send({"id": id, "error": {"code": -32603, "message": "no workspace"}})
            continue
        send({"id": id, "result": {
            "capabilities": {
                "textDocumentSync": 2,
                "hoverProvider": True,
                "completionProvider": {"triggerCharacters": ["."]},
            },
            "serverInfo": {"name": "fake"},
        }})
    elif method == "initialized":
        send({"id": "conf-1", "method": "workspace/configuration",
              "params": {"items": [{"section": "a"}, {"section": "b"}]}})
        send({"id": 100, "method": "window/workDoneProgress/create", "params": {"token": "t"}})
        send({"id": 101, "method": "custom/unknown", "params": {}})
        send({"id": 102, "method": "workspace/applyEdit", "params": {"edit": {}}})
        send({"method": "$/progress", "params": {"token": "t", "value": {"kind": "begin", "title": "Indexing", "percentage": 0}}})
        send({"method": "$/progress", "params": {"token": "t", "value": {"kind": "report", "percentage": 50}}})
        send({"method": "$/progress", "params": {"token": "t", "value": {"kind": "end"}}})
        send({"method": "window/showMessage", "params": {"type": 1, "message": "hello"}})
        send({"method": "window/logMessage", "params": {"type": 4, "message": "ignored"}})
    elif method == "textDocument/didOpen":
        uri = message["params"]["textDocument"]["uri"]
        send({"method": "textDocument/publishDiagnostics", "params": {
            "uri": uri,
            "version": message["params"]["textDocument"]["version"],
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 2}},
                "severity": 1,
                "message": "bad",
            }],
        }})
    elif method == "textDocument/hover":
        if message["params"]["position"]["line"] == 99:
            continue  # Answered only by a cancel.
        send({"id": id, "result": {"contents": {"kind": "markdown", "value": "**doc**"}}})
    elif method == "textDocument/completion":
        send({"id": id, "error": {"code": -32801, "message": "content modified"}})
    elif method == "$/cancelRequest":
        send({"id": message["params"]["id"], "error": {"code": -32800, "message": "cancelled"}})
    elif method == "flux/askApply":
        send({"id": 103, "method": "workspace/applyEdit", "params": {"label": "dropped", "edit": {}}})
    elif method == "flux/crash":
        sys.stderr.write("fatal: boom\n")
        sys.stderr.flush()
        sys.exit(3)
    elif method == "shutdown":
        send({"id": id, "result": None})
    elif method == "exit":
        sys.exit(0)
    elif id is not None and method is not None:
        send({"id": id, "error": {"code": -32601, "message": "unknown"}})
