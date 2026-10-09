# Claude Code CLI as a headless engine: stream-json + control protocol (Flux host reference)

**Sources.** Recorded against `claude` **2.1.285** on 2026-10-09 (Homebrew, claude.ai Team subscription, `--model haiku`). Types come from `@anthropic-ai/claude-agent-sdk` **0.3.295** (`sdk.d.ts`, `sdk-tools.d.ts`, `sdk.mjs`), which target CLI **2.1.295**.

**Files.** `tests/fixtures/<name>.jsonl` holds the CLI's stdout (sanitized: no account data, local paths or thinking signatures); `tests/fixtures/<name>.stdin.jsonl` holds what the host wrote. Only a part of the recordings listed in §16 is in the repository. `fake/fake-claude.py` replays a recording for UI scenarios.

**Tags.** **[V]** seen in a recording (fixture prefix in parentheses). **[T]** taken only from the SDK types or `sdk.mjs`. **[≠]** the recording is missing from, or differs from, the SDK types.

---------------------------------------------------------------------------------------------------

## 0. Rules for the host

1. **Spawn** `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio …` (§1) with env `CLAUDE_CODE_SDK_READS_SESSION_STATE=1`, which turns on `session_state_changed`.
2. **Write order.** Send the `initialize` control_request first, then user messages straight away; the SDK does not wait for the reply. Keep stdin open for the whole session. Closing stdin makes the CLI finish and exit with code 0, or 1 if the last result was an error [V].
3. **Parse** stdout as JSONL and dispatch on `type`, then `subtype`. **Ignore unknown types and fields**: 2.1.285 already emits several that are not in the SDK types (§14).
4. **End of turn vs. idle.** Each turn ends with exactly one `result`. The session is idle only after `system/session_state_changed {state:"idle"}`, because a finished background task makes the CLI start a new turn by itself (new `init` … `result`) with no host input [V j_background].
5. **Control requests.** Answer every CLI→host `control_request` exactly once, with success or error. A `control_cancel_request` means: close that prompt's UI and do not answer [V f2].
6. **Streaming.** Stream text from `stream_event` deltas, then commit the complete `assistant` message. There is one message per content block, and several messages can share one `message.id` [V].
7. **Tool pairing.** Pair `tool_use.id` with `tool_result.tool_use_id`. Render diffs and outputs from **`tool_use_result`**, not from `tool_use.input`, because the host may have changed the input (§8.4).
8. **Subagents.** `parent_tool_use_id != null` means the message belongs to the subagent spawned by that Agent `tool_use` [V].
9. **UUIDs.** Give every user message a `uuid`. It is echoed in `command_lifecycle`, `assistant`/`stream_event` `user_message_uuid`, and `result.user_message_uuid(s)` [V].
10. **Feature detection.** Use `get_binary_version`, `system/init.claude_code_version` and `system/init.capabilities`. There is no protocol version (§14).

---------------------------------------------------------------------------------------------------

## 1. Spawning

### 1.1 argv

```
claude -p --input-format stream-json --output-format stream-json --verbose
       --include-partial-messages --permission-prompt-tool stdio
       [--model M] [--permission-mode MODE] [--effort L] [--resume ID | --continue] [--session-id UUID]
       [--fork-session] [--add-dir D]… [--mcp-config JSON] [--settings JSON|PATH] [--tools A,B]
       [--allowedTools …] [--disallowedTools …] [--forward-subagent-text] [--thinking-display summarized]
```

- **Argument form.** The SDK leaves out `-p` (non-TTY stdio implies print mode) and writes each option as one `--flag=value` token. Both forms work [V].
- **`--permission-mode` values** [V]: `default` (the help text calls it `manual`; both become `default`), `acceptEdits`, `plan`, `dontAsk`, `auto`, `bypassPermissions`.
- **`--no-session-persistence`** skips only the transcript `.jsonl`. These are still created [V]: `~/.claude/projects/<slug>/memory/` and `<slug>/<session>/subagents/agent-*.meta.json`.
- **CLI flags worth knowing.** Verified [V]: `--forward-subagent-text`, `--replay-user-messages`, `--thinking-display summarized|omitted` (hidden in `--help`). `--prompt-suggestions` is accepted but produced no output (§5.7). Not tested [T]: `--include-hook-events`, `--name`, `--append-system-prompt`, `--system-prompt`, `--json-schema`, `--max-turns`, `--max-budget-usd`, `--fallback-model`, `--agents`, `--strict-mcp-config`, `--setting-sources`, `--bare`, `--safe-mode`.

### 1.2 SDK `Options` → CLI channel (from `sdk.mjs`) [T]

| Option | Channel |
|---|---|
| always | `--output-format stream-json --verbose --input-format stream-json` |
| `canUseTool` | **`--permission-prompt-tool=stdio`**: permission asks become `can_use_tool` requests |
| `permissionPromptToolName` / `permissionPrompts` | `--permission-prompt-tool=<mcp tool>` / `--permission-prompts=host\|none` (`none` auto-denies every ask) |
| `model`, `fallbackModel`, `agent`, `betas` | `--model=` `--fallback-model=` `--agent=` `--betas=a,b` |
| `permissionMode`, `allowDangerouslySkipPermissions` | `--permission-mode=` `--allow-dangerously-skip-permissions` |
| `continue`, `resume`, `resumeSessionAt`, `forkSession`, `sessionId` | `--continue` `--resume=ID` `--resume-session-at=UUID` `--fork-session` `--session-id=UUID` |
| `persistSession:false` | `--no-session-persistence` |
| `effort` | `--effort=low\|medium\|high\|xhigh\|max` |
| `thinking` | `adaptive` → `--thinking=adaptive`; `{enabled,budgetTokens:N}` → `--max-thinking-tokens=N`; `disabled` → `--thinking=disabled`; `display` → `--thinking-display=` |
| `maxThinkingTokens` (deprecated) | `0` → `--thinking=disabled`; `N` → `--max-thinking-tokens=N` |
| `maxTurns`, `maxBudgetUsd`, `taskBudget` | `--max-turns=` `--max-budget-usd=` `--task-budget=` |
| `includePartialMessages`, `includeHookEvents` | `--include-partial-messages` `--include-hook-events` |
| `allowedTools`, `disallowedTools`, `tools` | `--allowedTools=a,b` `--disallowedTools=a,b` `--tools=a,b`; `[]` → `--tools=`; preset → `--tools=default` |
| `mcpServers` (stdio, sse, http) | `--mcp-config '{"mcpServers":{name:cfg}}'` |
| `mcpServers` with `type:"sdk"` (in-process) | No flag. Names go in `initialize.sdkMcpServers`; traffic flows over `mcp_message` (§7.3). |
| `strictMcpConfig`, `settingSources` | `--strict-mcp-config` `--setting-sources=user,project,local` |
| `settings`, `sandbox` | `--settings <path or JSON>`; the sandbox config is merged into the JSON |
| `additionalDirectories` | one `--add-dir=<dir>` per entry |
| `plugins` | `--plugin-dir=P` (or `--plugin-dir-no-mcp=P`), or `initialize.plugins` together with `--await-initialize` |
| `outputFormat {type:"json_schema"}` | `--json-schema=<json>` plus `initialize.jsonSchema` |
| `extraArgs` | `--key value`, or `--key` alone for a null value |
| `hooks`, `agents`, `systemPrompt`/`appendSystemPrompt`, `promptSuggestions`, `agentProgressSummaries`, `forwardSubagentText`, `title`, `toolAliases`, `planModeInstructions`, `supportedDialogKinds`, `perTaskStopAffordance`, `skills` | `initialize` fields only (§2) |

**System prompt trap.** With no `systemPrompt` option, the SDK sends `initialize.systemPrompt:[""]`, which is an *empty* system prompt. If the `systemPrompt` field is left out entirely, the CLI uses Claude Code's own prompt, which is what Flux wants. Every recording here left it out and behaved like Claude Code (`get_context_usage` reports a system prompt of about 6–7k tokens) [V].

### 1.3 Environment

| Var | Effect |
|---|---|
| `CLAUDE_CODE_SDK_READS_SESSION_STATE=1` | Turns on `system/session_state_changed` (idle / running / requires_action). The SDK always sets it [V]. |
| `CLAUDE_CODE_ENTRYPOINT=sdk-ts`, `CLAUDE_AGENT_SDK_VERSION=x` | Set by the SDK; go into telemetry and the transcript `entrypoint` [T] |
| `CLAUDE_CODE_ENABLE_TASKS=false` | Brings back legacy `TodoWrite` in place of `TaskCreate/…` [V g_todowrite_legacy] |
| `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true` | Needed for `rewind_files`. Without it: `{canRewind:false,error:"File rewinding is not enabled."}` [V k] |
| `CLAUDE_CODE_QUESTION_PREVIEW_FORMAT=markdown\|html` | Format of AskUserQuestion option previews [T] |
| `CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=false` | Hard-disables prompt suggestions [T, found in the binary] |

The SDK also deletes `NODE_OPTIONS` and `DEBUG` from the child's environment.

### 1.4 Framing and lifecycle

- **Framing.** Each frame is UTF-8 JSON followed by `\n`, in both directions. Skip non-JSON lines. Ignore `{"type":"keep_alive"}`, which may arrive at any time [T].
- **SDK shutdown.** `stdin.end()`, then SIGTERM after 2 s and SIGKILL after 5 s more.
- **Stopping one turn.** Use the `interrupt` control request (§7.2), not a kill.

---------------------------------------------------------------------------------------------------

## 2. `initialize` (host → CLI, first frame)

Request [T]. Every field is optional:

```json
{"type":"control_request","request_id":"<unique>","request":{"subtype":"initialize",
 "hooks":{"PreToolUse":[{"matcher":"Bash","hookCallbackIds":["hook_0"],"timeout":30}]},
 "sdkMcpServers":["flux"], "sdkMcpServerConfigs":{"flux":{"timeout":60000}},
 "systemPrompt":["…"], "appendSystemPrompt":"…", "systemPromptSnapshot":true, "excludeDynamicSections":false,
 "agents":{"reviewer":{"description":"…","prompt":"…","tools":["Read"],"model":"sonnet"}},
 "jsonSchema":{…}, "title":"…", "skills":["name"], "toolAliases":{"Old":"New"}, "planModeInstructions":"…",
 "promptSuggestions":true, "agentProgressSummaries":true, "forwardSubagentText":true,
 "supportedDialogKinds":["refusal_fallback_prompt"], "perTaskStopAffordance":true, "plugins":[{"type":"local","path":"…"}]}}
```

**What was exercised [V]:** a bare `{subtype:"initialize"}`; `hooks` (p_hooks; the reply carries `hooks_applied:true`); `sdkMcpServers` (q_sdk_mcp); `promptSuggestions` and `perTaskStopAffordance` (accepted). The SDK names hook callbacks `hook_0`, `hook_1`, … in registration order.

**Response [V]:**

```
{"type":"control_response","response":{"subtype":"success","request_id":…,
  "response":{…},"pending_permission_requests":[],"pending_user_dialog_requests":[]}}
```

`response` holds:
- `commands[]`: `SlashCommand {name, description, argumentHint, aliases?, builtin?}`. There were 58 on this machine, including skills; plugin skills carry an alias like `anthropic-skills:docx`.
- `agents[]`: `{name, description, model?}`.
- `output_style`, `available_output_styles[]`, `user_output_styles_dir`.
- `models[]`: `ModelInfo` (§13).
- `account`: `{email, organization, subscriptionType, apiProvider, tokenSource?, apiKeySource?}`.
- `pid`, `current_permission_mode`, `session_state:"idle"`, `fast_mode_state`, `fast_mode_disabled_reason`, `hooks_applied?`, `feedback_mode{kind}`, `analytics_disabled`, `remote_control_*` (×5), `ide_rc_auto_enable_gate`.

There is **no CLI version** in this response. It does contain the account email and organization, so **redact fixtures before committing them.**

---------------------------------------------------------------------------------------------------

## 3. User messages (host → CLI)

```json
{"type":"user","message":{"role":"user","content":"text" | [blocks]},"parent_tool_use_id":null,
 "session_id":"","uuid":"<uuid4>", "priority":"now|next|later"?, "shouldQuery":false?,
 "client_composed":true?, "origin":{"kind":"human"}?, "pasted_content":[…]?, "inline_pastes":["…"]?}
```

**Content.**
- A string, or `[{"type":"text","text":…}]` (the SDK uses the array form).
- Images: `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"…"}}` [V m_image].
- PDFs: `{"type":"document",…}` [T].

**Slash commands** are sent as plain text, e.g. `"/compact focus on X"` [V l_slash].
- Built-in local commands such as `/usage` (aliases `/cost`, `/stats`), `/context` and `/compact` are handled by the CLI itself; `/compact` makes its own summarization call. 2.1.285 answers them with a **synthetic assistant message** (§5.3), not with `system/local_command_output`.
- Prompt-type commands and skills expand into a normal turn [T].
- An unknown `/word` is passed to the model as ordinary text [V].

**`@path` mentions** are expanded by the CLI, which attaches the file content. No tool_use event appears [V l_slash]. `client_composed:true` turns off both @-expansion and slash dispatch [T].

**Messages sent while a turn is running** [V o_queue]:
- **Default.** You get `command_lifecycle {state:"queued"}`. The message runs as its own turn after the current one, in FIFO order: `started`, then its own `result`, then `completed`. Between tool calls, queued messages may be folded into the running turn; `user_message_uuids` then lists all of them [T].
- **`priority:"now"`** aborts the running turn and runs right away. The aborted turn's `assistant` has `aborted:true`, its `result` is `subtype:"success"` with `terminal_reason:"aborted_streaming"`, and its lifecycle reaches `cancelled`.
- **`cancel_async_message {message_uuid}`** removes a still-queued message: `{cancelled:true}` plus lifecycle `cancelled`. An unknown or already-started uuid gives `{cancelled:false}`.

**Other fields.** `shouldQuery:false` appends the message without starting a turn [T]. The types recommend `origin:{kind:"human"}` for keyboard input; leaving it out caused no problems. `--replay-user-messages` echoes each accepted message back as `user` with `isReplay:true`; slash commands echo as `<command-name>/usage</command-name>…` [V l_slash].

---------------------------------------------------------------------------------------------------

## 4. Turn anatomy (order observed: b_edit_allow, a_text)

```
control_response (initialize)                                    once per process
command_lifecycle {command_uuid:<your uuid>, state:"queued"} → "started"
system/session_state_changed {state:"running"}                   (env-gated)
system/init                                                      at the start of EVERY turn
system/status {status:"requesting"}                              before EVERY API call
stream_event message_start → content_block_start(thinking) → [system/thinking_tokens]* → deltas
assistant {content:[thinking]}                                   one assistant message per finished block
stream_event content_block_stop → content_block_start(text|tool_use) → deltas
assistant {content:[tool_use]}                                   the block arrives BEFORE its content_block_stop
stream_event content_block_stop → message_delta{stop_reason} → message_stop
control_request can_use_tool → (host replies) → [system/status {permissionMode}]
user {content:[tool_result], tool_use_result}
… repeat from system/status for each follow-up API call …
rate_limit_event                                                 after API calls (when info changes)
result
command_lifecycle {state:"completed"}
system/session_state_changed {state:"idle"}
```

---------------------------------------------------------------------------------------------------

## 5. stdout message reference

### 5.1 `system/init` [V]

Sent again at the start of **every** turn, so it is where to refresh the model and mode badges.

- **Session:** `cwd`, `session_id`, `uuid`, `model` (resolved id), `permissionMode`, `claude_code_version`, `output_style`, `apiKeySource` (`"none"` with OAuth), `memory_paths{auto}`, `messaging_socket_path`.
- **Capabilities:** `tools[]` (the agent tool is listed as `"Task"`, but the model calls it `"Agent"`), `mcp_servers[{name,status,source}]`, `slash_commands[]`, `terminal_slash_commands[]` (hide these in a GUI), `agents[]`, `skills[]`, `plugins[{name,path,source,version?}]`, `capabilities[]`.
- **Other:** `fast_mode_state`, `fast_mode_disabled_reason`, `effort?` (absent for haiku), `per_turn_effort_active`, `view_mode`, `analytics_disabled`, `product_feedback_disabled`.

### 5.2 `stream_event` (only with `--include-partial-messages`) [V]

Shape: `{type, event, parent_tool_use_id, uuid, session_id, ttft_ms?, user_message_uuid(s)?, thinking_display?}`.

`event` is a raw Anthropic streaming event:

| `event.type` | Payload |
|---|---|
| `message_start` | `{message{id,model,usage…}}` |
| `content_block_start` | `{index, content_block}` |
| `content_block_delta` | `{index, delta}`, where `delta` is `text_delta{text}`, `thinking_delta{thinking, estimated_tokens}`, `signature_delta{signature}` or `input_json_delta{partial_json}` |
| `content_block_stop` | `{index}` |
| `message_delta` | `{delta{stop_reason}, usage}` |
| `message_stop` | — |

- The first event of a turn carries `ttft_ms` and `user_message_uuid(s)`.
- **Subagents produce no stream_events.** Their output arrives only as complete messages [V j_subagent].

### 5.3 `assistant` [V]

```
{type:"assistant", message:{id, model, role, content:[ONE block], stop_reason:null, usage, container,
 context_management, diagnostics, stop_details, input_transformations}, parent_tool_use_id, uuid, session_id,
 request_id, timestamp, user_message_uuid(s)?, wire_tool_inputs?≠, aborted?:true, agent_id?, subagent_type?,
 task_description?, error?, supersedes?, …}
```

- **One block per message.** `stop_reason` is therefore always `null` and `usage` is not final; read the final values from `result`.
- **`wire_tool_inputs` [≠]** is `{tool_use_id: input exactly as the model emitted it}`. For ExitPlanMode it is `{}` while `content[].input` holds the plan the CLI filled in.
- **`aborted:true`** means the turn was interrupted: the text is truncated and no stop_reason arrives [V f_interrupt].
- **`error`** [T] is one of: `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`, `verification_required`, `billing_error`, `rate_limit`, `overloaded`, `invalid_request`, `model_not_found`, `server_error`, `unknown`, `max_output_tokens`, `cloud_credential_error`.

**Local slash-command reply [≠ V l_slash].**
- The message has `message.model:"<synthetic>"`, `content:[text]`, `local_command_source:"<local-command-stdout>…"` and `local_command_run`.
- `/usage` adds `usage_report`.
- `/context` adds `context_usage{model, total_tokens, raw_max_tokens, percentage, categories[{name,tokens,kind}], mcp_tools[], memory_files[], agents[], skills?}`.
- A `result` follows with `num_turns:0` and `local_command:"cost"`.

### 5.4 `user` (emitted by the CLI) [V]

```
{type:"user", message:{role:"user", content:string|blocks}, parent_tool_use_id, uuid, session_id, timestamp,
 tool_use_result?, tool_result_meta?≠, isSynthetic?, isReplay?, subagent_type?, task_description?}
```

What arrives as `user`:
- **Tool results:** `content:[{type:"tool_result", tool_use_id, content, is_error?}]` plus `tool_use_result` (§9). If the call did not run, `tool_result_meta [{id, non_execution_kind:"permission-rule"|"user-rejected"}]` [≠] is added.
- **Interrupt markers:** `[{type:"text",text:"[Request interrupted by user]"}]`, or `"…for tool use]"` when a tool or prompt was pending.
- **Others:** the subagent prompt (the first message with `parent_tool_use_id` set), the compaction summary (`isSynthetic:true`), and replays (`isReplay:true`).

### 5.5 `result` [V]

Exactly one per turn.
- **Success:** `subtype:"success"`, with `result` holding the final text. `is_error:true` here means the turn ended on an API error.
- **Error subtypes:** `error_during_execution`, `error_max_turns`, `error_max_budget_usd`, `error_max_structured_output_retries`, with `errors[]`.

Fields:
- **Timing and turns:** `duration_ms`, `duration_api_ms`, `ttft_ms`, `num_turns`, `stop_reason`, `terminal_reason`.
- **Cost and usage:** `total_cost_usd` is **cumulative for the process** (it grows across turns); `usage` covers the main loop for this turn only; `modelUsage{model:{inputTokens, outputTokens, cacheReadInputTokens, cacheCreationInputTokens, thinkingTokens, webSearchRequests, costUSD, contextWindow, maxOutputTokens, canonicalModel, provider, costBasis}}`.
- **Other:** `permission_denials[{tool_name, tool_use_id, tool_input}]`, `queued_turn_count`, `result_index`, `user_message_uuid(s)`, `local_command?`, `api_error_status`, `fast_mode_*`, `subagent_stats{spawned, completed, failed, killed, refused, by_type…}` [≠], `structured_output?`, `deferred_tool_use?` [T].

`terminal_reason`:
- Observed [V]: `completed`; `aborted_streaming` (interrupted while generating); `aborted_tools` (interrupted during a tool or prompt); `null` for local commands.
- Others [T]: `max_turns`, `prompt_too_long`, `blocking_limit`, `budget_exhausted`, `stop_hook_prevented`, `hook_stopped`, `tool_deferred`, `model_error`, `api_error`, `image_error`, `malformed_tool_use_exhausted`, `background_requested`, `structured_output_retry_exhausted`, `turn_setup_failed`, `rapid_refill_breaker`, `tool_deferred_unavailable`.

The interrupted turn in f_interrupt reported `total_cost_usd:0` and `errors:["[ede_diagnostic] …"]`.

### 5.6 Other `system` subtypes

| subtype | Fields / meaning | Status |
|---|---|---|
| `status` | `status: "requesting"\|"compacting"\|null`, `permissionMode?` (this is how mode changes are announced), `compact_result?`, `compact_error?` | V |
| `compact_boundary` | `compact_metadata{trigger:"manual"\|"auto", pre_tokens, post_tokens, duration_ms, cumulative_dropped_tokens≠, preserved_segment{head_uuid,anchor_uuid,tail_uuid}, preserved_messages{anchor_uuid,uuids}}` | V l_slash |
| `thinking_tokens` | `estimated_tokens, estimated_tokens_delta, user_message_uuid`: a live counter for hidden thinking | V |
| `task_started` | `task_id, tool_use_id, description, task_type ("local_agent"\|"local_bash"\|…), subagent_type?, is_backgrounded, spawn_depth?, prompt?, parent_task_id?, run_id?, skip_transcript?, ambient?` | V |
| `task_progress` | `task_id, tool_use_id, description, subagent_type, usage{total_tokens,tool_uses,duration_ms}, last_tool_name, summary?` | V |
| `task_updated` | `task_id, patch{status: pending\|running\|completed\|failed\|killed\|paused, end_time, error?, is_backgrounded?}` | V |
| `task_notification` | `task_id, tool_use_id, status: completed\|failed\|stopped, output_file, summary, usage?` | V |
| `background_tasks_changed` | `tasks[{task_id, task_type, description, subagent_type?, parent_task_id?, ambient?}]`: **replace** your set | V j_background |
| `session_state_changed` | `state: idle\|running\|requires_action`, `sdk_host_only:true` (needs the env var) | V |
| `permission_denied` | `tool_name, tool_use_id, decision_reason_type ("mode"…), decision_reason?, message, agent_id?`. Sent only for **automatic** denials (dontAsk, rules, classifier), never for a host deny. | V d2 |
| `session_title_changed` | `title` | ≠ V k |
| `commands_changed` | `commands[]`: **replace** the cached list | V k2 |
| `api_retry` | `attempt, max_retries, retry_delay_ms, error_status, error, no_response?` | T |
| `notification` | `key, text, priority, color?, timeout_ms?` | T |
| `informational` | `content, level: info\|notice\|suggestion\|warning, tool_use_id?, prevent_continuation?, tag?` | T |
| `local_command_output` | `content`. 2.1.285 does not use it for /usage or /context (§5.3). | T |
| `hook_started` / `hook_progress` / `hook_response` | `hook_id, hook_name, hook_event, stdout, stderr, output, exit_code?, outcome`. Only for settings-file hooks with `--include-hook-events`; none appeared for SDK callback hooks. | T |
| `model_refusal_fallback` / `_no_fallback` | `original_model, fallback_model?, direction, content, retracted_message_uuids?, refused_user_message_uuid?` | T |
| `elicitation_complete`, `files_persisted`, `memory_recall`, `plugin_install`, `mirror_error`, `worker_shutting_down`, `control_request_progress` | Rare or internal; log them | T |

### 5.7 Other top-level `type`s

| type | Fields / meaning | Status |
|---|---|---|
| `rate_limit_event` | `rate_limit_info{status: allowed\|allowed_warning\|rejected, resetsAt (epoch s), rateLimitType, utilization?, overageStatus, overageDisabledReason, isUsingOverage, surpassedThreshold?, unifiedWindows≠{five_hour{utilization 0..1, resetsAt}, seven_day{…}}}` | V |
| `command_lifecycle` | `command_uuid` (your user `uuid`), `state: queued\|started\|completed\|cancelled`. Capability `msg_lifecycle_v1`. | ≠ V |
| `tool_progress` | `tool_use_id, tool_name, parent_tool_use_id, elapsed_time_seconds, task_id?, heartbeat?, subagent_type?` | T |
| `tool_use_summary` | `summary, preceding_tool_use_ids[]` | T |
| `prompt_suggestion` | `suggestion`. **Never emitted** here, with either `--prompt-suggestions` or `initialize.promptSuggestions:true`; the binary generates it after `result`, behind internal gates. | T |
| `auth_status` | `isAuthenticating, output[], error?` | T |
| `conversation_reset` | `new_conversation_id, trigger: clear\|plan_mode_exit\|fresh_session\|onboarding`: start a fresh transcript view | T |
| `active_goal` | `value{condition, iterations, …} \| null` | T |
| `keep_alive` | Ignore | T |
| `control_request` / `control_response` / `control_cancel_request` | §7 | V |

---------------------------------------------------------------------------------------------------

## 6. Content blocks

| Block | Shape / notes |
|---|---|
| text | `{type:"text", text, citations?}` [V] |
| thinking | `{type:"thinking", thinking, signature}`. **By default `thinking` is `""`** (signature only); stream events carry `thinking_display:"updates"` and progress comes as `thinking_tokens`. `--thinking-display summarized` gives readable summarized text deltas [V m_image]. |
| redacted_thinking | `{type:"redacted_thinking", data}` [T] |
| tool_use | `{type:"tool_use", id, name, input, caller:{type:"direct"}≠}`; the input streams as `input_json_delta` [V] |
| tool_result | `{type:"tool_result", tool_use_id, content: string \| [text\|image blocks], is_error?}`. An image result is `content:[{type:"image",source:{type:"base64",media_type:"image/png",data}}]` [V m2_read_image]. |
| server_tool_use / web_search_tool_result | Not seen on the main stream. WebSearch is a client tool, and its `srvtoolu_…` results sit inside `tool_use_result` [V r_web]. |

---------------------------------------------------------------------------------------------------

## 7. Control protocol

### 7.1 Envelopes [V]

```
request : {"type":"control_request","request_id":"<sender-unique>","request":{"subtype":"…",…}}
success : {"type":"control_response","response":{"subtype":"success","request_id":"…","response":{…}}}
error   : {"type":"control_response","response":{"subtype":"error","request_id":"…","error":"text","error_code":"…"?}}
cancel  : {"type":"control_cancel_request","request_id":"<id of a request the SENDER issued>"}   (no reply)
```

- The CLI uses UUIDs for its own request ids.
- An unknown subtype returns `error:"Unsupported control request subtype: <x>"` [V].
- Replies can arrive interleaved with chat messages.

### 7.2 Host → CLI

| subtype | Request fields | Success `response` / notes | Status |
|---|---|---|---|
| `initialize` | §2 | §2 | V |
| `interrupt` | `cancel_queued?` | `{still_queued:[uuid], cancelled?:[uuid]}`, followed by `assistant{aborted:true}`, the interrupt marker and `result error_during_execution` | V f, f2 |
| `set_permission_mode` | `mode` | `{mode}`, plus `system/status{permissionMode}` if it changed. `manual` becomes `default`. Errors: `bypass_not_launched` (bypassPermissions without the launch flag), `auto_mode_model` (auto on haiku). | V k, k2 |
| `set_model` | `model`: alias, id, `"default"` or `null` | `{}`. An unknown model returns `error_code:"catalog_unknown"`. | V |
| `set_max_thinking_tokens` | `max_thinking_tokens: int\|null`, `thinking_display?` | `{}` | V |
| `apply_flag_settings` | `settings{…}`, e.g. `{effortLevel:"high"}`, `{model}`; `null` clears a key | `{}`. This is the mid-session effort switch (`effortLevel` and `null` verified without a turn). | V |
| `update_settings` | `source: localSettings\|userSettings`, `settings` (allowlist: `outputStyle` / `effortLevel`) | `{}` | T |
| `get_settings` | — | Effective and per-source settings. Not recorded because it contains user secrets. | T |
| `list_models` | — | `{models: ModelInfo[]}` | V |
| `get_usage` | `skip_behaviors?` (`true` skips a 7-day transcript scan) | §12. **The first call right after spawn returned `rate_limits:null`**; it was filled in 3 s later. | V k, k2 |
| `get_context_usage` | `detail?: "summary"\|"full"` (`full`, the default, calls the count-tokens API) | §12 | V |
| `get_session_cost` | — | `{text}` (preformatted) | V |
| `get_binary_version` | — | `{version:"2.1.285", buildTime}` | V |
| `file_suggestions` | `query` (partial path; `""` lists the top level) | `{suggestions:[{path}], cwd}`. **The first query after spawn returns `[]` while the index warms.** Directories end in `/`. Results include paths from additional dirs such as `~/.claude/...`. | V k2 |
| `read_file` | `path`, `max_bytes?`, `encoding?: "utf-8"\|"base64"` | `{contents, absPath, truncated?, encoding?}` | V |
| `rename_session` | `title`, `source?: "host"\|"remote"`, `session_id?` | `{}`, plus `system/session_title_changed` | V |
| `mcp_status` | — | `{mcpServers: McpServerStatus[]}`. Returned `[]` right after init even with an sdk server; use `init.mcp_servers`. | V |
| `mcp_toggle` / `mcp_reconnect` | `serverName` (+ `enabled`) | `{}`. An unknown server returns error `"Server not found: x"`. | V (error) |
| `mcp_set_servers` | `servers{name: config}` | `{added, removed, errors}` | T |
| `mcp_read_resource` | `serverName`, `uri` (`ui://` only) | `{contents[{uri,mimeType?,text?,blob?,_meta?}]}` | T |
| `mcp_call` | `tool`, `arguments`, … | Runs an MCP tool without a model turn and without a permission check (mostly internal) | T |
| `cancel_async_message` | `message_uuid` | `{cancelled: bool}` | V o |
| `stop_task` | `task_id` | `{}`. Returns success even for an unknown id. | V |
| `background_tasks` | `tool_use_id?` | `{}`. The Ctrl+B equivalent: moves foreground Bash or agents to the background. | V (empty) |
| `get_task_output` | `task_id` | `{output, total_bytes, truncated}` | **≠ unsupported on 2.1.285** |
| `rewind_files` | `user_message_id` (your user `uuid`), `dry_run?` | `{canRewind, error?, filesChanged?, insertions?, deletions?}`. Needs checkpointing (§1.3). | V (disabled) |
| `reload_skills` | — | `{skills: SlashCommand[]}`, plus `commands_changed` | V |
| `reload_plugins` / `reload_output_styles` | `hold_on_cache_impact?` / — | `{commands, agents, plugins, mcpServers, error_count, held?, cache_impact?}` / `{available_output_styles}` | T |
| `list_permission_rules` | — | `{state:{rules[{behavior, source, rule, description{prefix,emphasis,suffix}, editability}], workspaceDirectories[{path,source}], originalCwd, managedOnly}}` | V e_bash_rule |
| `get_hooks_listing`, `seed_read_state{path,mtime}`, `register_repo_root{directory,…}`, `set_color{color}` | — | Typed but untested | T |
| `side_question`, `generate_session_title`, `export_conversation`, `get_status`, `get_plan`, `claude_authenticate…`, `remote_control`, `set_mcp_permission_mode_override`, `set_prompt_suggestions_paused` | — | **Internal / experimental**: present in `sdk.mjs` but not in the public `.d.ts` | T |

### 7.3 CLI → host

**`can_use_tool`**: see §8.

**`hook_callback`** [V p_hooks]

Request: `{callback_id:"hook_0", input, tool_use_id?}`. `input` always has `session_id, transcript_path, cwd, prompt_id, permission_mode, hook_event_name`. PreToolUse adds `tool_name, tool_input, tool_use_id`; PostToolUse adds the same plus `tool_response` (the tool's output object) and `duration_ms`.

Reply with a HookJSONOutput:
- `{}` lets the call continue.
- `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow"|"deny"|"ask","permissionDecisionReason":"…"}}` decides the permission. **`allow` skips `can_use_tool` entirely** [V].
- Generic keys [T]: `continue?`, `stopReason?`, `suppressOutput?`, `systemMessage?`, `decision?:"block"`, `reason?`.

Events [T]: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Notification`, `UserPromptSubmit`, `SessionStart`, `SessionEnd`, `Stop`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact`, `PermissionRequest`, `PermissionDenied`, and others.

**`mcp_message`**: in-process ("sdk") MCP servers [V q_sdk_mcp]

Request: `{server_name, message:<JSON-RPC>}`. For a request, reply `{"mcp_response":{"jsonrpc":"2.0","id":<same>,"result":{…}}}`, or put a JSON-RPC `error` in place of `result`. For a notification, reply `{"mcp_response":{"jsonrpc":"2.0","result":{},"id":0}}`.

Sequence observed:
1. Right after `initialize`, before its reply: MCP `initialize` (protocolVersion `"2025-11-25"`, clientInfo `claude-code`).
2. `notifications/initialized`, then `tools/list`.
3. `system/init` shows `mcp_servers:[{name:"flux",status:"connected",source:"sdk"}]` and the tool `mcp__flux__open_in_editor`.
4. On use: `can_use_tool` with `mcp_server{name,source:"sdk"}` and `display_name:"Open In Editor"`, then `tools/call {name, arguments, _meta:{"claudecode/toolUseId", progressToken}}`.
5. The host's `result.content` becomes both the tool_result and the `tool_use_result`.

This is **the clean way to give Claude IDE tools** such as open-in-editor or diagnostics. Server-initiated messages travel host→CLI as `mcp_message` [T].

**`elicitation`** [T]: request `{mcp_server_name, message, mode?:"form"|"url", url?, elicitation_id?, requested_schema?, title?, display_name?, description?}`; reply with an MCP ElicitResult, `{action:"accept"|"decline"|"cancel", content?}`. The SDK's default is `decline`.

**`request_user_dialog`** [T]: request `{dialog_kind, payload, tool_use_id?}`, sent only for kinds declared in `initialize.supportedDialogKinds` (the only known kind is `refusal_fallback_prompt`); reply `{behavior:"completed", result}` or `{behavior:"cancelled"}`.

**Never sent to a plain stdio host** [T]: `oauth_token_refresh`, `host_auth_token_refresh`, `remote_*`, `ui_*`. Answer any subtype you do not know with an error.

---------------------------------------------------------------------------------------------------

## 8. Permissions

### 8.1 Modes

| Mode | Behaviour | Status |
|---|---|---|
| `default` (= `manual`) | Asks via `can_use_tool` for edits and writes, non-read-only Bash, WebFetch/WebSearch, MCP tools, and paths outside the working dirs. Read, read-only Bash (`pwd`) and the Task*/Todo tools run without asking. | V |
| `acceptEdits` | Edits and filesystem Bash (e.g. `touch`) inside the working dirs are approved automatically | V e_bash_all_suggestions |
| `plan` | Read-only exploration. Writing the plan file is allowed without asking; `ExitPlanMode` asks. | V i_plan |
| `dontAsk` | Anything that would ask is denied, with `system/permission_denied` and an explanatory tool_result | V d2 |
| `auto` | A model classifier decides. Unavailable on haiku (`auto_mode_model`). | T |
| `bypassPermissions` | No checks. Requires `--dangerously-skip-permissions` or `--allow-dangerously-skip-permissions` at launch. | V (error) |

### 8.2 `can_use_tool` request [V]

```
{subtype:"can_use_tool", tool_name, display_name, input, tool_use_id, description?, permission_suggestions?,
 blocked_path?, decision_reason?, decision_reason_type?, requires_user_interaction?, mcp_server?{name,source},
 agent_id?, title?, classifier_approvable?, suppress_always_allow_rule?, default_to_no?, matched_ask_rule?}
```

- **`description`** is a ready-made label: the basename for Edit/Write, the model's `input.description` for Bash, the URL for WebFetch, the query for WebSearch, the path for Read.
- **`requires_user_interaction:true`** (AskUserQuestion, ExitPlanMode): render the tool's own UI, not Approve/Deny.
- **`decision_reason_type`** is one of: `rule`, `mode`, `subcommandResults`, `permissionPromptTool`, `hook`, `asyncAgent`, `sandboxOverride`, `workingDir` [V], `safetyCheck`, `classifier`, `other`.
- **`default_to_no`**: do not pre-select Approve. **`suppress_always_allow_rule`**: hide "always allow".

**Suggestions observed [V]:**

| Tool | `permission_suggestions` |
|---|---|
| Edit, Write | `[{type:"setMode",mode:"acceptEdits",destination:"session"}]` |
| Bash `touch f` | `[{type:"addRules",rules:[{toolName:"Bash",ruleContent:"touch f"}],behavior:"allow",destination:"localSettings"}, {type:"addDirectories",directories:[cwd],destination:"session"}, {type:"setMode",mode:"acceptEdits",destination:"session"}]`, plus `blocked_path` |
| WebFetch | `addRules [{toolName:"WebFetch",ruleContent:"domain:example.com"}]` → `localSettings` |
| WebSearch | `addRules [{toolName:"WebSearch"}]` → `localSettings` |
| MCP | `addRules [{toolName:"mcp__flux__open_in_editor"}]` → `localSettings` |
| Read outside cwd | `addRules [{toolName:"Read",ruleContent:"//abs/dir/**"}]` → `session`, with `decision_reason:"Path is outside allowed working directories"` |

### 8.3 Reply (the `response` of a success control_response)

```
allow: {"behavior":"allow","updatedInput":{…},"updatedPermissions":[PermissionUpdate…]?,
        "decisionClassification":"user_temporary"|"user_permanent"|"user_reject"?}
deny : {"behavior":"deny","message":"shown to the model","interrupt":true?}
```

- The SDK passes the result through unchanged, adding only `"toolUseID":<tool_use_id>`.
- **Always send `updatedInput`**, using the original input when it is unchanged. Every recording did this; omitting it was not tested.
- `interrupt:true` on a deny also aborts the turn [T].

**`PermissionUpdate`** [T; `addRules` and `setMode` V]:
- `{type:"addRules"|"replaceRules"|"removeRules", rules:[{toolName, ruleContent?}], behavior:"allow"|"deny"|"ask", destination}`
- `{type:"setMode", mode, destination}`
- `{type:"addDirectories"|"removeDirectories", directories:[…], destination}`

**`destination`:**

| Value | Where the rule lives |
|---|---|
| `session` | Memory only |
| `localSettings` | `<cwd>/.claude/settings.local.json`; [V] it was written as `{"permissions":{"allow":["Bash(touch flux_probe.txt)"]}}` |
| `projectSettings` | `.claude/settings.json` |
| `userSettings` | `~/.claude/settings.json` |
| `cliArg` | — |

Rule syntax, as listed by `list_permission_rules`: `Bash(touch:*)` (prefix match), `WebFetch(domain:x)`, `Read(//abs/**)`, or a bare tool name.

### 8.4 Observed behaviours [V]

- **Allow unchanged** (b_edit_allow). The tool runs. The tool_result reads `"The file … has been updated successfully. (file state is current in your context — no need to Read it back)"`, and `tool_use_result` holds the full Edit output.
- **Allow with a modified `updatedInput`** (c_edit_modified):
  - The CLI runs the host's input as given: the file got "green" where the model wrote "blue". `tool_use_result.newString` and `structuredPatch` show the modified edit, with `userModified:false`.
  - **The model is not told.** Its tool_result is the generic success line, and it went on to claim the line was `color = blue`. The assistant `tool_use.input` keeps the original.
  - So: render from `tool_use_result`, and if you edit a proposal, tell Claude.
  - **Telling Claude works through a PostToolUse SDK hook** [V, 2026-10-09]: register `{"PostToolUse":[{"matcher":"Edit|MultiEdit|Write","hookCallbackIds":["flux-edit"]}]}` in `initialize`; the CLI then sends `hook_callback {callback_id:"flux-edit", input:{hook_event_name:"PostToolUse", tool_use_id, tool_name, tool_input, tool_response…}}` after each edit, and the answer `{"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"<what the file really holds>"}}` reaches the model (asked for the line afterwards, it reported the user's `color = green`, not its own `blue`). Answer `{}` when nothing changed. Flux: `Session::hook_response`.
- **Deny with a message** (d_write_deny): tool_result `{is_error:true, content:"<your message>"}`, `tool_use_result:"Error: <your message>"`, `tool_result_meta:[{non_execution_kind:"permission-rule"}]`. The model continues the same turn (it explained and did not retry), and the call appears in `result.permission_denials`. No `system/permission_denied` is sent.
- **Always-allow** (e_bash_rule): the reply carried `updatedPermissions:[{type:"addRules",rules:[{toolName:"Bash",ruleContent:"touch:*"}],behavior:"allow",destination:"session"}]`. The next `touch` ran without a request, and `list_permission_rules` shows `{source:"session", rule:"Bash(touch:*)", editability:"session"}`.
- **Echoing all suggestions back** (e_bash_all_suggestions) writes an exact-command rule to settings.local.json, adds the directory, and applies `setMode acceptEdits`. The mode change arrives as `system/status{permissionMode:"acceptEdits"}` and shows in later `init` frames.
- **Interrupt while a prompt is pending** (f2):
  1. `control_cancel_request{request_id}`, then `control_response{still_queued:[]}`.
  2. tool_result `is_error`: "The user doesn't want to proceed with this tool use. The tool use was rejected…", with `non_execution_kind:"user-rejected"`, then the text "[Request interrupted by user for tool use]".
  3. `result error_during_execution` with `terminal_reason:"aborted_tools"`.
- **Session state while a prompt is pending.** `session_state_changed` goes to `requires_action` and back to `running` after the answer [V j_background].

---------------------------------------------------------------------------------------------------

## 9. Tool contracts (input → `tool_use_result`)

**Default tools on 2.1.285** (`init.tools`) [V]: Task (= Agent), AskUserQuestion, Bash, CronCreate, CronDelete, CronList, DesignSync, Edit, EnterPlanMode, EnterWorktree, ExitPlanMode, ExitWorktree, ListAgents, LSP, Monitor, NotebookEdit, PushNotification, Read, ReportFindings, ScheduleWakeup, SendMessage, ShareOnboardingGuide, Skill, TaskCreate, TaskGet, TaskList, TaskStop, TaskUpdate, ToolSearch, WebFetch, WebSearch, Write.

- **Absent:** Glob and Grep (search goes through Bash), TodoWrite, MultiEdit, BashOutput, KillShell.
- **Deferred schemas:** some tools are loaded on demand via ToolSearch ("System tools (deferred)" in context usage).
- **Legacy aliases** (sdk.mjs): Task→Agent, KillShell/KillBash→TaskStop, ListPeers→ListAgents, Brief→SendUserMessage.

| Tool | Input | `tool_use_result` | Status |
|---|---|---|---|
| **Edit** | `{file_path, old_string, new_string, replace_all?}` | `{filePath, oldString, newString, originalFile, structuredPatch:[{oldStart,oldLines,newStart,newLines,lines:[" ctx","-old","+new"]}], userModified, replaceAll, gitDiff?, staged?}` | V b, c |
| **Write** | `{file_path, content}` | `{type:"create"\|"update", filePath, content, structuredPatch, originalFile:null\|string, userModified?, gitDiff?}` | V i_plan |
| **Read** | `{file_path, offset?, limit?, pages?}` | `{type:"text", file:{filePath, content, numLines, startLine, totalLines}}` [V]<br>`{type:"image", file:{base64, type, originalSize, dimensions{…}}}` [V m2]<br>`type: "notebook"\|"pdf"\|"parts"\|"file_unchanged"` [T]<br>The tool_result text has `N\t` line prefixes. | V |
| **Bash** | `{command, description?, timeout?, run_in_background?, dangerouslyDisableSandbox?}` | `{stdout, stderr, interrupted, isImage, noOutputExpected, backgroundTaskId?, returnCodeInterpretation?, persistedOutputPath?, gitOperation?…}`. **No exit-code field.** Empty output appears as `"(Bash completed with no output)"`. | V e, j2 |
| **TaskCreate** | `{subject, description, activeForm?, metadata?}` | `{task:{id, subject}}`; text: `"Task #1 created successfully: …"` | V g_tasks |
| **TaskUpdate** | `{taskId, status?: pending\|in_progress\|completed\|deleted, subject?, description?, activeForm?, addBlocks?, addBlockedBy?, owner?}` | `{success, taskId, updatedFields[], statusChange?{from,to}}` | V |
| **TaskList / TaskGet** | `{}` / `{taskId}` | `{tasks[{id,subject,status,owner?,blockedBy}]}` / `{task{…,blocks,blockedBy}\|null}` | T |
| **TodoWrite** (legacy, `CLAUDE_CODE_ENABLE_TASKS=false`) | `{todos:[{content, status: pending\|in_progress\|completed, activeForm}]}`, the full list every time | `{oldTodos[], newTodos[]}` | V g_todowrite_legacy |
| **Agent** (listed as "Task") | `{description, prompt, subagent_type?, model?: sonnet\|opus\|haiku\|fable, run_in_background?, effort?, name?, isolation?}` | `{status:"completed", agentId, agentType, content:[text], totalDurationMs, totalTokens, totalToolUseCount, usage, toolStats{readCount,searchCount,bashCount,editFileCount,linesAdded,linesRemoved,otherToolCount}, resolvedModel, prompt}`<br>or `{status:"async_launched", isAsync, agentId, description, prompt, outputFile, canReadOutputFile, resolvedModel}` (§10) | V j, j2 |
| **AskUserQuestion** | `{questions:[{question, header (≤12 chars), options:[{label, description, preview?}] (2–4, no "Other"), multiSelect}] (1–4)}` | `{questions, answers:{[question]: label}, annotations?, response?}` | V h_ask |
| **ExitPlanMode** | In `can_use_tool`: `{plan:"<markdown>", planFilePath:"<abs>"}`. The model sent `{}`; the CLI fills both from the plan file. | `{plan, isAgent:false, filePath}` | V i_plan |
| **EnterPlanMode** | `{}` | `{message}` | T |
| **NotebookEdit** | `{notebook_path, new_source, cell_id?, cell_type?: code\|markdown, edit_mode?: replace\|insert\|delete}` | `{new_source, old_source?, cell_id?, cell_type, language, edit_mode, error?, notebook_path, original_file, updated_file}` | T |
| **WebFetch** | `{url, prompt}` | `{bytes, code, codeText, result (a small model's answer to `prompt`), durationMs, url}` | V r_web |
| **WebSearch** | `{query, mode:"standard"\|"extended"≠, allowed_domains?, blocked_domains?}` | `{query, results:[{tool_use_id:"srvtoolu_…", content:[{title,url}]} \| "commentary string"], durationSeconds, searchCount?}`; the text starts `"Web search results for query: … Links: [...]"` | V |
| WebFetch / WebSearch, detached | — | `{detachedToolCall:true}` when the call stepped aside for a user message | T |
| **MCP** `mcp__<server>__<tool>` | the server's schema | the MCP `content` array, e.g. `[{type:"text",text}]` | V q |
| **TaskStop** / **Monitor** | `{task_id}` / `{description, timeout_ms, command?\|ws?}` | `{message, task_id, task_type, command?}` / `{taskId, timeoutMs, persistent?}` | T |
| **Skill** / **SendMessage** | `{skill, args?}` / `{to, message, summary?}` | — | T |

### AskUserQuestion: how to answer [V h_ask]

1. The CLI sends `can_use_tool` with `tool_name:"AskUserQuestion"`, `requires_user_interaction:true` and `input.questions`.
2. Reply with `{"behavior":"allow","updatedInput":{...input, "answers":{"Which color do you prefer?":"Green"}}}`.
   - The key is the exact `question` text.
   - The value is the option `label`, or the user's free text for "Other".
   - For multiSelect, join labels with `", "` (per the type doc; not tested).
   - `annotations:{[question]:{notes?, preview?}}` is optional.
3. The model receives the tool_result `Your questions have been answered: "Which color do you prefer?"="Green". You can now continue with these answers in mind.`, and the turn continues.
4. To skip a question, deny with a message; the model sees an error tool_result.

### ExitPlanMode: approve vs. keep planning [V i_plan, i_plan_feedback]

**Plan file.** Before calling ExitPlanMode, Claude writes the plan to `<plansDirectory>/<slug>.md` using Write, which plan mode allows without asking. The default directory is `~/.claude/plans/`; Flux can override it with `--settings '{"plansDirectory":"<dir relative to project root>"}'`.

**Approve:** reply `allow` with `updatedInput:=input`.
- You get `system/status{permissionMode:"default"}`, the mode from before plan mode.
- The tool_result reads `"User has approved your plan. You can now start coding. … Your plan has been saved to: <path> … ## Approved Plan: <plan>"`.
- Claude then implements the plan; edits still ask as usual.

**Approve and auto-accept edits:** the same `allow`, plus `updatedPermissions:[{type:"setMode",mode:"acceptEdits",destination:"session"}]`. You get `permissionMode:"acceptEdits"`, and later Edits ran without `can_use_tool`.

**Keep planning:** reply `deny` with feedback, for example `{"behavior":"deny","message":"Keep planning: add a verification step…"}`.
- The model stays in plan mode, rewrites the plan file (Write with `type:"update"`) and calls ExitPlanMode again.
- The rejected call appears in `result.permission_denials`.

**Other plan inputs.** `allowedPrompts` is deprecated [T]. `initialize.planModeInstructions` replaces the plan workflow text [T].

---------------------------------------------------------------------------------------------------

## 10. Subagents and background tasks

**Foreground subagent** (j_subagent, with `--forward-subagent-text`) [V]:
1. The main assistant sends `tool_use{name:"Agent", input:{subagent_type:"general-purpose", description, prompt, run_in_background:false}}`.
2. `system/task_started{task_id:"a…", tool_use_id, task_type:"local_agent", is_backgrounded:false, spawn_depth:1, prompt}`.
3. Subagent frames follow, each with `parent_tool_use_id` set to the Agent tool_use id, plus `subagent_type` and `task_description` (no `agent_id` on 2.1.285):
   - a `user` message with the prompt;
   - `assistant` blocks (thinking, tool_use, text) and `user` tool_results;
   - **no stream_events**.
   - Permission asks from a subagent carry `agent_id` [T].
4. `task_progress{usage, last_tool_name, description}` → `task_updated{patch:{status:"completed",end_time}}` → `task_notification{status, summary, output_file, usage}`.
5. The main `user` tool_result arrives with `tool_use_result.status:"completed"` (§9).

Without `--forward-subagent-text` (or `initialize.forwardSubagentText`), only the subagent's tool_use and tool_result blocks are forwarded [T].

**Background work** (j_background) [V]:
- **Bash with `run_in_background:true`.** The tool_result reads `"Command running in background with ID: b4e752g0b. Output is being written to: <tmp>/<cwd-slug>/<session>/tasks/b4e752g0b.output …"`, `tool_use_result.backgroundTaskId` is set, and you get `task_started{task_type:"local_bash", is_backgrounded:true}`.
- **Agent with `run_in_background:true`** returns `{status:"async_launched", agentId, outputFile, …}`. Its frames keep arriving after the main turn's `result`.
- **Task set and completion.** `background_tasks_changed` carries the full live set whenever it changes. Each task ends with `task_updated` and `task_notification`; for Bash the summary reads `Background command "…" completed (exit code 0)`.
- **Automatic follow-up turn.** After the notifications **the CLI starts a new turn on its own**: `system/init` → model output → a second `result`, with no host user message and no `command_lifecycle`. `session_state_changed idle` arrives only after this turn.
- **Stopping tasks** [T]. `stop_task{task_id}` stops one task. With `initialize.perTaskStopAffordance:true`, background tasks survive an `interrupt`; otherwise the interrupt kills them.

---------------------------------------------------------------------------------------------------

## 11. Sessions, resume, transcripts

- **Session ids.** Each process gets a new `session_id`, which appears in `init` and on every frame. `--session-id <uuid>` sets it explicitly [V n].
- **`--resume <id>`** keeps the same `session_id`, and the model remembers the history [V n_resume_2]. **Nothing is replayed on stdout**, and not even `init` arrives before the first user message, so the host has to render history from its own store or from the transcript.
- **Other resume flags** [T]: `--continue` (the most recent session in the cwd), `--fork-session` (a new id with a copy of the history), `--resume-session-at <message uuid>` (resume from that point).
- **Transcript path.** `~/.claude/projects/<slug>/<session_id>.jsonl`, where `<slug>` is the absolute cwd with every non-alphanumeric character replaced by `-`. Example: `/Users/me/dev/my_app` → `-Users-me-dev-my-app`.
- **Subagent transcripts** live in `<slug>/<session_id>/subagents/agent-<id>.jsonl` plus `.meta.json` [V meta].

**Transcript lines** [V]. One JSON object per line; `type` is one of:

| type | Contents |
|---|---|
| `user` | `uuid, parentUuid, isSidechain, promptId, promptSource, permissionMode, cwd, gitBranch, version, entrypoint, userType, sessionId, timestamp, turnOrigin, turnPosition, message` |
| `assistant` | One content block per line: `message, requestId, apiBlockIndex, perTurnEffort, parentUuid…` |
| `attachment` | Context injected into the turn: `attachment, rendered?, renderedRole?` |
| bookkeeping | `queue-operation`, `last-prompt{lastPrompt,leafUuid}`, `ai-title{aiTitle}`, `atis-latch`, `mode{mode}`, `cost-state{totalCostUSD,modelUsage,…}` |

To rebuild the chat, follow `parentUuid` back from the leaf, skipping `isSidechain` and bookkeeping lines. The SDK's `listSessions`/`getSessionMessages` read these files directly; they are not control requests [T].

---------------------------------------------------------------------------------------------------

## 12. Usage, cost, context, rate limits

**`rate_limit_event`** [V] arrives after API calls.
- Windows: `rate_limit_info.unifiedWindows.{five_hour,seven_day}`. `utilization` is a **0..1 fraction** and `resetsAt` is in **epoch seconds**.
- `status` is `allowed`, `allowed_warning` or `rejected`, and `rateLimitType` names the binding window.

**`get_usage`** [V; the SDK labels it experimental]:

```
{session:{total_cost_usd,total_api_duration_ms,total_duration_ms,total_lines_added,total_lines_removed,model_usage},
 subscription_type:"team", rate_limits_available:true,
 rate_limits:{five_hour:{utilization:10 /*0..100*/, resets_at:"ISO", limit_dollars,used_dollars,remaining_dollars,locked_reason},
              seven_day:{…}, seven_day_opus|_sonnet|_oauth_apps:null, <many codename keys>:null,
              extra_usage:{is_enabled, monthly_limit, used_credits, utilization, currency, …},
              limits:[{kind:"session"|"weekly_all"|"weekly_scoped", group:"session"|"weekly", percent, severity:"normal"…,
                       resets_at, scope{model?{display_name}}|null, is_active}]},
 behaviors: {day|week:{request_count, session_count, behaviors[{key,pct,count}], agents[], skills[], plugins[], mcp_servers[]}} | null}
```

- Render `limits[]` exactly as given: these are the server's rows, the same ones `/usage` shows.
- Use `skip_behaviors:true` for a meter.
- If you get `rate_limits:null` right after spawn, retry (Flux asks up to 4 times, 3 s apart).

**`get_context_usage`** [V]:

```
{categories:[{name,tokens,color,kind:"used"|"free"|"buffer"|"deferred",isDeferred?}], totalTokens, maxTokens,
 rawMaxTokens, percentage, autocompactSource, autoCompactThreshold, isAutoCompactEnabled, model, gridRows[10][10]
 {color,isFilled,categoryName,tokens,percentage,squareFullness}, memoryFiles[{path,type,tokens}],
 mcpTools[{name,serverName,tokens,isLoaded?}], agents[{agentType,source,tokens}], slashCommands{…}, skills{…},
 messageBreakdown{toolCallTokens,toolResultTokens,…,toolCallsByType[]}, apiUsage{input_tokens,…}|null}
```

- It works without a turn, so it can drive a "28.9k / 200k" meter.
- Classify rows by `kind`, never by `name`.
- `detail:"summary"` avoids the count-tokens calls.

**Other sources.** `result.total_cost_usd` is cumulative and `result.modelUsage[model].contextWindow` gives the window size. `get_session_cost.text` is a preformatted summary. `/usage` produces a synthetic assistant message carrying `usage_report` [V].

---------------------------------------------------------------------------------------------------

## 13. Models, effort, thinking

**Models from `list_models` on this account** [V]:
- Aliases: `default` and `opus` → `claude-opus-5-5`; `sonnet` → `claude-sonnet-5-5`; `haiku` → `claude-haiku-4-5-20251001`. The CLI help also mentions `fable`.
- Explicit ids: `claude-fable-5-1`, `claude-sonnet-5`, `claude-opus-5`, `claude-fable-5`, `claude-opus-4-8`, `-4-7`, `-4-6`, `claude-sonnet-4-6`. `claude-haiku-5-5` comes back with `disabled:true`≠ ("Update Claude Code to use this model").

**`ModelInfo`:** `{value, resolvedModel, displayName, description, supportsEffort?, supportedEffortLevels?, supportsAdaptiveThinking?, supportsFastMode?, supportsAutoMode?, disabled?}`.

**Effort** is `low|medium|high|xhigh|max`, limited per model by `supportedEffortLevels`.
- Set it with `--effort` at launch.
- Change it mid-session with `apply_flag_settings{settings:{effortLevel}}` [T] or `/effort`.
- `init.effort` reports the level in effect; it is absent for models without effort support [V].

**Thinking.**
- Launch flags: `--thinking adaptive|disabled`, `--max-thinking-tokens N`, `--thinking-display summarized|omitted`.
- At runtime: `set_max_thinking_tokens` [V ack].
- The 2.1.285 default display shows no thinking text (§6).

**Model switch.** `set_model{model}` applies to later turns [V ack]; the next `system/init.model` shows it [T].

---------------------------------------------------------------------------------------------------

## 14. Version compatibility

**Version sources** [V]:
- `get_binary_version` returns `{version:"2.1.285", buildTime:"2026-09-29T01:34:53Z"}` and needs no turn.
- `system/init.claude_code_version`.
- `system/init.capabilities`: `interrupt_receipt_v1`, `interrupt_cancel_queued_v1`, `msg_lifecycle_v1`, `mcp_read_resource_v1`, `mcp_tool_ui_meta_v1`. The types also mention `queued_notifications`.

**No protocol version.** `initialize` carries none, and the SDK never checks the CLI version. Its manifest pins `claudeCodeVersion 2.1.295` and lists `sdkCompat.testedWrapperVersions`, but nothing enforces them [T]. Newer `initialize` fields are simply ignored by older CLIs.

**Skew between 2.1.285 and the 2.1.295 types:**
- `get_task_output` is unsupported.
- Emitted by the CLI but missing from the types: frames `command_lifecycle` and `system/session_title_changed`; message fields `wire_tool_inputs`, `tool_result_meta`, `local_command_run`, `subagent_stats`, `tool_use.caller`, `stream_event.thinking_display`; response fields `rate_limit_info.unifiedWindows`, the `get_usage` `limits[]` and codename keys, `ModelInfo.disabled`.
- The types expect `system/local_command_output`; the CLI sends a synthetic assistant message instead.

**Strategy.** Parse permissively and keep unknown frames as raw JSON in a debug pane. Feature-detect with `capabilities`. Treat an "Unsupported control request subtype" error as "feature absent". Pin and test the Homebrew CLI version you support.

---------------------------------------------------------------------------------------------------

## 15. Verified vs. unverified

**Verified by recording:**
- **Session and stream:** initialize request and response; stream_event text, thinking and tool-input deltas; per-block assistant messages; `result` success and error.
- **System and event frames:** `system` init, status, thinking_tokens, compact_boundary, task_* (agent and bash), background_tasks_changed, session_state_changed, permission_denied, commands_changed and session_title_changed; rate_limit_event; command_lifecycle.
- **Permissions:** `can_use_tool` for Edit, Write, Bash, WebFetch, WebSearch, MCP, AskUserQuestion, ExitPlanMode and out-of-cwd Read; allow, allow with modified input, deny with message; `updatedPermissions` (addRules to session and localSettings, setMode, addDirectories).
- **Interrupts:** interrupt during streaming and while a prompt is pending; `control_cancel_request` from the CLI.
- **Callbacks:** `hook_callback` (PreToolUse, PostToolUse); `mcp_message` for an in-process server.
- **Host requests:** list_models, get_usage, get_context_usage, get_session_cost, get_binary_version, file_suggestions, read_file, set_model, set_permission_mode (every mode and its errors), set_max_thinking_tokens, mcp_status, list_permission_rules, rename_session, cancel_async_message, stop_task, background_tasks, reload_skills, rewind_files (disabled), and error replies.
- **Input:** images; slash commands (/usage, /context, /compact, unknown); @file; queueing, `priority:"now"` and cancel; `--replay-user-messages`.
- **Sessions:** `--resume` and `--session-id`; the transcript layout.
- **Tools:** AskUserQuestion, ExitPlanMode (approve, approve + acceptEdits, keep planning), TaskCreate/TaskUpdate, legacy TodoWrite, Agent (foreground and background), background Bash, WebFetch, WebSearch, image Read.

**From types or JS only:**
- **Never seen on the wire:** prompt_suggestion (enabled, never received), api_retry, notification, informational, local_command_output, hook_* events, model_refusal_*, auth_status, conversation_reset, tool_progress, tool_use_summary, keep_alive, redacted_thinking; the CLI requests elicitation and request_user_dialog.
- **Untested host requests:** apply_flag_settings, update_settings, get_settings, mcp_set_servers, mcp_call, mcp_read_resource, reload_plugins, reload_output_styles, get_hooks_listing, seed_read_state, register_repo_root, set_color, and a working get_task_output.
- **Untested behaviours:** deny with `interrupt:true`, `shouldQuery:false`, `client_composed`, folding of queued messages, `--continue`, `--fork-session`, `--resume-session-at`, checkpointed rewind, `perTaskStopAffordance` semantics, `auto` mode.
- **Tool contracts:** NotebookEdit, Monitor, TaskStop, EnterPlanMode, Skill, SendMessage, and the AskUserQuestion multiSelect separator.

**Differs from the types:** see the skew list in §14.

---------------------------------------------------------------------------------------------------

## 16. Fixture index (`fixtures/`, plus a `.stdin.jsonl` for each)

| Fixture | Scenario |
|---|---|
| `a_text` | Plain answer with partial messages; `--prompt-suggestions` (no suggestion arrived) |
| `b_edit_allow` | Read → Edit → can_use_tool → allow unchanged |
| `c_edit_modified` | Edit allowed with a modified `new_string`: the file holds the host's text and the model isn't told |
| `d_write_deny` / `d2_dontask` | Write denied with a message / `--permission-mode dontAsk` auto-deny → `system/permission_denied` |
| `e_bash_all_suggestions` | `touch` allowed with every suggestion echoed back (localSettings rule, addDirectories, setMode); read-only `pwd` not asked |
| `e_bash_rule` | `touch` allowed with the session rule `Bash(touch:*)`; the second touch is not asked; `list_permission_rules` |
| `f_interrupt` / `f2_interrupt_pending_permission` | Interrupt mid-stream, then a follow-up turn / interrupt while `can_use_tool` is unanswered → `control_cancel_request` |
| `g_tasks` / `g_todowrite_legacy` / `g_todowrite_unavailable` | TaskCreate ×3 + TaskUpdate / TodoWrite via the env toggle / `--tools TodoWrite` on the default build → no tools (the model fakes a call in text) |
| `h_ask` | AskUserQuestion answered through `updatedInput.answers` |
| `i_plan` / `i_plan_feedback` | Plan file → ExitPlanMode approved → Edit asks / denied (keep planning), then approved with setMode acceptEdits |
| `j_subagent` / `j_background` | Foreground Agent with `--forward-subagent-text` / background Bash and Agent, automatic follow-up turn, idle |
| `k_control`, `k2_control` | Control requests without a turn, plus error cases (the second pass shows file_suggestions and get_usage after warm-up) |
| `l_slash` | `/cost`, `/context`, an unknown command, `@notes.txt`, `/compact`, `/cost`, with `--replay-user-messages` |
| `m_image` / `m2_read_image` | Image block in the user message with `--thinking-display summarized` / Read of a PNG → image tool_result |
| `n_resume_1_create`, `n_resume_2_resume` | Persisted session with `--session-id`, then `--resume` (nothing replayed) |
| `o_queue` | Queued message, `priority:"now"`, `cancel_async_message`, `initialize.promptSuggestions` |
| `p_hooks` / `q_sdk_mcp` / `r_web` | Callback hooks (a PreToolUse allow skips can_use_tool) / in-process MCP server over `mcp_message` / WebFetch and WebSearch |

**Privacy.** The fixtures contain account data: the email and organization in initialize responses, the user's permission rules in `k_control`, local paths and usage numbers. Redact them before committing.
