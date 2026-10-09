#!/usr/bin/env python3
"""A fake `claude` for UI scenarios and tests: replays a recorded host-mode stream
(`crates/flux-claude/tests/fixtures/<name>.jsonl`) instead of calling the API — no subscription
usage, the same frames every run.

    FLUX_CLAUDE_PATH=crates/flux-claude/fake/fake-claude.py FLUX_FAKE_CLAUDE=h_ask scripts/ui-scenario.sh …

Environment:
- `FLUX_FAKE_CLAUDE`: a fixture name or a path to a `.jsonl` (default `a_text`). Useful ones:
  `a_text` (a plain answer), `b_edit_allow` (Read → Edit asks), `c_edit_modified`, `d_write_deny`
  (Write asks), `e_bash_rule` (Bash asks, suggestions), `f_interrupt` (a long answer),
  `g_tasks` (a task list), `h_ask` (Claude's question), `i_plan` / `i_plan_feedback` (plan mode),
  `j_subagent`, `j_background`, `l_slash` (/usage, /context, /compact), `m_image` (summarized
  thinking), `m2_read_image`, `o_queue`, `r_web`.
- `FLUX_FAKE_CLAUDE_DELAY_MS`: the pause between frames (default 25; stream events — a third).
- `FLUX_FAKE_CLAUDE_SIGNED_OUT=1`: `auth status` says signed out.
- Arguments `--fake-fixture=<name>` and `--fake-delay-ms=<n>` do the same as the variables (tests).

Behaviour:
- `--version` and `auth status` answer at once; anything else is host mode.
- `initialize` gets the recording's answer; each user message replays the recording up to the next
  `result`. Paths of the recording (`/project/<name>`) become the current directory, and the files
  the recording read or edited are created there (with their recorded text) when missing, so the
  app's diff and reload have something to show.
- A recorded permission request waits for the host's answer. Allowed edits (Edit, MultiEdit,
  Write) are applied to the files, with the input the host sent (a changed edit lands as changed);
  a denial ends the turn: the denial's tool result, then the turn's `result`.
- Host requests: `interrupt` ends the turn as the CLI does (a pending request is withdrawn with
  `control_cancel_request`); the rest get canned answers.
"""
import json
import os
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
FIXTURES = os.path.join(HERE, "..", "tests", "fixtures")
EDIT_TOOLS = ("Edit", "MultiEdit", "Write")


def main():
    args = sys.argv[1:]
    if "--version" in args or "-v" in args:
        print("2.1.285 (Claude Code)")
        return
    if args[:2] == ["auth", "status"]:
        signed_out = os.environ.get("FLUX_FAKE_CLAUDE_SIGNED_OUT") == "1"
        print(json.dumps({"loggedIn": not signed_out, "authMethod": "claude.ai", "email": "user@example.com"}))
        return
    # Tests pass the recording as arguments (`LaunchOptions::extra_args`) instead of the environment.
    for arg in args:
        if arg.startswith("--fake-fixture="):
            os.environ["FLUX_FAKE_CLAUDE"] = arg.split("=", 1)[1]
        elif arg.startswith("--fake-delay-ms="):
            os.environ["FLUX_FAKE_CLAUDE_DELAY_MS"] = arg.split("=", 1)[1]
    Host(load(os.environ.get("FLUX_FAKE_CLAUDE", "a_text"))).run()


def load(name):
    path = name if name.endswith(".jsonl") else os.path.join(FIXTURES, name + ".jsonl")
    frames = []
    for line in open(path):
        line = line.strip()
        if line:
            frames.append(json.loads(line))
    return frames


class Host:
    def __init__(self, frames):
        self.frames = frames
        self.position = 0
        self.lock = threading.Lock()
        self.answers = {}
        self.answered = threading.Condition(self.lock)
        self.interrupted = threading.Event()
        self.delay = int(os.environ.get("FLUX_FAKE_CLAUDE_DELAY_MS", "25")) / 1000
        self.turns = []
        self.turn_ready = threading.Condition(threading.Lock())
        self.cwd = os.getcwd()
        self.recorded_cwd = next((f.get("cwd") for f in frames
                                  if f.get("type") == "system" and f.get("subtype") == "init"), None)

    # --- Paths and files ---

    def local(self, text):
        """The recording's paths in the current directory."""
        if self.recorded_cwd and isinstance(text, str):
            return text.replace(self.recorded_cwd, self.cwd)
        return text

    def recorded_result(self, tool_use_id):
        """The recorded `tool_use_result` of a tool call (looking ahead)."""
        for frame in self.frames[self.position:]:
            if frame.get("type") != "user":
                continue
            content = frame.get("message", {}).get("content")
            if isinstance(content, list) and any(
                    block.get("type") == "tool_result" and block.get("tool_use_id") == tool_use_id
                    for block in content if isinstance(block, dict)):
                return frame.get("tool_use_result")
        return None

    def ensure_file(self, path, text):
        """Creates a file the recording read or edited, with its recorded text, when missing."""
        path = self.local(path)
        if not path or text is None or not path.startswith(self.cwd + os.sep) or os.path.exists(path):
            return
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as file:
            file.write(text)

    def prepare(self, frame):
        """Before a frame goes out: the files it is about exist."""
        if frame.get("type") == "user" and isinstance(frame.get("tool_use_result"), dict):
            result = frame["tool_use_result"]
            if result.get("type") == "text" and isinstance(result.get("file"), dict):
                self.ensure_file(result["file"].get("filePath"), result["file"].get("content"))
        if frame.get("type") == "control_request":
            request = frame.get("request", {})
            if request.get("subtype") == "can_use_tool" and request.get("tool_name") in EDIT_TOOLS:
                result = self.recorded_result(request.get("tool_use_id"))
                if isinstance(result, dict) and isinstance(result.get("originalFile"), str):
                    self.ensure_file(request.get("input", {}).get("file_path"), result["originalFile"])

    def apply_edit(self, tool, data):
        path = data.get("file_path")
        if not path:
            return
        try:
            if tool == "Write":
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w") as file:
                    file.write(data.get("content", ""))
                return
            with open(path) as file:
                text = file.read()
            edits = data.get("edits") if tool == "MultiEdit" else [data]
            for edit in edits or []:
                old, new = edit.get("old_string", ""), edit.get("new_string", "")
                if old and old in text:
                    text = text.replace(old, new) if edit.get("replace_all") else text.replace(old, new, 1)
            with open(path, "w") as file:
                file.write(text)
        except OSError as err:
            sys.stderr.write(f"fake-claude: can't apply the edit of {path}: {err}\n")

    # --- I/O ---

    def run(self):
        threading.Thread(target=self.player, daemon=True).start()
        for line in sys.stdin:
            try:
                message = json.loads(line)
            except ValueError:
                continue
            kind = message.get("type")
            if kind == "control_request":
                self.host_request(message)
            elif kind == "control_response":
                response = message.get("response", {})
                with self.answered:
                    self.answers[response.get("request_id")] = response
                    self.answered.notify_all()
            elif kind == "user":
                with self.turn_ready:
                    self.turns.append(message)
                    self.turn_ready.notify_all()
        # stdin closed: the CLI exits.
        os._exit(0)

    def host_request(self, message):
        request = message.get("request", {})
        subtype = request.get("subtype")
        request_id = message.get("request_id")
        if subtype == "initialize":
            recorded = next((f for f in self.frames if f.get("type") == "control_response"
                             and "commands" in (f.get("response", {}).get("response") or {})), None)
            response = recorded["response"]["response"] if recorded else {"commands": [], "models": []}
            return self.reply(request_id, response)
        if subtype == "interrupt":
            self.interrupted.set()
            with self.answered:
                self.answered.notify_all()
            return self.reply(request_id, {"still_queued": []})
        canned = {
            "get_context_usage": {"totalTokens": 28900, "maxTokens": 200000, "percentage": 14},
            "get_usage": {"rate_limits": {"five_hour": {"utilization": 12, "resets_at": "2030-01-01T00:00:00Z"},
                                          "seven_day": {"utilization": 49, "resets_at": "2030-01-05T00:00:00Z"}}},
            "list_models": {"models": [
                {"value": "default", "displayName": "Default (Opus 5.5)", "description": "Recommended",
                 "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
                {"value": "sonnet", "displayName": "Sonnet 5.5", "description": "Fast",
                 "supportedEffortLevels": ["low", "medium", "high"]},
                {"value": "haiku", "displayName": "Haiku 4.5", "description": "Fastest"}]},
            "get_binary_version": {"version": "2.1.285"},
            "file_suggestions": {"suggestions": [], "cwd": self.cwd},
        }
        if subtype == "set_permission_mode":
            self.write({"type": "system", "subtype": "status", "status": None,
                        "permissionMode": request.get("mode")})
            return self.reply(request_id, {"mode": request.get("mode")})
        self.reply(request_id, canned.get(subtype, {}))

    def reply(self, request_id, response):
        self.write({"type": "control_response",
                    "response": {"subtype": "success", "request_id": request_id, "response": response}})

    def write(self, frame):
        text = json.dumps(frame, ensure_ascii=False)
        if self.recorded_cwd:
            text = text.replace(self.recorded_cwd, self.cwd)
        with self.lock:
            sys.stdout.write(text + "\n")
            sys.stdout.flush()

    # --- Replay ---

    def player(self):
        while True:
            with self.turn_ready:
                while not self.turns:
                    self.turn_ready.wait()
                message = self.turns.pop(0)
            self.interrupted.clear()
            self.play_turn(message.get("uuid"))

    def play_turn(self, uuid):
        while self.position < len(self.frames):
            frame = self.frames[self.position]
            self.position += 1
            kind = frame.get("type")
            if kind == "control_response":
                continue  # answers to the recording host's own requests
            if kind == "command_lifecycle" and uuid:
                frame = dict(frame, command_uuid=uuid)
            if self.interrupted.is_set():
                self.finish_interrupted()
                return
            time.sleep(self.delay / 3 if kind == "stream_event" else self.delay)
            self.prepare(frame)
            self.write(frame)
            if kind == "control_request" and frame.get("request", {}).get("subtype") == "can_use_tool":
                if not self.wait_answer(frame):
                    return
            if kind == "result":
                # The turn is over; the lifecycle and state frames after it belong to it too.
                while self.position < len(self.frames) and self.frames[self.position].get("type") in (
                        "command_lifecycle", "system") and self.frames[self.position].get("subtype") != "init":
                    self.write(self.frames[self.position])
                    self.position += 1
                return
        # The recording is over: a short answer so that more messages still get a turn.
        self.write({"type": "assistant", "message": {"id": "fake", "role": "assistant", "model": "fake",
                    "content": [{"type": "text", "text": "(the recording is over)"}]}, "parent_tool_use_id": None})
        self.write({"type": "result", "subtype": "success", "is_error": False, "result": "", "duration_ms": 1,
                    "num_turns": 1, "total_cost_usd": 0, "terminal_reason": "completed"})

    def wait_answer(self, frame):
        """Waits for the host's answer to a permission request; `False` — the turn ended."""
        request_id = frame.get("request_id")
        request = frame.get("request", {})
        with self.answered:
            while request_id not in self.answers and not self.interrupted.is_set():
                self.answered.wait(0.1)
            answer = self.answers.get(request_id)
        if answer is None:
            self.write({"type": "control_cancel_request", "request_id": request_id})
            self.finish_interrupted(tool_use_id=request.get("tool_use_id"))
            return False
        decision = answer.get("response") or {}
        if decision.get("behavior") == "deny":
            self.finish_denied(request.get("tool_use_id"), decision.get("message", ""))
            return False
        if request.get("tool_name") in EDIT_TOOLS:
            self.apply_edit(request.get("tool_name"), decision.get("updatedInput") or request.get("input", {}))
        return True

    def skip_to_result(self):
        """The recorded turn's `result` (the rest of the turn is skipped)."""
        while self.position < len(self.frames):
            frame = self.frames[self.position]
            self.position += 1
            if frame.get("type") == "result":
                return frame
        return {"type": "result", "subtype": "success", "is_error": False, "duration_ms": 1,
                "num_turns": 1, "total_cost_usd": 0, "terminal_reason": "completed"}

    def finish_denied(self, tool_use_id, message):
        self.write({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": tool_use_id, "content": message, "is_error": True}]},
            "parent_tool_use_id": None, "tool_use_result": "Error: " + message,
            "tool_result_meta": [{"id": tool_use_id, "non_execution_kind": "permission-rule"}]})
        self.write({"type": "assistant", "message": {"id": "fake-denied", "role": "assistant", "model": "fake",
                    "content": [{"type": "text", "text": "Understood — I won't do that."}]},
                    "parent_tool_use_id": None})
        result = self.skip_to_result()
        self.write(dict(result, is_error=False, result="Understood — I won't do that."))
        self.end_turn()

    def finish_interrupted(self, tool_use_id=None):
        if tool_use_id:
            self.write({"type": "user", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool_use_id, "is_error": True,
                 "content": "The user doesn't want to proceed with this tool use."}]},
                "parent_tool_use_id": None, "tool_use_result": "User rejected tool use",
                "tool_result_meta": [{"id": tool_use_id, "non_execution_kind": "user-rejected"}]})
        marker = "[Request interrupted by user for tool use]" if tool_use_id else "[Request interrupted by user]"
        self.write({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": marker}]},
                    "parent_tool_use_id": None})
        self.write({"type": "result", "subtype": "error_during_execution", "is_error": True, "duration_ms": 1,
                    "num_turns": 0, "total_cost_usd": 0,
                    "terminal_reason": "aborted_tools" if tool_use_id else "aborted_streaming"})
        # The rest of this turn in the recording is skipped.
        self.skip_to_result()
        self.end_turn()

    def end_turn(self):
        """After a turn cut short: the recorded frames that closed it are skipped, the session is
        idle."""
        while self.position < len(self.frames) and self.frames[self.position].get("type") in (
                "command_lifecycle", "system") and self.frames[self.position].get("subtype") != "init":
            self.position += 1
        self.write({"type": "system", "subtype": "session_state_changed", "state": "idle"})


if __name__ == "__main__":
    main()
