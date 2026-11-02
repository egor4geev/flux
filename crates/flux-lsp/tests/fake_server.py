"""A fake language server for the client tests.

Speaks just enough LSP: answers initialize, hover, completion (always "content modified"),
shutdown; sends server requests and progress after `initialized`; publishes a diagnostic on
didOpen; asks for its settings again on `flux/askConfiguration`; crashes on `flux/crash`; on
`flux/status`, `flux/register`, `flux/unregister` sends rust-analyzer's `experimental/serverStatus`
and (un)registers watched files with the params given. Every message it receives is appended to the
log file given as the first argument, one JSON per line ({"pid": ..., "env": $FLUX_FAKE_ENV} first).

`--pull`: problems only by pull diagnostics (`diagnosticProvider`, identifier "fake"): a full report
with a new `resultId` after each change, "unchanged" otherwise, a related file's report too;
`flux/cancelNextPull` makes the next pull `ServerCancelled` (retrigger), `flux/refreshDiagnostics`
sends `workspace/diagnostic/refresh`. `--items=a,b`: completion answers these labels (otherwise
"content modified"); `--hover=text`: the hover's Markdown (`""` — no hover).
"""

import json
import os
import sys

log_path = sys.argv[1]
fail_init = "--fail-init" in sys.argv
pull = "--pull" in sys.argv
options = dict(arg[2:].split("=", 1) for arg in sys.argv[2:] if arg.startswith("--") and "=" in arg)
items = options.get("items")
hover = options.get("hover", "**doc**")
# Pull diagnostics: the text's version (+1 per didChange), the result id of the last full report.
changes = 0
reported = None
cancel_next_pull = False
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


log({"pid": os.getpid(), "env": os.environ.get("FLUX_FAKE_ENV")})
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
        capabilities = {
            "textDocumentSync": 2,
            "hoverProvider": True,
            "completionProvider": {"triggerCharacters": ["."]},
        }
        if pull:
            capabilities["diagnosticProvider"] = {
                "identifier": "fake", "interFileDependencies": True, "workspaceDiagnostics": False,
            }
        send({"id": id, "result": {"capabilities": capabilities, "serverInfo": {"name": "fake"}}})
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
    elif method == "textDocument/didChange":
        changes += 1
    elif method == "textDocument/diagnostic":
        params = message["params"]
        if cancel_next_pull:
            cancel_next_pull = False
            send({"id": id, "error": {"code": -32802, "message": "server cancelled",
                                      "data": {"retriggerRequest": True}}})
            continue
        result_id = "r%d" % changes
        if params.get("previousResultId") == result_id and reported == result_id:
            send({"id": id, "result": {"kind": "unchanged", "resultId": result_id}})
            continue
        reported = result_id
        uri = params["textDocument"]["uri"]
        related = uri.rsplit("/", 1)[0] + "/related.rs"
        send({"id": id, "result": {
            "kind": "full",
            "resultId": result_id,
            "items": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 2,
                "message": "pulled %d" % changes,
                "source": params.get("identifier"),
            }],
            "relatedDocuments": {related: {"kind": "full", "items": []}},
        }})
    elif method == "flux/cancelNextPull":
        cancel_next_pull = True
    elif method == "flux/refreshDiagnostics":
        send({"id": "refresh", "method": "workspace/diagnostic/refresh"})
    elif method == "textDocument/didOpen" and pull:
        pass
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
        if not hover:
            send({"id": id, "result": None})
            continue
        send({"id": id, "result": {"contents": {"kind": "markdown", "value": hover}}})
    elif method == "textDocument/completion":
        if items is None:
            send({"id": id, "error": {"code": -32801, "message": "content modified"}})
            continue
        send({"id": id, "result": {"isIncomplete": False, "items": [
            {"label": label} for label in items.split(",") if label
        ]}})
    elif method == "$/cancelRequest":
        send({"id": message["params"]["id"], "error": {"code": -32800, "message": "cancelled"}})
    elif method == "flux/status":
        send({"method": "experimental/serverStatus", "params": message["params"]})
    elif method == "flux/register":
        send({"id": "reg", "method": "client/registerCapability", "params": message["params"]})
    elif method == "flux/unregister":
        send({"id": "unreg", "method": "client/unregisterCapability", "params": message["params"]})
    elif method == "flux/askApply":
        send({"id": 103, "method": "workspace/applyEdit", "params": {"label": "dropped", "edit": {}}})
    elif method == "flux/askConfiguration":
        # The settings changed: ask for them again, as rust-analyzer does.
        send({"id": "conf-2", "method": "workspace/configuration",
              "params": {"items": [{"section": "a"}, {"section": "a.x"}, {"section": "b"}]}})
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
