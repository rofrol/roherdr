//! What a run's typed decision calls share ([`super::auto_review`],
//! [`super::auto_answer`]): one bounded, stateless `claude -p` call with
//! structured JSON output (no tools, no session kept, the user's normal
//! Claude login, run by the server outside any worker sandbox), each call
//! recorded before its decision is, and a decision that cannot be recorded
//! shown, never hidden.
//!
//! A decision step that fails (the worker store refuses a write, a worker
//! cannot be stopped) leaves the run as it was in the store, which still
//! waits on its event. Until the step succeeds, `todo.wait`, `todo.status`
//! and `todo.runs` show the run `blocked` with the error
//! ([`decision_failure`]), and its driver takes the step again at the next
//! write the store announces (another run's, a `todo.resume`): that write
//! landing is the sign the store takes writes again. No clock retries it.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::review_tools::{self, RepoTools};
use super::{lock, CreditStage, CHANGES};
use crate::api::schema::{TodoEventKind, TodoRunEvent, TodoRunInfo, TodoRunStatus};
use crate::workers::{now_ms, WorkerSupervisor};

/// Calls per decision: the first and one more after a failure.
pub(super) const MAX_CALLS: usize = 2;
/// A failed call's output kept in its record, in characters.
const ERROR_MAX: usize = 2_000;

/// The decision steps that failed, by run, with the error: the run shows
/// `blocked` with it until its step succeeds.
static FAILURES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Why the run's decision step cannot go on, while it cannot.
pub(super) fn decision_failure(run_id: &str) -> Option<String> {
    lock(&FAILURES).get(run_id).cloned()
}

/// The run as a client sees it: `blocked` with the error while a decision
/// step of it fails. The store still has it waiting on its event, which
/// `todo.resume` answers as before.
pub(super) fn shown(mut info: TodoRunInfo) -> TodoRunInfo {
    if let Some(why) = decision_failure(&info.run_id) {
        info.status = TodoRunStatus::Blocked;
        info.error = Some(failure_text(&why));
    }
    info
}

fn failure_text(why: &str) -> String {
    format!(
        "a decision of the run could not be recorded or applied ({why}); the server takes it \
         again at the next write the worker store takes, and `todo resume` still answers the \
         event the run waits on"
    )
}

/// The `blocked` event `todo.wait` returns while a decision step of the
/// run fails, named by the event the run waits on (else its latest): a
/// wait after it waits for the run to go on.
pub(super) fn failure_event(info: &TodoRunInfo, latest: Option<i64>) -> Option<TodoRunEvent> {
    let why = decision_failure(&info.run_id)?;
    let mut event = super::new_event(TodoEventKind::Blocked);
    event.event_id = info.pending_event.or(latest).unwrap_or_default();
    event.error = Some(failure_text(&why));
    event.actions = super::actions_for(TodoEventKind::Blocked);
    Some(event)
}

/// Records that the run's decision step failed with `why`, announces it
/// when it is new, and returns the change generation to wait past. The
/// failure is set and the generation read under the generation's lock, so
/// a write announced by anyone who saw the failure is never missed.
pub(super) fn failed(run_id: &str, why: &str) -> u64 {
    let mut generation = lock(&CHANGES.generation);
    let changed = lock(&FAILURES).insert(run_id.to_owned(), why.to_owned()) != Some(why.to_owned());
    if !changed {
        return *generation;
    }
    *generation += 1;
    CHANGES.changed.notify_all();
    let seen = *generation;
    drop(generation);
    super::super::notify_clients();
    seen
}

/// The run's decision step succeeded: it shows as it is again.
pub(super) fn recovered(run_id: &str) {
    if lock(&FAILURES).remove(run_id).is_some() {
        super::announce();
    }
}

/// Blocks until a write is announced after `seen`, or `stop` says to
/// give up (a live handoff, which announces too).
pub(super) fn wait_for_write(seen: u64, stop: impl Fn() -> bool) {
    let mut generation = lock(&CHANGES.generation);
    while *generation == seen && !stop() {
        generation = CHANGES
            .changed
            .wait(generation)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

/// Test only: the note types whose next write fails, by repository.
#[cfg(test)]
static FAIL_WRITES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Test only: the next write of a `kind` note of a run of `repo` fails, as
/// a worker store that refuses it would.
#[cfg(all(test, unix))]
pub(in crate::workers) fn fail_next_write(repo: &str, kind: &str) {
    lock(&FAIL_WRITES).push((repo.to_owned(), kind.to_owned()));
}

/// Test only: announces a write, as another run's write would.
#[cfg(all(test, unix))]
pub(in crate::workers) fn announce_for_test() {
    super::announce();
}

/// What `claude -p --output-format json` printed: the structured output and
/// the model that answered.
pub(super) fn parse_cli_output(stdout: &str) -> Result<(Value, Option<String>), String> {
    let reply: Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("the reply is not JSON ({error}): {}", cut(stdout)))?;
    if reply["is_error"].as_bool() == Some(true) {
        return Err(format!(
            "the call failed ({}): {}",
            reply["subtype"].as_str().unwrap_or("error"),
            cut(reply["result"].as_str().unwrap_or_default())
        ));
    }
    let model = reply["modelUsage"]
        .as_object()
        .and_then(|models| models.keys().next().cloned());
    let output = match &reply["structured_output"] {
        Value::Object(_) => reply["structured_output"].clone(),
        _ => {
            let text = reply["result"].as_str().unwrap_or_default();
            serde_json::from_str(text.trim()).map_err(|error| {
                format!(
                    "the reply has no structured output and its result is not JSON ({error}): {}",
                    cut(text)
                )
            })?
        }
    };
    Ok((output, model))
}

/// The output tokens one credit call may bill at most.
#[cfg(unix)]
const CREDIT_MAX_TOKENS: u32 = 16_000;

/// What a credit call came to.
// The credit proxy is Unix only: elsewhere a credit call is always refused.
#[cfg_attr(not(unix), allow(dead_code))]
pub(in crate::workers) enum CreditCall {
    Answered { reply: Value, cost: f64 },
    Refused(String),
}

/// The structured output and the model of an API message that answered a
/// call with a JSON schema: its text blocks hold the JSON.
pub(in crate::workers) fn parse_message(reply: &Value) -> Result<(Value, Option<String>), String> {
    if reply["stop_reason"].as_str() == Some("max_tokens") {
        return Err("the model's answer was cut at its token limit".into());
    }
    let text: String = reply["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect();
    let output: Value = serde_json::from_str(text.trim())
        .map_err(|error| format!("the model's answer is not JSON: {error}: {}", cut(&text)))?;
    Ok((output, reply["model"].as_str().map(str::to_owned)))
}

/// A request with the read-only repository tools, sent until the model
/// answers without calling one: each round `send` makes one API call and
/// returns the reply and the run's spend so far. The budget is the tools':
/// calls and result bytes, counted. Once it is spent the model is told not
/// to call a tool (`tool_choice` none) and a call it still makes is
/// answered with a refusal; a model that goes on after that fails the call.
// Used by the credit path, which is Unix only.
#[cfg_attr(not(unix), allow(dead_code))]
fn tool_loop(
    mut body: Value,
    tools: &mut RepoTools<'_>,
    mut send: impl FnMut(&Value) -> Result<(Value, f64), String>,
) -> Result<(Value, f64), String> {
    body["tools"] = review_tools::definitions();
    for _ in 0..review_tools::MAX_CALLS + 3 {
        if tools.spent() {
            body["tool_choice"] = json!({"type": "none"});
        }
        let (reply, cost) = send(&body)?;
        if reply["stop_reason"].as_str() != Some("tool_use") {
            return Ok((reply, cost));
        }
        let uses: Vec<&Value> = reply["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "tool_use")
            .collect();
        let results: Vec<Value> = uses
            .iter()
            .map(|block| {
                let (text, error) =
                    tools.call(block["name"].as_str().unwrap_or_default(), &block["input"]);
                json!({"type": "tool_result", "tool_use_id": block["id"], "content": text,
                       "is_error": error})
            })
            .collect();
        if results.is_empty() {
            return Err("the model stopped for tool use without a tool call".into());
        }
        let messages = body["messages"]
            .as_array_mut()
            .ok_or("the request has no messages")?;
        messages.push(json!({"role": "assistant", "content": reply["content"]}));
        messages.push(json!({"role": "user", "content": results}));
    }
    Err("the model kept calling tools past the tool budget".into())
}

pub(super) fn cut(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(ERROR_MAX) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
    }
}

/// A decision's id: `d-` and 8 base32 characters of a hash of the run, the
/// decided key and the input, so the same decision gets the same id.
pub(super) fn decision_id(run_id: &str, key: &str, digest: &str) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let hash = Sha256::digest(format!("{run_id}:{key}:{digest}"));
    let bits = hash[..5]
        .iter()
        .fold(0u64, |bits, byte| (bits << 8) | u64::from(*byte));
    let id: String = (0..8)
        .map(|index| ALPHABET[((bits >> (35 - 5 * index)) & 31) as usize] as char)
        .collect();
    format!("d-{id}")
}

/// SHA-256 of `text`, in hex.
pub(super) fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// One decision call's prompt: the output schema, the system prompt and
/// the line before the input.
pub(in crate::workers) struct Prompt<'a> {
    pub(in crate::workers) schema: &'a str,
    pub(in crate::workers) system: &'a str,
    pub(in crate::workers) intro: &'a str,
}

/// The calls already recorded for one decision (`kind` notes whose `key`
/// field is `key`): the errors of those that failed, and the checked
/// output of one that answered for the same input, which is taken without
/// asking again.
pub(super) fn recorded_calls(
    calls: &[Value],
    key: (&str, &Value),
    input_digest: &str,
) -> (Vec<String>, Option<(Value, Option<String>)>) {
    let (field, value) = key;
    let mine: Vec<&Value> = calls.iter().filter(|call| &call[field] == value).collect();
    let errors = mine
        .iter()
        .filter_map(|call| call["error"].as_str().map(str::to_owned))
        .collect();
    let answered = mine
        .iter()
        .find(|call| call["input_digest"] == json!(input_digest) && call["output"].is_object())
        .map(|call| {
            (
                call["output"].clone(),
                call["model"].as_str().map(str::to_owned),
            )
        });
    (errors, answered)
}

impl WorkerSupervisor {
    /// The program a decision calls: the server's `claude`.
    pub(super) fn decision_program(&self) -> std::path::PathBuf {
        #[cfg(test)]
        if let Some(program) = lock(&self.shared.review_program).clone() {
            return program;
        }
        self.shared.program.clone()
    }

    #[cfg(all(test, unix))]
    pub(in crate::workers) fn set_review_program_for_test(&self, program: std::path::PathBuf) {
        *lock(&self.shared.review_program) = Some(program);
    }

    /// Appends a note to the run, failing when the store does.
    pub(super) fn write_note(&self, run_id: &str, note: &Value) -> Result<(), String> {
        #[cfg(test)]
        {
            let repo = self.load_run(run_id).map(|run| run.info.repo).ok();
            let kind = note["type"].as_str().unwrap_or_default().to_owned();
            let mut fail = lock(&FAIL_WRITES);
            if let Some(index) = fail
                .iter()
                .position(|(at, of)| Some(at) == repo.as_ref() && *of == kind)
            {
                fail.remove(index);
                return Err(format!(
                    "the worker store failed: injected failure of the {kind} write"
                ));
            }
        }
        self.run_store()
            .map_err(|error| error.to_string())?
            .transaction(|tx| tx.run_note(run_id, note, now_ms()))
            .map_err(|error| format!("the worker store failed: {error}"))?;
        super::announce();
        Ok(())
    }

    /// The call of one of `run`'s decisions, booked as `stage`. A run on the
    /// API credits (`pi-anthropic`) makes it through herdr's credit proxy, its
    /// cost in the credit ledger under the run and that stage; a credit
    /// refusal (the proxy, the cap, the key) falls back once to `claude -p`
    /// and is recorded as a `run_credit_call_fallback` event. Other runs call
    /// `claude -p` as ever.
    pub(in crate::workers) fn call_model_for(
        &self,
        run: &super::Run,
        stage: CreditStage,
        prompt: &Prompt<'_>,
        input: &str,
    ) -> Result<(Value, Option<String>), String> {
        self.call_model_with(run, stage, prompt, input, None)
    }

    /// [`Self::call_model_for`] with `model` when given: `claude -p --model`,
    /// or the credit proxy's model for a credit run (a model the proxy does
    /// not admit is a credit refusal, which falls back to `claude -p`).
    pub(in crate::workers) fn call_model_with(
        &self,
        run: &super::Run,
        stage: CreditStage,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
    ) -> Result<(Value, Option<String>), String> {
        self.call_model_tools(run, stage, prompt, input, model, None)
    }

    /// [`Self::call_model_with`] with the read-only repository `tools` when
    /// given: a credit call runs the tool loop over the API's tool use. The
    /// `claude -p` path (a run without credits, or the fallback after a
    /// credit refusal) has no tools: its call is the same stateless one.
    pub(in crate::workers) fn call_model_tools(
        &self,
        run: &super::Run,
        stage: CreditStage,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
        tools: Option<&mut RepoTools<'_>>,
    ) -> Result<(Value, Option<String>), String> {
        if !run.finish.anthropic_credits {
            return self.call_model(prompt, input, model);
        }
        match self.call_credit(run, stage, prompt, input, model, tools) {
            Ok(CreditCall::Answered { reply, cost }) => {
                let note = json!({"type": "run_credit_call", "run_spent_usd": cost});
                self.write_note(&run.info.run_id, &note)?;
                parse_message(&reply)
            }
            Ok(CreditCall::Refused(reason)) | Err(reason) => {
                let note = json!({"type": "run_credit_call_fallback", "refusal": reason});
                self.write_note(&run.info.run_id, &note)?;
                self.call_model(prompt, input, model)
            }
        }
    }

    /// One call through the credit proxy, in-process (no listener: the
    /// run's grant, the run's ledger rows). `Err` and `Refused` are credit refusals.
    fn call_credit(
        &self,
        run: &super::Run,
        stage: CreditStage,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
        tools: Option<&mut RepoTools<'_>>,
    ) -> Result<CreditCall, String> {
        let caller = CreditCaller {
            run_id: &run.info.run_id,
            item: &run.info.item,
            stage,
            credit_model: run.finish.credit_model.as_deref(),
        };
        credit_call(&caller, prompt, input, model, tools)
    }

    /// One `claude -p` call ([`call_cli`]) with the server's `claude`, run
    /// in the worker store's directory.
    pub(super) fn call_model(
        &self,
        prompt: &Prompt<'_>,
        input: &str,
        model: Option<&str>,
    ) -> Result<(Value, Option<String>), String> {
        call_cli(
            &self.decision_program(),
            &self.shared.dir,
            prompt,
            input,
            model,
        )
    }
}

/// Whom a credit call is booked to in the credit ledger: a run and its item,
/// the stage of the run's work that made the call, and the run's pinned credit
/// model.
// The credit proxy is Unix only: elsewhere a credit call reads none of it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(in crate::workers) struct CreditCaller<'a> {
    pub(in crate::workers) run_id: &'a str,
    pub(in crate::workers) item: &'a str,
    pub(in crate::workers) stage: CreditStage,
    pub(in crate::workers) credit_model: Option<&'a str>,
}

/// One call through the credit proxy, in-process, booked to `caller`.
/// `Err` and `Refused` are credit refusals.
#[cfg(unix)]
pub(in crate::workers) fn credit_call(
    caller: &CreditCaller<'_>,
    prompt: &Prompt<'_>,
    input: &str,
    model: Option<&str>,
    tools: Option<&mut RepoTools<'_>>,
) -> Result<CreditCall, String> {
    use crate::platform::anthropic_proxy::{ledger_path, AnthropicProxy};

    let mut config = crate::config::load_live_config()
        .map_err(|_| "cannot load credit configuration".to_owned())?
        .config
        .workers;
    if let Some(model) = caller
        .credit_model
        .and_then(|model| model.strip_prefix(crate::workers::pi::ANTHROPIC_PREFIX))
    {
        config.anthropic_api_model = model.to_owned();
    }
    if let Some(model) = model {
        config.anthropic_api_model = model.to_owned();
    }
    let ledger = ledger_path(&config).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(crate::config::state_dir()).map_err(|error| error.to_string())?;
    let schema: Value = serde_json::from_str(prompt.schema)
        .map_err(|error| format!("the output schema is not JSON: {error}"))?;
    let body = json!({
        "model": config.anthropic_api_model,
        "max_tokens": CREDIT_MAX_TOKENS,
        "system": prompt.system,
        "messages": [{"role": "user", "content": format!("{}\n\n{input}\n", prompt.intro)}],
        "output_config": {"format": {"type": "json_schema", "schema": schema}},
    });
    let send = |body: &Value| -> Result<(Value, f64), String> {
        let proxy = AnthropicProxy::open(&config, &ledger).map_err(|error| error.to_string())?;
        let (status, reply) = proxy
            .message(
                caller.run_id,
                caller.item,
                caller.stage.as_str(),
                &body.to_string(),
            )
            .map_err(|error| format!("the credit proxy refused the call: {error}"))?;
        if status != 200 {
            return Err(format!(
                "the credit proxy answered {status}: {}",
                cut(&reply)
            ));
        }
        let reply: Value = serde_json::from_str(&reply)
            .map_err(|error| format!("the credit proxy's answer is not JSON: {error}"))?;
        // The proxy settled the call in the ledger before it answered.
        let cost = AnthropicProxy::accounting(&ledger, Some(caller.run_id))
            .map_err(|error| error.to_string())?
            .spent_usd;
        Ok((reply, cost))
    };
    let answered = match tools {
        Some(tools) => tool_loop(body, tools, send),
        None => send(&body),
    };
    Ok(match answered {
        Ok((reply, cost)) => CreditCall::Answered { reply, cost },
        Err(reason) => CreditCall::Refused(reason),
    })
}

#[cfg(not(unix))]
pub(in crate::workers) fn credit_call(
    _caller: &CreditCaller<'_>,
    _prompt: &Prompt<'_>,
    _input: &str,
    _model: Option<&str>,
    _tools: Option<&mut RepoTools<'_>>,
) -> Result<CreditCall, String> {
    Err("the Anthropic credit proxy requires Unix".into())
}

/// One `claude -p` call by `program` with `input` on its standard input;
/// its exit is the event waited for (no deadline: the provider imposes
/// none). The call runs in `dir` with this process's environment (the
/// user's login) without `HERDR_*`, and without tools, session, MCP
/// servers, hooks or project instructions. `model` is `--model`.
pub(in crate::workers) fn call_cli(
    program: &std::path::Path,
    dir: &std::path::Path,
    prompt: &Prompt<'_>,
    input: &str,
    model: Option<&str>,
) -> Result<(Value, Option<String>), String> {
    let mut command = Command::new(program);
    if let Some(model) = model {
        command.args(["--model", model]);
    }
    command
        .args([
            "-p",
            "--output-format",
            "json",
            "--json-schema",
            prompt.schema,
            "--tools",
            "",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--safe-mode",
            "--system-prompt",
            prompt.system,
        ])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("HERDR_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot run {}: {error}", program.display()))?;
    // Written from a thread: a child that answers before it read all
    // of its input must not block on a full pipe while this one writes.
    let writer = child.stdin.take().map(|mut stdin| {
        let input = format!("{}\n\n{input}\n", prompt.intro);
        std::thread::spawn(move || stdin.write_all(input.as_bytes()))
    });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("waiting for {}: {error}", program.display()))?;
    if let Some(writer) = writer {
        // A child that exited without reading all of it broke the pipe;
        // its exit status says what happened.
        let _ = writer.join();
    }
    if !output.status.success() {
        return Err(format!(
            "{} exited with {}: {}",
            program.display(),
            output.status,
            cut(&String::from_utf8_lossy(&output.stderr))
        ));
    }
    parse_cli_output(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_reply_gives_the_structured_output_and_the_model() {
        let reply = json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "",
            "structured_output": {"action": "approve"},
            "modelUsage": {"claude-opus-5-5": {"inputTokens": 1}},
        });
        assert_eq!(
            parse_cli_output(&reply.to_string()),
            Ok((json!({"action": "approve"}), Some("claude-opus-5-5".into())))
        );
        let text =
            json!({"is_error": false, "result": "{\"action\": \"retry\", \"review\": \"x\"}"});
        assert_eq!(
            parse_cli_output(&text.to_string()).unwrap().0["action"],
            "retry"
        );
        assert!(parse_cli_output("not json").is_err());
        assert!(
            parse_cli_output(&json!({"is_error": true, "result": "limit"}).to_string())
                .unwrap_err()
                .contains("limit")
        );
        assert!(parse_cli_output(&json!({"result": "prose"}).to_string()).is_err());
    }

    #[test]
    fn the_tool_loop_runs_tools_until_the_budget_is_spent_then_forces_the_answer() {
        let (dir, sha) = super::super::review_tools::tests::repo_with(&[("a.txt", "x\n")]);
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        let mut sent: Vec<Value> = Vec::new();
        let body = json!({"messages": [{"role": "user", "content": "review"}]});
        let result = tool_loop(body, &mut tools, |body| {
            sent.push(body.clone());
            let round = sent.len();
            if body["tool_choice"]["type"] == "none" {
                let answer = json!({"stop_reason": "end_turn", "content": [
                    {"type": "text", "text": "{\"action\":\"approve\"}"}]});
                return Ok((answer, 0.5));
            }
            // Two parallel calls a round.
            let uses: Vec<Value> = (0..2)
                .map(|i| {
                    json!({"type": "tool_use", "id": format!("t{round}{i}"),
                    "name": "read_file", "input": {"path": "a.txt"}})
                })
                .collect();
            Ok((json!({"stop_reason": "tool_use", "content": uses}), 0.1))
        });
        let (reply, cost) = result.unwrap();
        assert_eq!(reply["stop_reason"], "end_turn");
        assert_eq!(cost, 0.5);
        assert_eq!(tools.calls.len(), review_tools::MAX_CALLS);
        assert!(tools.calls.iter().all(|call| !call.error));
        assert_eq!(sent.len(), review_tools::MAX_CALLS / 2 + 1);
        assert_eq!(sent[0]["tools"].as_array().unwrap().len(), 3);
        assert!(sent[0].get("tool_choice").is_none());
        let last = sent.last().unwrap()["messages"].as_array().unwrap().clone();
        assert_eq!(last.len(), 1 + 2 * (review_tools::MAX_CALLS / 2));
        assert_eq!(last[2]["content"][0]["tool_use_id"], "t10");
        assert_eq!(last[2]["content"][0]["is_error"], false);
        // A model that ignores `tool_choice` none gets refusals, then fails.
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        let mut rounds = 0;
        let stubborn = tool_loop(json!({"messages": []}), &mut tools, |_| {
            rounds += 1;
            Ok((
                json!({"stop_reason": "tool_use", "content": [
                {"type": "tool_use", "id": "x", "name": "grep", "input": {"pattern": "x"}}]}),
                0.0,
            ))
        });
        assert!(stubborn.unwrap_err().contains("kept calling tools"));
        assert_eq!(rounds, review_tools::MAX_CALLS + 3);
        assert!(tools.calls.last().unwrap().error);
        // A refusal of the proxy ends the loop.
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        let refused = tool_loop(json!({"messages": []}), &mut tools, |_| Err("cap".into()));
        assert_eq!(refused.unwrap_err(), "cap");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_decision_id_is_stable_for_the_same_decision() {
        let id = decision_id("r-abcdefgh", "7", "digest");
        assert_eq!(id, decision_id("r-abcdefgh", "7", "digest"));
        assert_ne!(id, decision_id("r-abcdefgh", "8", "digest"));
        assert!(id.starts_with("d-") && id.len() == 10, "{id}");
    }

    #[test]
    fn a_recorded_answer_for_the_same_input_is_taken_and_errors_counted() {
        let calls = vec![
            json!({"event": 7, "input_digest": "a", "error": "boom"}),
            json!({"event": 7, "input_digest": "a", "output": {"action": "approve"}, "model": "m"}),
            json!({"event": 8, "input_digest": "a", "error": "other"}),
        ];
        let (errors, answered) = recorded_calls(&calls, ("event", &json!(7)), "a");
        assert_eq!(errors, ["boom"]);
        assert_eq!(
            answered,
            Some((json!({"action": "approve"}), Some("m".into())))
        );
        let (_, other_input) = recorded_calls(&calls, ("event", &json!(7)), "b");
        assert_eq!(other_input, None);
    }

    #[test]
    fn a_failed_decision_shows_the_run_blocked_until_it_recovers() {
        let info: TodoRunInfo = serde_json::from_value(json!({
            "run_id": "r-failtest", "repo": "/r", "item": "t-abcd2345", "step": "review",
            "status": "waiting", "attempt": 1, "task": "", "message": "", "paths": [],
            "checks": [], "pending_event": 9, "created_ms": 0, "updated_ms": 0,
        }))
        .unwrap();
        assert_eq!(shown(info.clone()).status, TodoRunStatus::Waiting);
        assert!(failure_event(&info, Some(9)).is_none());
        failed("r-failtest", "disk full");
        let blocked = shown(info.clone());
        assert_eq!(blocked.status, TodoRunStatus::Blocked);
        assert!(blocked.error.unwrap().contains("disk full"));
        let event = failure_event(&info, Some(9)).unwrap();
        assert_eq!((event.kind, event.event_id), (TodoEventKind::Blocked, 9));
        recovered("r-failtest");
        assert_eq!(shown(info).status, TodoRunStatus::Waiting);
    }
}
