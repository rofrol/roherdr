//! Runs the user's pre-tool checks (`[workers] pre_tool_checks`) for a
//! headless worker, such as a PreToolUse hook the user runs in interactive
//! sessions.
//!
//! Workers start with `disableAllHooks`, so no hook from the user's or the
//! project's settings runs in them (trial 2, T2-1). Herdr registers one
//! PreToolUse hook of its own instead, as an SDK callback in the
//! `initialize` control request: the CLI still runs SDK callback hooks when
//! `disableAllHooks` is set, and only those (Observed in the 2.1.295
//! bundle: with `disableAllHooks` in a non-policy source the hooks snapshot
//! keeps only managed hooks, and the registered `sdkHost` callbacks are
//! added to it unconditionally). Before each Bash, Write, Edit and MultiEdit
//! call the CLI sends a `hook_callback` request and waits for its answer.
//! Herdr's policy could not do this instead: with `autoAllowBashIfSandboxed`
//! the CLI never asks it about a sandboxed Bash command, nor about a call a
//! user's allow rule permits.
//!
//! The hook is registered for every worker, checks or none: it also runs
//! herdr's own rule against waiting in the background
//! ([`super::policy::background_wait_denial`]) before any check, for
//! [`MATCHER`]'s tools; the checks run only for [`CHECKED_TOOLS`].
//!
//! Each check runs in the worker's directory, outside the worker's sandbox,
//! with the hook input on stdin, as Claude Code runs a command hook, one
//! after the other until one denies. A check denies by exiting 0 with
//! `hookSpecificOutput.permissionDecision` `deny` (its
//! `permissionDecisionReason` is the reason) or the older `decision` `block`
//! (with `reason`), or by exiting 2 (its stderr is the reason). Anything
//! else is no objection: only a denial is passed on, never an `allow`, which
//! would skip herdr's policy, nor an `updatedInput`.
//!
//! A check that cannot start, exits otherwise non-zero or by a signal, or
//! prints broken JSON fails open: the call goes on and the failure is a
//! `pre_tool_check_failed` event. Herdr sets no time limit; the check's own
//! exit decides. When the CLI gives up on the hook (its own hook timeout,
//! or the turn ends) it cancels the request, and herdr then kills the check
//! and reports that as a failure too.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::lock;
use crate::platform::Signal;

/// The callback id herdr registers; the CLI names it in each `hook_callback`.
pub(super) const CALLBACK_ID: &str = "herdr-pre-tool-checks";

/// The tools the hook runs for: the checked ones and those that can wait in
/// the background.
pub(super) const MATCHER: &str =
    "Bash|Write|Edit|MultiEdit|Monitor|ScheduleWakeup|CronCreate|Agent|Task";

/// The tools the checks run for.
pub(super) const CHECKED_TOOLS: &[&str] = &["Bash", "Write", "Edit", "MultiEdit"];

/// The request id of herdr's `initialize` request.
pub(super) const INITIALIZE_REQUEST_ID: &str = "herdr-initialize";

/// The `initialize` control request that registers the hook.
pub(super) fn initialize_request() -> Value {
    json!({
        "type": "control_request",
        "request_id": INITIALIZE_REQUEST_ID,
        "request": {
            "subtype": "initialize",
            "hooks": {
                "PreToolUse": [{"matcher": MATCHER, "hookCallbackIds": [CALLBACK_ID]}],
            },
        },
    })
}

/// The configured checks with `~` expanded in every argument, and why each
/// one left out was (an empty argv).
pub(super) fn expand(checks: &[Vec<String>]) -> (Vec<Vec<String>>, Vec<String>) {
    let mut expanded = Vec::new();
    let mut errors = Vec::new();
    for (index, argv) in checks.iter().enumerate() {
        if argv.first().is_none_or(|program| program.is_empty()) {
            errors.push(format!(
                "pre_tool_checks[{index}] has no program; it is left out"
            ));
            continue;
        }
        expanded.push(argv.iter().map(|arg| expand_tilde(arg)).collect());
    }
    (expanded, errors)
}

fn expand_tilde(arg: &str) -> String {
    if arg == "~" || arg.starts_with("~/") {
        crate::worktree::expand_tilde_path(arg)
            .display()
            .to_string()
    } else {
        arg.to_owned()
    }
}

/// What the checks decided about one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    Allow,
    /// The argv of the check that denied, and its reason.
    Deny(Vec<String>, String),
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckResult {
    Pass,
    Deny(String),
    /// It failed to give an answer; the call goes on.
    Failed(String),
}

/// The checks' verdict and each check that failed, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Outcome {
    pub(super) verdict: Verdict,
    pub(super) failures: Vec<(Vec<String>, String)>,
}

impl Outcome {
    /// The `hook_callback` answer: the denial, or no objection.
    pub(super) fn response(&self) -> Value {
        match &self.verdict {
            Verdict::Allow => json!({}),
            Verdict::Deny(_, reason) => denial(reason),
        }
    }
}

/// The `hook_callback` answer that denies the call with `reason`.
pub(super) fn denial(reason: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        },
    })
}

/// A request's running check: its process group, once it runs, and whether
/// the CLI cancelled the request.
#[derive(Debug, Default)]
struct Running {
    pid: Option<u32>,
    cancelled: bool,
}

/// One worker's checks.
#[derive(Debug, Clone)]
pub(super) struct Runner {
    checks: Arc<Vec<Vec<String>>>,
    cwd: PathBuf,
    running: Arc<Mutex<HashMap<String, Running>>>,
}

impl Runner {
    pub(super) fn new(checks: Vec<Vec<String>>, cwd: &Path) -> Self {
        Self {
            checks: Arc::new(checks),
            cwd: cwd.to_owned(),
            running: Arc::default(),
        }
    }

    /// Whether a check runs for `tool_name`.
    pub(super) fn checks_tool(&self, tool_name: &str) -> bool {
        !self.checks.is_empty() && CHECKED_TOOLS.contains(&tool_name)
    }

    /// Runs the checks for the `hook_callback` request `request_id` with
    /// `input`, the hook input, until one denies.
    pub(super) fn run(&self, request_id: &str, input: &Value) -> Outcome {
        let stdin = format!("{input}\n");
        lock(&self.running).insert(request_id.to_owned(), Running::default());
        let mut failures = Vec::new();
        let mut verdict = Verdict::Allow;
        for argv in self.checks.iter() {
            match self.run_one(request_id, argv, stdin.as_bytes()) {
                CheckResult::Pass => {}
                CheckResult::Deny(reason) => {
                    verdict = Verdict::Deny(argv.clone(), reason);
                    break;
                }
                CheckResult::Failed(error) => failures.push((argv.clone(), error)),
            }
            if lock(&self.running)
                .get(request_id)
                .is_some_and(|running| running.cancelled)
            {
                break;
            }
        }
        lock(&self.running).remove(request_id);
        Outcome { verdict, failures }
    }

    /// The CLI cancelled `request_id`: kills its running check. Whether a
    /// check was running for it.
    pub(super) fn cancel(&self, request_id: &str) -> bool {
        let mut running = lock(&self.running);
        let Some(entry) = running.get_mut(request_id) else {
            return false;
        };
        entry.cancelled = true;
        if let Some(pid) = entry.pid {
            let _ = crate::platform::signal_process_group(pid, Signal::Kill);
        }
        true
    }

    /// The worker ended: kills every running check.
    pub(super) fn cancel_all(&self) {
        let ids: Vec<String> = lock(&self.running).keys().cloned().collect();
        for id in ids {
            self.cancel(&id);
        }
    }

    fn run_one(&self, request_id: &str, argv: &[String], stdin: &[u8]) -> CheckResult {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(&self.cwd)
            .env("CLAUDE_PROJECT_DIR", &self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("HERDR_") {
                command.env_remove(key);
            }
        }
        // Its own process group, so a cancel ends what it started too.
        crate::platform::configure_worker_process(&mut command);
        let mut child = {
            let mut running = lock(&self.running);
            if running
                .get(request_id)
                .is_some_and(|running| running.cancelled)
            {
                return CheckResult::Failed(cancelled_message());
            }
            let child = match command.spawn() {
                Ok(child) => child,
                Err(error) => return CheckResult::Failed(format!("cannot start: {error}")),
            };
            if let Some(entry) = running.get_mut(request_id) {
                entry.pid = Some(child.id());
            }
            child
        };
        // Written from a thread, so a check that does not read its input
        // cannot hold up reading its output.
        let writer = child.stdin.take().map(|mut pipe| {
            let input = stdin.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&input);
            })
        });
        let stderr_reader = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut buffer = Vec::new();
                let _ = pipe.read_to_end(&mut buffer);
                buffer
            })
        });
        let mut stdout = Vec::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_end(&mut stdout);
        }
        let stderr = stderr_reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default();
        // Its output is closed: from here on a cancel no longer signals its
        // group, whose id the system may reuse once it is reaped.
        let cancelled = {
            let mut running = lock(&self.running);
            running.get_mut(request_id).is_some_and(|entry| {
                entry.pid = None;
                entry.cancelled
            })
        };
        let status = child.wait();
        if let Some(writer) = writer {
            let _ = writer.join();
        }
        if cancelled {
            return CheckResult::Failed(cancelled_message());
        }
        let stderr = String::from_utf8_lossy(&stderr).trim().to_owned();
        match status {
            Err(error) => CheckResult::Failed(format!("cannot wait for it: {error}")),
            Ok(status) => match status.code() {
                Some(0) => judge_output(&String::from_utf8_lossy(&stdout)),
                Some(2) if stderr.is_empty() => {
                    CheckResult::Deny(format!("{} denied the call", argv[0]))
                }
                Some(2) => CheckResult::Deny(stderr),
                Some(code) => {
                    CheckResult::Failed(with_stderr(format!("exited with code {code}"), &stderr))
                }
                None => CheckResult::Failed(with_stderr(
                    match crate::platform::exit_status_signal(&status) {
                        Some(signal) => format!("ended by signal {signal}"),
                        None => "ended without an exit code".to_owned(),
                    },
                    &stderr,
                )),
            },
        }
    }
}

fn cancelled_message() -> String {
    "the CLI cancelled the hook before the check exited (its own hook timeout, or the turn \
     ended); the check was killed"
        .to_owned()
}

fn with_stderr(what: String, stderr: &str) -> String {
    match stderr.lines().last() {
        Some(line) => format!("{what}: {line}"),
        None => what,
    }
}

/// A check that exited 0: a JSON object on stdout may deny; other text is no
/// objection, as Claude Code treats it.
fn judge_output(stdout: &str) -> CheckResult {
    let text = stdout.trim();
    if !text.starts_with('{') {
        return CheckResult::Pass;
    }
    let output: Value = match serde_json::from_str(text) {
        Ok(output) => output,
        Err(error) => return CheckResult::Failed(format!("printed broken JSON: {error}")),
    };
    let specific = &output["hookSpecificOutput"];
    if specific["permissionDecision"].as_str() == Some("deny") {
        return CheckResult::Deny(
            specific["permissionDecisionReason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("a pre-tool check denied the call")
                .to_owned(),
        );
    }
    if output["decision"].as_str() == Some("block") {
        return CheckResult::Deny(
            output["reason"]
                .as_str()
                .filter(|reason| !reason.is_empty())
                .unwrap_or("a pre-tool check blocked the call")
                .to_owned(),
        );
    }
    CheckResult::Pass
}

#[cfg(test)]
mod tests {
    use super::*;

    // The checks below run stub scripts through `sh`, `mkfifo` and the
    // executable bit, which only Unix has.
    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// Denies a Bash `sleep 5`, allows everything else.
        const DELAY_CHECK: &str = r#"#!/usr/bin/env python3
import json, sys
call = json.load(sys.stdin)
if "sleep 5" in call.get("tool_input", {}).get("command", ""):
    json.dump({"hookSpecificOutput": {"hookEventName": "PreToolUse",
               "permissionDecision": "deny",
               "permissionDecisionReason": "sleep 5 waits for nothing"}}, sys.stdout)
"#;

        struct Dir(PathBuf);

        impl Dir {
            fn new(name: &str) -> Self {
                let dir = std::env::temp_dir().join(format!(
                    "herdr-pre-tool-checks-{name}-{}",
                    std::process::id()
                ));
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir).unwrap();
                Self(dir)
            }

            fn script(&self, name: &str, text: &str) -> String {
                let path = self.0.join(name);
                std::fs::write(&path, text).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
                path.display().to_string()
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        fn bash(command: &str) -> Value {
            json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
                   "tool_input": {"command": command}})
        }

        #[test]
        fn a_denying_check_refuses_the_call_with_its_reason() {
            let dir = Dir::new("deny");
            let check = vec![dir.script("delay-check.py", DELAY_CHECK)];
            let runner = Runner::new(vec![check.clone()], &dir.0);

            let denied = runner.run("r1", &bash("sleep 5 && ls"));
            assert_eq!(
                denied.verdict,
                Verdict::Deny(check, "sleep 5 waits for nothing".into())
            );
            assert!(denied.failures.is_empty());
            assert_eq!(
                denied.response()["hookSpecificOutput"]["permissionDecision"],
                "deny"
            );

            let allowed = runner.run("r2", &bash("ls"));
            assert_eq!(allowed.verdict, Verdict::Allow);
            assert!(allowed.failures.is_empty());
            // No objection is not an allow: herdr's policy still decides.
            assert_eq!(allowed.response(), json!({}));
        }

        #[test]
        fn exit_code_two_denies_with_stderr_and_the_first_denial_wins() {
            let dir = Dir::new("exit2");
            let first = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "cat >/dev/null; echo 'blocked by rule' >&2; exit 2".to_owned(),
            ];
            let marker = dir.0.join("second-ran");
            let second = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("touch {}", marker.display()),
            ];
            let runner = Runner::new(vec![first.clone(), second], &dir.0);
            let outcome = runner.run("r1", &bash("ls"));
            assert_eq!(
                outcome.verdict,
                Verdict::Deny(first, "blocked by rule".into())
            );
            assert!(!marker.exists(), "a check after the denial ran");
        }

        #[test]
        fn a_check_that_fails_lets_the_call_go_on_and_reports_why() {
            let dir = Dir::new("fail");
            let missing = vec![dir.0.join("missing-check").display().to_string()];
            let crashing = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo boom >&2; exit 1".to_owned(),
            ];
            let broken = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo '{not json'".to_owned(),
            ];
            let runner = Runner::new(
                vec![missing.clone(), crashing.clone(), broken.clone()],
                &dir.0,
            );
            let outcome = runner.run("r1", &bash("sleep 5"));
            assert_eq!(outcome.verdict, Verdict::Allow);
            let failed: Vec<_> = outcome
                .failures
                .iter()
                .map(|(argv, _)| argv.clone())
                .collect();
            assert_eq!(failed, vec![missing, crashing, broken]);
            assert!(
                outcome.failures[0].1.starts_with("cannot start"),
                "{:?}",
                outcome.failures
            );
            assert_eq!(outcome.failures[1].1, "exited with code 1: boom");
            assert!(outcome.failures[2].1.starts_with("printed broken JSON"));
        }

        #[test]
        fn a_cancelled_check_is_killed_and_reported() {
            let dir = Dir::new("cancel");
            let fifo = dir.0.join("started");
            assert!(Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success());
            // Tells the test it runs, then never exits on its own.
            let hung = vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!("echo up > {}; exec sleep 1000", fifo.display()),
            ];
            let runner = Runner::new(vec![hung], &dir.0);
            let running = runner.clone();
            let thread = std::thread::spawn(move || running.run("r1", &bash("ls")));
            // Opening the fifo returns once the check opened it to write.
            std::fs::read_to_string(&fifo).unwrap();
            assert!(runner.cancel("r1"));
            let outcome = thread.join().unwrap();
            assert_eq!(outcome.verdict, Verdict::Allow);
            assert_eq!(outcome.failures.len(), 1);
            assert!(
                outcome.failures[0].1.contains("cancelled"),
                "{:?}",
                outcome.failures
            );
            assert!(!runner.cancel("r1"), "the request is still tracked");
        }
    }

    #[test]
    fn tildes_expand_and_empty_checks_are_left_out() {
        let (checks, errors) = expand(&[
            vec!["python3".into(), "~/.claude/hooks/delay-check.py".into()],
            Vec::new(),
            vec![String::new()],
        ]);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0][0], "python3");
        assert!(!checks[0][1].starts_with('~'), "{checks:?}");
        assert!(checks[0][1].ends_with("/.claude/hooks/delay-check.py"));
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn the_initialize_request_registers_the_hook_for_every_tool_herdr_decides() {
        let request = initialize_request();
        assert_eq!(request["request"]["subtype"], "initialize");
        let entry = &request["request"]["hooks"]["PreToolUse"][0];
        let matcher = entry["matcher"].as_str().unwrap();
        let tools: Vec<&str> = matcher.split('|').collect();
        for tool in CHECKED_TOOLS
            .iter()
            .chain(super::super::policy::BACKGROUND_WAIT_TOOLS)
            .chain(
                super::super::policy::BACKGROUND_CAPABLE_TOOLS
                    .iter()
                    .map(|(tool, _)| tool),
            )
        {
            assert!(tools.contains(tool), "{tool} missing from {matcher}");
        }
        assert_eq!(entry["hookCallbackIds"], json!([CALLBACK_ID]));
    }
}
