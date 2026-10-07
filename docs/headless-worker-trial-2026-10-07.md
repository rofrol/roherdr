# Headless Claude worker over stream-json: trial, 2026-10-07

Context: `TODO.md`, "Compare the T3 Code approach" and "Experiment: one
headless Claude worker". The user chose plain
`claude -p --input-format stream-json --output-format stream-json` on his own
Claude Code login, with no SDK.

Setup: Claude Code 2.1.292, model `sonnet` (`claude-sonnet-5-5`) on the user's
claude.ai login (`apiKeySource: "none"` in `system/init`). The driver is
`scripts/headless_worker_trial.py`. It runs the CLI as a plain subprocess with
pipes (no PTY), one throwaway git repo and one fresh session per case under
`$TMPDIR`. It removes `HERDR_*` from the child's environment, so the worker's
hooks do not report to the pane that runs the trial. It journals every stdin and
stdout line with a timestamp to `<case>.jsonl`. Every case ends on an event
(`result`, EOF, process exit). The long-running tool is controlled by FIFOs, not
timers. Fixed flags: `--verbose --replay-user-messages --max-turns 8`.

The report marks each fact as **Observed** (seen in this trial's journals) or
**Read** (taken from the installed binary's bundled JSON schemas, found with
`strings ~/.local/bin/claude`). I found no public docs page for the control
protocol. The shapes below come from the binary.

Cost: 7 headless sessions plus 2 discarded runs (the first interrupt and crash
attempts, see case 4), each $0.06–0.10 at list price on the subscription. One
interactive `--resume`, closed right after it opened.

## Message shapes (Read, then Observed working)

- User message on stdin:
  `{"type":"user","message":{"role":"user","content":"..."}}`.
- Permission request on stdout (only with `--permission-prompt-tool stdio`,
  the flag the Agent SDK passes too):
  `{"type":"control_request","request_id":"<uuid>","request":{"subtype":"can_use_tool","tool_name","display_name","input","tool_use_id","description","permission_suggestions","blocked_path",...}}`.
  The schema also has `decision_reason`, `decision_reason_type`,
  `requires_user_interaction`, `mcp_server` and `tool_kind`.
- Answer on stdin:
  `{"type":"control_response","response":{"subtype":"success","request_id":"<same>","response":{"behavior":"allow","updatedInput":{...}}}}`,
  or `{"behavior":"deny","message":"..."}`.
- Interrupt on stdin:
  `{"type":"control_request","request_id":"<ours>","request":{"subtype":"interrupt"}}`.
  An optional `reason` field exists (internal).
- Other request subtypes in the schema (Read, not tried): `initialize`,
  `set_permission_mode`, `set_model`, `request_user_dialog` (dialogs a host
  must declare in `supportedDialogKinds`, otherwise the CLI "fails closed"),
  `stop_task`, `get_context_usage` and more.

## Cases

### 1. Normal completion (Observed)

Flags: `--permission-mode acceptEdits --allowedTools "Bash(git add:*)" "Bash(git commit:*)"`.

The host sent the prompt at t=0.003 s, before any output. The event sequence
was: `system/hook_started` ×2, `system/hook_response` ×2 (the user's
SessionStart hooks), `system/init` (t=0.71 s), the replayed `user` message
(`isReplay: true`, t=2.66 s), `assistant` with Bash `tool_use`,
`rate_limit_event`, `system/vcs_state_changed` (`kind: commit`), `user` with
`tool_result`, `assistant` text, `result/success`. The process exited 0 when
the host closed stdin. The commit landed (`add hello`). The model added a
`Co-Authored-By` trailer by itself.

`system/init` keys: `session_id`, `model`, `permissionMode`, `cwd`, `tools`,
`mcp_servers`, `slash_commands`, `skills`, `plugins`, `agents`,
`claude_code_version`, `apiKeySource`, `memory_paths`, `capabilities`,
`messaging_socket_path`, `output_style`, `fast_mode_state` and more.
`system/init` is sent again before every turn (Observed in case 2b), with the
same `session_id`.

`result` fields: `subtype: "success"`, `is_error: false`,
`terminal_reason: "completed"`, `stop_reason: "end_turn"`,
`api_error_status: null`, `num_turns`, `duration_ms`, `duration_api_ms`,
`total_cost_usd`, `usage`, `modelUsage`, `permission_denials: []`,
`queued_turn_count`, `result_index`, `safety_stops`, `subagent_stats`,
`ttft_ms`, `session_id`, `result` (the reply text).

Side effects of the user's global config, all Observed:
- The worker answered in Polish ("Gotowe.") although the prompt asked for
  "done". The user's `~/.claude/CLAUDE.md` loads in print mode too.
- SessionStart hooks ran. In cases 2b and 3 the uncommitted-files Stop hook
  injected `Stop hook feedback: ...` as a new `user` message. A
  `system/notification` (`key: stop-hook-error`) followed, and the model wrote
  one more assistant reply before the `result`. A worker run this way inherits
  every global hook. Its turn can be longer than the prompt asked for, and the
  extra reply arrives before the `result` message.

### 2a. A tool nobody may approve (Observed)

Flags: `--permission-mode manual --permission-prompts none` (the init reports
`permissionMode: "default"`). The prompt asked for `touch created-by-bash.txt`.

The stream showed `assistant` with Bash `tool_use`, then
`system/permission_denied`, then a `user` `tool_result` with `is_error: true`.
The model then said it was refused, and the run ended with `result/success`.
The denial is listed in `result.permission_denials`
(`tool_name`, `tool_use_id`, `tool_input`). No file was created, and the
process did not hang.

### 2b. The host answers permission requests (Observed)

Flags: `--permission-mode manual --permission-prompt-tool stdio`, two turns in
one session.

- Turn 1, `touch first.txt`: a `control_request` arrived with
  `subtype: "can_use_tool"`, `tool_name: "Bash"`, `input`, `tool_use_id`,
  `description`, `blocked_path`, and `permission_suggestions` (add a rule to
  localSettings, add the directory, switch to acceptEdits). The host answered
  `deny` with a message. The tool result was an error, the model replied
  "refused", and the turn ended with `result/success` with the denial in
  `permission_denials`. The file was not created.
- Turn 2, `touch second.txt`: another `can_use_tool` arrived. The host
  answered `allow` with `updatedInput`. The tool ran, the file exists, and the
  result was `result/success`.

So plain stream-json exposes `can_use_tool`, and answering with a
`control_response` works. No `initialize` handshake was needed.

### 3. AskUserQuestion (Observed)

Flags: `--permission-mode acceptEdits --permission-prompt-tool stdio`.

`AskUserQuestion` arrived as a `can_use_tool` control request with
`tool_name: "AskUserQuestion"`, `input.questions[]` (`question`, `header`,
`options[].label/description`, `multiSelect`) and `requires_user_interaction`
among its keys. The host answered `allow` with
`updatedInput = input + {"answers": {"<question>": "beta.txt"}}`. The tool
result carried the answer, the model created `beta.txt` (not `alpha.txt`), and
the run ended with `result/success`. In print mode the question goes to the
host as a permission request, so we can answer it ourselves.

Not tried: what happens with `--permission-prompts none` or without a
permission tool. The 2a behaviour suggests an automatic denial, but that is not
verified.

### 4. Interrupt (Observed)

Flags: `--permission-mode manual --allowedTools "Bash(sh wait-gate.sh)"`.

First attempt: a plain `sleep 41` was refused by Claude Code itself
(`<tool_use_error>Blocked: standalone sleep 41 ...`). The interrupt then
raced a tool that never ran (`terminal_reason: "aborted_streaming"`). The
script was changed so the tool runs `sh wait-gate.sh`, which first writes to
`started.fifo` and then reads `gate.fifo`. The host's read of `started.fifo`
returning is the event "the tool is running".

On that event the host sent `control_request` `interrupt`. Within 2 ms the
host got
`control_response {"subtype":"success","request_id":<ours>,"response":{"still_queued":[]}}`.
Then came a `tool_result` error ("The user doesn't want to proceed with this
tool use..."), a `user` text `[Request interrupted by user for tool use]`, and
`result` with `subtype: "error_during_execution"`, `is_error: true`,
`terminal_reason: "aborted_tools"`, `stop_reason: "tool_use"`. The tool's
`cat gate.fifo` was still alive after the `result`. It was gone after the CLI
exited. After the host closed stdin, the process exited with **code 1**,
probably because the last turn was an error (not verified).

### 5. Crash: SIGKILL of the CLI mid-tool (Observed)

The setup was the same as in case 4. On "the tool is running" the host sent
SIGKILL to the `claude` process only (not its process group). The reader got
EOF right after the last line already written (`rate_limit_event`). There was
no `result` and no partial JSON line, and the exit code was -9. The tool's
`cat gate.fifo` **survived as an orphan** after the CLI died. The script freed
it by opening the gate. A crashed worker can leave its tool processes running,
so a supervisor should start each worker in its own process group (the script
uses `start_new_session=True`) and kill the group after a crash.

### 6. Limits (Observed fields, Read semantics)

A limit was not reproduced. From the normal runs:
- `result.api_error_status: null` and `is_error: false` on success. Per the
  analysis and T3 Code, 429/529 show up in `api_error_status`. The binary also
  has a `system/api_retry` event subtype (Read, not seen).
- New and Observed: each turn emits
  `{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","resetsAt",...,"overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.06,"resetsAt"},"seven_day":{"utilization":0.16,"resetsAt"}}}}`.
  A supervisor gets the subscription's 5-hour and 7-day usage on every turn for
  free, before any 429.

### 7. Takeover with `claude --resume <id>` (Observed)

The host took the session id from case 1's `system/init`
(`188926f5-c959-4792-94ec-b8975e961896`). It created a child tab
(`herdr tab create --cwd <repo> --no-focus`) and ran
`claude --resume 188926f5-...` there. Claude first showed the **folder trust
dialog**, because print mode never asks it and so the folder was never trusted.
After "Yes, I trust this folder" the TUI showed the headless transcript (the
prompt, `Committed c50f26e`, "Gotowe."), with Sonnet 5.5 and auto mode.
`herdr agent list` identified the pane as `claude` with
`agent_session.value = 188926f5-...` and `agent_status: idle`. The tab was then
closed, and no process remained. No new work was done in it.

The tab was not tested while the headless process was alive. "Never two
writers" stays a rule we must enforce.

## Failures 1-8 from the analysis

| # | Failure today (TUI workers) | Headless stream-json | Evidence |
|---|---|---|---|
| 1 | Lost first prompt | **Gone.** The prompt written at t=0 before `system/init` was buffered and processed. `--replay-user-messages` echoes it (`isReplay: true`) as an acknowledgement. | Observed, case 1 |
| 2 | Trust dialog blocks startup | **Gone for the worker, moved to takeover.** `-p` ran in an untrusted folder without asking. `claude --resume` in a tab then asks once. | Observed, cases 1, 7 |
| 3 | State flicker (done/idle while working) | **Gone.** The turn ends exactly at the `result` message. A Stop hook can add a reply before it, still inside the turn. | Observed, cases 1-3 |
| 4 | Stale screen | **Gone.** There is no screen. The state comes from the journal of events. | Observed |
| 5 | Esc without Stop | **Gone.** The interrupt is a control request with an ack and ends in `terminal_reason: aborted_tools`. | Observed, case 4 |
| 6 | One PTY per worker | **Gone for the CLI**, which runs on pipes. Whether its Bash tool allocates PTYs was not measured. | Observed (CLI), not measured (tools) |
| 7 | Permission classifier | **Changed, stays ours.** Asks arrive as `can_use_tool` requests that our host must answer (or deny automatically with `--permission-prompts none`, listed in `permission_denials`). `--permission-mode auto` (the classifier) was not tried headless. | Observed, cases 2a, 2b |
| 8 | Tab ids the user cannot see | **Stays, changed.** A headless worker has no tab, so it needs another identity the user sees (session id or name). It gets a tab only on takeover. | Observed, case 7 |

## New findings for a design

- Strip `HERDR_*` (done in the script) or the worker's hooks will act on the
  supervisor's pane. Decide whether workers should load the user's global
  hooks and CLAUDE.md at all. `--bare` skips hooks, but it may also change auth
  (not checked).
- Run each worker in its own process group and kill the group after a crash
  (case 5).
- Treat exit code 1 after an interrupted last turn as expected, not as a crash.
  Decide by the last `result`, not by the exit code.
- `rate_limit_event` gives limit pressure on every turn (case 6).
- The model refuses a standalone long `sleep` in Bash, and a test that needs a
  running tool must block on something observable (case 4).
- Not covered: `request_user_dialog` (dialog kinds a host must declare), an
  `initialize` handshake, a real limit, two writers on one session, and pipe
  backpressure with a slow reader.
