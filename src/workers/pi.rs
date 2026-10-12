//! Headless pi workers: `pi --mode rpc` in place of `claude -p`, with the
//! same folder slot, contract, file policy, verify and `WORKER-DONE` line.
//!
//! The rest of the supervisor speaks Claude's stream-json, so this module
//! translates at the edge, both ways, and nothing else needs to know which
//! CLI runs:
//!
//! - **Output** ([`Translator`]): pi's session events become the stream-json
//!   events the status folds. A completed assistant message (`message_end`)
//!   is an `assistant` event with its text and `tool_use` blocks; a finished
//!   tool (`tool_execution_end`) a `user` event with its `tool_result`;
//!   `agent_settled` (pi will not go on by itself) the turn's `result`, whose
//!   text is the last assistant text, so the worker's `WORKER-DONE` line
//!   reaches the run as a Claude worker's does. The streaming deltas, queue,
//!   retry and compaction events are not recorded.
//! - **Tool calls**: pi asks nobody before a tool runs, so herdr loads one
//!   extension of its own ([`extension_source`], `--no-extensions -e`) whose
//!   `tool_call` handler asks the client through pi's extension UI (an
//!   `input` dialog titled [`GATE_TITLE`]) and blocks the call unless herdr
//!   allows it. Such a request becomes a `can_use_tool` request named as
//!   Claude's tools are (`bash` is `Bash`, `read` is `Read` with `file_path`,
//!   ...), so herdr's policy decides it and a question reaches the
//!   coordinator as a Claude worker's does. The extension runs every `bash`
//!   command the policy allows through the platform's shell sandbox
//!   ([`crate::platform::sandboxed_shell_prefix`]): writes only to the
//!   worker's folder, its temp dir and the slot's caches, no network, no
//!   credential or environment file read; the same bounds Claude Code's
//!   sandbox gives a Claude worker's Bash.
//! - **Fail closed**: the extension reports [`GATE_READY`] when its session
//!   starts, before pi answers the `get_state` herdr sends first
//!   ([`INIT_REQUEST_ID`]). Herdr writes the worker's prompt only after that
//!   answer and only when the report came before it; a pi that did not load
//!   the extension is ended without a prompt, so no tool ever runs ungated.
//! - **Input** ([`input_line`]): a prompt becomes pi's `prompt` command
//!   (queued as a follow-up while pi works), a permission answer an
//!   `extension_ui_response`, an interrupt `abort`.
//!
//! Known limits of this first slice: the `[workers] pre_tool_checks` run
//! for Claude workers only, a pi worker has no session to resume, so it
//! cannot be taken over into a tab, and a live handoff between pi's last
//! message and its `agent_settled` loses the result's text.

use serde_json::{json, Value};

/// The program a pi worker runs.
pub(super) const PROGRAM: &str = "pi";

pub(super) const ANTHROPIC_MODEL: &str = "herdr-anthropic/claude-sonnet-4-5-20250929";

/// pi 1.1's custom provider, isolated from the user's auth.json. Only the
/// opaque grant is interpolated; neither credential is written to this file.
#[cfg(unix)]
pub(super) fn anthropic_models(address: &str) -> Value {
    json!({"providers": {"herdr-anthropic": {
        "baseUrl": address, "api": "anthropic-messages",
        "apiKey": "${HERDR_ANTHROPIC_PROXY_TOKEN}",
        "models": [{"id": "claude-sonnet-4-5-20250929", "contextWindow": 200000, "maxTokens": 8192}]
    }}})
}

/// The model a pi worker runs when none is given: GPT-6.1 Sol through the
/// `openai-codex` provider, on the owner's ChatGPT plan.
pub(super) const DEFAULT_MODEL: &str = "openai-codex/gpt-6.1-sol";

/// The id of herdr's first command, `get_state`: its answer comes once pi
/// started, and carries the session id.
pub(super) const INIT_REQUEST_ID: &str = "herdr-init";

/// What herdr's extension reports (a `notify`) when its session starts.
pub(super) const GATE_READY: &str = "herdr-gate-ready";

/// The title of the extension's dialog that asks herdr about a tool call.
pub(super) const GATE_TITLE: &str = "herdr-tool-call";

/// The built-in tools a pi worker has; no MCP, extension or code-mode tool.
const TOOLS: &str = "read,bash,edit,write,grep,find,ls";

/// pi's arguments: RPC mode, the model, no session file, herdr's extension
/// as the only one, no skills, prompt templates, themes or MCP servers,
/// project resources not trusted, the built-in tools only, and herdr's
/// contract appended to the system prompt.
pub(super) fn args(model: &str, extension: &str, contract: &str) -> Vec<String> {
    [
        "--mode",
        "rpc",
        "--model",
        model,
        "--no-session",
        "--no-extensions",
        "-e",
        extension,
        "--no-skills",
        "--no-prompt-templates",
        "--no-themes",
        "--no-mcp",
        "--no-approve",
        "--tools",
        TOOLS,
        "--append-system-prompt",
        contract,
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

/// herdr's first line to pi, which pi answers once it has started.
pub(super) fn init_request() -> Value {
    json!({"type": "get_state", "id": INIT_REQUEST_ID})
}

/// herdr's extension: reports [`GATE_READY`] when its session starts, asks
/// herdr about every tool call and blocks it unless herdr allows it, and
/// runs every allowed `bash` command behind `shell_prefix` (the shell text
/// before the shell-quoted command). A missing or unreadable answer blocks.
pub(super) fn extension_source(shell_prefix: &str) -> String {
    let constants = json!({
        "ready": GATE_READY,
        "title": GATE_TITLE,
        "prefix": shell_prefix,
    });
    format!(
        r#"// Written by herdr for one headless pi worker; see src/workers/pi.rs.
const HERDR = {constants};
const quote = (text) => "'" + text.replace(/'/g, "'\\''") + "'";
export default function (pi) {{
  pi.on("session_start", async (_event, ctx) => {{
    ctx.ui.notify(HERDR.ready, "info");
  }});
  pi.on("tool_call", async (event, ctx) => {{
    let answer = null;
    try {{
      const value = await ctx.ui.input(
        HERDR.title,
        JSON.stringify({{ tool_call_id: event.toolCallId, tool_name: event.toolName, input: event.input }}),
      );
      answer = typeof value === "string" ? JSON.parse(value) : null;
    }} catch (error) {{
      answer = null;
    }}
    if (!answer || answer.allow !== true) {{
      return {{ block: true, reason: (answer && answer.reason) || "herdr did not allow this tool call" }};
    }}
    if (event.toolName === "bash") {{
      event.input.command = HERDR.prefix + quote(String(event.input.command ?? ""));
    }}
  }});
}}
"#
    )
}

/// pi's tool name and input as herdr's policy names Claude's: the tool, and
/// the input with the path field the policy reads (`file_path` for the file
/// tools, `path` and `pattern` for `find`). Unknown tools keep their name,
/// which the policy leaves to the user.
fn claude_tool(tool: &str, input: &Value) -> (String, Value) {
    let mut mapped = input.clone();
    if !mapped.is_object() {
        mapped = json!({});
    }
    let path = input.get("path").cloned();
    let name = match tool {
        "bash" => "Bash",
        "read" | "edit" | "write" => {
            mapped["file_path"] = path.unwrap_or(Value::Null);
            match tool {
                "read" => "Read",
                "edit" => "Edit",
                _ => "Write",
            }
        }
        "grep" => "Grep",
        "find" => "Glob",
        "ls" => "LS",
        other => return (other.to_owned(), mapped),
    };
    (name.to_owned(), mapped)
}

/// The text of a pi message's content: a string, or its text blocks.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"].as_str() == Some("text"))
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// What one line of pi's output is for herdr.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Out {
    /// Not recorded: a delta, or an event the status has no use for.
    Skip,
    /// Recorded as this stream-json event.
    Record(Value),
    /// The answer to [`init_request`] as a `system/init` event, and whether
    /// the extension reported itself before it.
    Init { event: Value, gate_ready: bool },
    /// A dialog herdr does not answer: recorded as this input, which
    /// [`input_line`] turns into its cancellation.
    Cancel(Value),
}

/// A pi worker's output translator. What it keeps: whether the extension
/// reported itself, and the last assistant text and error of the turn.
#[derive(Debug, Default)]
pub(super) struct Translator {
    gate_ready: bool,
    last_text: Option<String>,
    last_error: Option<String>,
}

impl Translator {
    pub(super) fn output(&mut self, event: &Value) -> Out {
        match event["type"].as_str().unwrap_or("") {
            "extension_ui_request" => self.ui_request(event),
            "response" => self.response(event),
            "message_end" => self.message_end(&event["message"]),
            "tool_execution_end" => {
                let result = &event["result"];
                Out::Record(json!({
                    "type": "user",
                    "message": {"role": "user", "content": [{
                        "type": "tool_result",
                        "tool_use_id": event["toolCallId"],
                        "content": content_text(&result["content"]),
                        "is_error": event["isError"].as_bool().unwrap_or(false),
                    }]},
                }))
            }
            "agent_settled" => Out::Record(self.settled(event["aborted"].as_bool() == Some(true))),
            "extension_error" => Out::Record(json!({
                "type": "system",
                "subtype": "pi_extension_error",
                "event": event["event"],
                "error": event["error"],
            })),
            _ => Out::Skip,
        }
    }

    fn ui_request(&mut self, event: &Value) -> Out {
        let id = event["id"].as_str().unwrap_or("");
        match event["method"].as_str().unwrap_or("") {
            "notify" if event["message"].as_str() == Some(GATE_READY) => {
                self.gate_ready = true;
                Out::Record(json!({"type": "system", "subtype": "herdr_gate_ready"}))
            }
            "input" if event["title"].as_str() == Some(GATE_TITLE) => {
                let call: Value = event["placeholder"]
                    .as_str()
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_else(|| json!({}));
                let pi_tool = call["tool_name"].as_str().unwrap_or("");
                let (tool_name, input) = claude_tool(pi_tool, &call["input"]);
                Out::Record(json!({
                    "type": "control_request",
                    "request_id": id,
                    "request": {
                        "subtype": "can_use_tool",
                        "tool_name": tool_name,
                        "input": input,
                        "tool_use_id": call["tool_call_id"],
                        "pi_tool_name": pi_tool,
                    },
                }))
            }
            "select" | "confirm" | "input" | "editor" => {
                Out::Cancel(json!({"type": "pi_dialog_cancel", "id": id}))
            }
            _ => Out::Skip,
        }
    }

    fn response(&mut self, event: &Value) -> Out {
        if event["id"].as_str() == Some(INIT_REQUEST_ID) {
            let data = &event["data"];
            let model = match (
                data["model"]["provider"].as_str(),
                data["model"]["id"].as_str(),
            ) {
                (Some(provider), Some(id)) => Value::String(format!("{provider}/{id}")),
                _ => Value::Null,
            };
            return Out::Init {
                event: json!({
                    "type": "system",
                    "subtype": "init",
                    // Not `session_id`: pi runs without a session file,
                    // so there is no session for a takeover to resume.
                    "pi_session_id": data["sessionId"],
                    "model": model,
                    "agent": "pi",
                    "error": event["error"],
                }),
                gate_ready: self.gate_ready && event["success"].as_bool() == Some(true),
            };
        }
        if event["success"].as_bool() != Some(false) {
            return Out::Skip;
        }
        let error = event["error"].as_str().unwrap_or("no reason given");
        // A refused prompt starts no run, so no `agent_settled` ends it.
        if event["command"].as_str() == Some("prompt") {
            return Out::Record(json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": true,
                "terminal_reason": "prompt_refused",
                "result": format!("pi refused the prompt: {error}"),
            }));
        }
        Out::Record(json!({
            "type": "system",
            "subtype": "pi_command_failed",
            "command": event["command"],
            "error": error,
        }))
    }

    fn message_end(&mut self, message: &Value) -> Out {
        if message["role"].as_str() != Some("assistant") {
            return Out::Skip;
        }
        let blocks = message["content"].as_array().cloned().unwrap_or_default();
        let content: Vec<Value> = blocks
            .iter()
            .filter_map(|block| match block["type"].as_str() {
                Some("text") => Some(json!({"type": "text", "text": block["text"]})),
                Some("toolCall") => Some(json!({
                    "type": "tool_use",
                    "id": block["id"],
                    "name": block["name"],
                    "input": block["arguments"],
                })),
                _ => None,
            })
            .collect();
        let text = content_text(&message["content"]);
        if !text.trim().is_empty() {
            self.last_text = Some(text);
        }
        if message["stopReason"].as_str() == Some("error") {
            self.last_error = Some(
                message["errorMessage"]
                    .as_str()
                    .unwrap_or("the model call failed")
                    .to_owned(),
            );
        }
        Out::Record(json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "model": message["model"],
                "content": content,
                "stop_reason": message["stopReason"],
            },
        }))
    }

    /// The turn's `result`, from what the turn left; the next turn starts
    /// with none.
    fn settled(&mut self, aborted: bool) -> Value {
        let text = self.last_text.take();
        let error = self.last_error.take();
        if aborted {
            return json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": false,
                "terminal_reason": "aborted",
                "result": text,
            });
        }
        match error {
            Some(error) => json!({
                "type": "result",
                "subtype": "error_during_execution",
                "is_error": true,
                "terminal_reason": "model_error",
                "result": text.unwrap_or_else(|| error.clone()),
                "error": error,
            }),
            None => json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "terminal_reason": "completed",
                "result": text,
            }),
        }
    }
}

/// The line pi gets for an input event herdr recorded, none for one pi has
/// no use for (it is recorded only).
pub(super) fn input_line(event: &Value) -> Option<Value> {
    match event["type"].as_str()? {
        "user" => Some(json!({
            "type": "prompt",
            "message": content_text(&event["message"]["content"]),
            "streamingBehavior": "followUp",
        })),
        "control_response" => {
            let response = &event["response"];
            let id = response["request_id"].as_str()?;
            let answer = &response["response"];
            let value = match answer["behavior"].as_str() {
                Some("allow") => json!({"allow": true}),
                Some("deny") => json!({
                    "allow": false,
                    "reason": answer["message"].as_str().unwrap_or("denied by herdr"),
                }),
                _ => {
                    return Some(
                        json!({"type": "extension_ui_response", "id": id, "cancelled": true}),
                    )
                }
            };
            Some(json!({"type": "extension_ui_response", "id": id, "value": value.to_string()}))
        }
        "control_request" => match event["request"]["subtype"].as_str()? {
            "interrupt" => Some(json!({"type": "abort"})),
            _ => None,
        },
        "pi_dialog_cancel" => Some(json!({
            "type": "extension_ui_response",
            "id": event["id"],
            "cancelled": true,
        })),
        "get_state" => Some(event.clone()),
        _ => None,
    }
}

/// Whether an input event is a prompt, which herdr holds back until the
/// gate is proven ([`Out::Init`]).
pub(super) fn is_prompt(event: &Value) -> bool {
    event["type"].as_str() == Some("user")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(out: Out) -> Value {
        match out {
            Out::Record(event) => event,
            other => panic!("not recorded: {other:?}"),
        }
    }

    #[test]
    fn the_gate_is_ready_only_when_its_report_comes_before_the_init_answer() {
        let answer = json!({"id": INIT_REQUEST_ID, "type": "response", "command": "get_state",
            "success": true, "data": {"sessionId": "s1",
            "model": {"provider": "openai-codex", "id": "gpt-6.1-sol"}}});
        let mut without = Translator::default();
        let Out::Init { event, gate_ready } = without.output(&answer) else {
            panic!("no init");
        };
        assert!(!gate_ready);
        assert_eq!(event["subtype"], "init");
        assert_eq!(event["pi_session_id"], "s1");
        assert_eq!(event.get("session_id"), None);
        assert_eq!(event["model"], "openai-codex/gpt-6.1-sol");

        let mut with = Translator::default();
        let ready = json!({"type": "extension_ui_request", "id": "u1", "method": "notify",
            "message": GATE_READY});
        assert_eq!(record(with.output(&ready))["subtype"], "herdr_gate_ready");
        let Out::Init { gate_ready, .. } = with.output(&answer) else {
            panic!("no init");
        };
        assert!(gate_ready);
        // Another notify proves nothing.
        let mut other = Translator::default();
        let notify = json!({"type": "extension_ui_request", "id": "u2", "method": "notify",
            "message": "hello"});
        assert_eq!(other.output(&notify), Out::Skip);
    }

    #[test]
    fn a_gate_request_becomes_a_can_use_tool_request_named_as_claude_tools() {
        let mut translator = Translator::default();
        let request = |tool: &str, input: Value| {
            json!({"type": "extension_ui_request", "id": "g1", "method": "input",
                "title": GATE_TITLE, "placeholder": json!({"tool_call_id": "call-1",
                "tool_name": tool, "input": input}).to_string()})
        };
        let bash = record(translator.output(&request("bash", json!({"command": "ls"}))));
        assert_eq!(bash["type"], "control_request");
        assert_eq!(bash["request_id"], "g1");
        assert_eq!(bash["request"]["subtype"], "can_use_tool");
        assert_eq!(bash["request"]["tool_name"], "Bash");
        assert_eq!(bash["request"]["input"]["command"], "ls");
        assert_eq!(bash["request"]["tool_use_id"], "call-1");
        for (tool, claude) in [("read", "Read"), ("edit", "Edit"), ("write", "Write")] {
            let event = record(translator.output(&request(tool, json!({"path": "a/b.rs"}))));
            assert_eq!(event["request"]["tool_name"], claude);
            assert_eq!(event["request"]["input"]["file_path"], "a/b.rs");
        }
        let find = record(translator.output(&request("find", json!({"pattern": "*.rs"}))));
        assert_eq!(find["request"]["tool_name"], "Glob");
        let other = record(translator.output(&request("mcp__x", json!({}))));
        assert_eq!(other["request"]["tool_name"], "mcp__x");
        // A dialog that is not herdr's is cancelled, so pi does not hang.
        let confirm = json!({"type": "extension_ui_request", "id": "c1", "method": "confirm",
            "title": "Sure?"});
        let Out::Cancel(cancel) = translator.output(&confirm) else {
            panic!("not cancelled");
        };
        assert_eq!(
            input_line(&cancel),
            Some(json!({"type": "extension_ui_response", "id": "c1", "cancelled": true}))
        );
    }

    #[test]
    fn a_turn_becomes_assistant_tool_result_and_a_result_with_the_last_text() {
        let mut translator = Translator::default();
        let call = json!({"type": "message_end", "message": {"role": "assistant",
            "model": "gpt-6.1-sol", "stopReason": "toolUse", "content": [
                {"type": "thinking", "thinking": "hm"},
                {"type": "toolCall", "id": "call-1", "name": "bash",
                 "arguments": {"command": "ls"}}]}});
        let assistant = record(translator.output(&call));
        assert_eq!(assistant["type"], "assistant");
        assert_eq!(
            assistant["message"]["content"],
            json!([{"type": "tool_use", "id": "call-1", "name": "bash",
                "input": {"command": "ls"}}])
        );
        let done = json!({"type": "tool_execution_end", "toolCallId": "call-1",
            "toolName": "bash", "isError": false,
            "result": {"content": [{"type": "text", "text": "a\nb\n"}]}});
        let user = record(translator.output(&done));
        assert_eq!(user["type"], "user");
        assert_eq!(user["message"]["content"][0]["tool_use_id"], "call-1");
        assert_eq!(user["message"]["content"][0]["content"], "a\nb\n");
        let reply = json!({"type": "message_end", "message": {"role": "assistant",
            "stopReason": "stop", "content": [{"type": "text",
            "text": "done\nWORKER-DONE abc1234 | summary"}]}});
        record(translator.output(&reply));
        // A user message or delta is not recorded.
        let user_end = json!({"type": "message_end", "message": {"role": "user",
            "content": "go"}});
        assert_eq!(translator.output(&user_end), Out::Skip);
        let delta = json!({"type": "message_update", "assistantMessageEvent":
            {"type": "text_delta", "delta": "x"}});
        assert_eq!(translator.output(&delta), Out::Skip);
        let result = record(translator.output(&json!({"type": "agent_settled",
            "aborted": false})));
        assert_eq!(result["type"], "result");
        assert_eq!(result["subtype"], "success");
        assert_eq!(result["is_error"], false);
        assert_eq!(result["result"], "done\nWORKER-DONE abc1234 | summary");
        // The next turn starts without the last one's text.
        let next = record(translator.output(&json!({"type": "agent_settled"})));
        assert_eq!(next["result"], Value::Null);
    }

    #[test]
    fn a_model_error_an_abort_and_a_refused_prompt_end_the_turn_as_failures() {
        let mut translator = Translator::default();
        let failed = json!({"type": "message_end", "message": {"role": "assistant",
            "stopReason": "error", "errorMessage": "429 usage limit", "content": []}});
        record(translator.output(&failed));
        let result = record(translator.output(&json!({"type": "agent_settled",
            "aborted": false})));
        assert_eq!(result["is_error"], true);
        assert_eq!(result["subtype"], "error_during_execution");
        assert_eq!(result["result"], "429 usage limit");
        let aborted = record(translator.output(&json!({"type": "agent_settled",
            "aborted": true})));
        assert_eq!(aborted["terminal_reason"], "aborted");
        let refused = record(translator.output(&json!({"type": "response", "id": "x",
            "command": "prompt", "success": false, "error": "No API key"})));
        assert_eq!(refused["type"], "result");
        assert_eq!(refused["is_error"], true);
        assert!(refused["result"].as_str().unwrap().contains("No API key"));
        let accepted = json!({"type": "response", "command": "prompt", "success": true});
        assert_eq!(translator.output(&accepted), Out::Skip);
    }

    #[cfg(unix)]
    #[test]
    fn anthropic_provider_file_contains_only_proxy_address_and_environment_reference() {
        let config = anthropic_models("http://127.0.0.1:12345");
        let provider = &config["providers"]["herdr-anthropic"];
        assert_eq!(provider["api"], "anthropic-messages");
        assert_eq!(provider["baseUrl"], "http://127.0.0.1:12345");
        assert_eq!(provider["apiKey"], "${HERDR_ANTHROPIC_PROXY_TOKEN}");
        assert_eq!(provider["models"][0]["id"], "claude-sonnet-4-5-20250929");
        assert!(!config.to_string().contains("sk-ant-"));
        assert!(
            args(ANTHROPIC_MODEL, "extension", "contract").contains(&ANTHROPIC_MODEL.to_owned())
        );
    }

    #[test]
    fn herdr_inputs_become_pi_commands() {
        let prompt = json!({"type": "user", "message": {"role": "user", "content": "do it"}});
        assert!(is_prompt(&prompt));
        assert_eq!(
            input_line(&prompt),
            Some(json!({"type": "prompt", "message": "do it", "streamingBehavior": "followUp"}))
        );
        let allow = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "g1", "response": {"behavior": "allow", "updatedInput": {}}}});
        let line = input_line(&allow).unwrap();
        assert_eq!(line["type"], "extension_ui_response");
        assert_eq!(line["id"], "g1");
        assert_eq!(
            serde_json::from_str::<Value>(line["value"].as_str().unwrap()).unwrap(),
            json!({"allow": true})
        );
        let deny = json!({"type": "control_response", "response": {"subtype": "success",
            "request_id": "g2", "response": {"behavior": "deny", "message": "outside"}}});
        let line = input_line(&deny).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(line["value"].as_str().unwrap()).unwrap(),
            json!({"allow": false, "reason": "outside"})
        );
        let interrupt = json!({"type": "control_request", "request_id": "i",
            "request": {"subtype": "interrupt"}});
        assert_eq!(input_line(&interrupt), Some(json!({"type": "abort"})));
        assert_eq!(input_line(&init_request()), Some(init_request()));
        let initialize = json!({"type": "control_request", "request_id": "x",
            "request": {"subtype": "initialize"}});
        assert_eq!(input_line(&initialize), None);
    }

    #[test]
    fn the_extension_reports_itself_gates_every_call_and_sandboxes_bash() {
        let source = extension_source("/usr/bin/sandbox-exec -p 'p' /bin/bash -c ");
        assert!(source.contains(GATE_READY), "{source}");
        assert!(source.contains(GATE_TITLE), "{source}");
        assert!(source.contains(r#""prefix":"/usr/bin/sandbox-exec -p 'p' /bin/bash -c ""#));
        assert!(source.contains("pi.on(\"tool_call\""), "{source}");
        assert!(source.contains("answer.allow !== true"), "{source}");
        assert!(source.contains("event.toolName === \"bash\""), "{source}");
        let args = args(DEFAULT_MODEL, "/w/gate.js", "contract");
        let joined = args.join(" ");
        for part in [
            "--mode rpc",
            "--model openai-codex/gpt-6.1-sol",
            "--no-extensions -e /w/gate.js",
            "--no-mcp",
            "--no-approve",
            "--tools read,bash,edit,write,grep,find,ls",
            "--append-system-prompt contract",
        ] {
            assert!(joined.contains(part), "{part:?} not in {joined}");
        }
    }
}
