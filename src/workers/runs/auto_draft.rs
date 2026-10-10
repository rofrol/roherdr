//! A run whose worker's task the server drafts itself (`todo.draft_run`,
//! `herdr todo run --draft`): at its `draft` step the driver asks a model
//! in one bounded, stateless call ([`super::decision`]) with the item's text
//! as the claim recorded it, the `DECISIONS.md` sections the item names,
//! the repository's code, test and commit rules (`AGENTS.md`), its
//! registered checks, `git log -20` of the run's base and the item's
//! history, and applies the typed decision it returns:
//!
//! - `draft` (the task text, the exact commit subject, the path globs and
//!   the check names): checked as `todo.run` checks its parameters (a
//!   lowercase conventional subject, relative globs that stay inside the
//!   repository, each check registered and named once), then written to
//!   the run, which starts its worker as if the coordinator had passed them
//!   (`run_drafted`);
//! - `escalate` (a question for the user with options): a `draft` event the
//!   run waits on, a notice to the user and an entry in the user's `?` list
//!   ([`super::escalations`]). The user's answer there, or the
//!   coordinator's `todo.resume --action retry --task <answer>`, records the
//!   answer (`run_draft_answer`) and takes the run back to its draft step,
//!   whose next call gets every answer so far.
//!
//! A call that fails or returns a draft the server refuses is made once
//! more, its input naming why the earlier one was refused; a second failure
//! is escalated. Each call (`run_draft_call`) and the decision of each
//! round (`run_draft_decision`, with its id, input digest, output and
//! model; a round per answer) are recorded before the decision is applied,
//! so a server that ends between them applies the recorded decision when it
//! starts instead of asking again. The application is one transaction that
//! first checks the run is still at its draft step: an abort that came
//! first wins.
//!
//! The call's process exit is the event the driver waits for. It has no
//! deadline: the provider imposes none on a `claude -p` call.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::auto_review::{escalation_options, nonempty, only};
use super::decision::{self, Prompt, MAX_CALLS};
use super::escalations;
use super::{
    check_message, check_paths, contract_check_of, git, new_event, read_checks, registered_checks,
    ChecksFile, Run, RunCheck, CHECKS_FILE, DECISIONS_FILE,
};
use crate::api::schema::{TodoDraft, TodoEventKind, TodoRunStatus, TodoStep};
use crate::workers::{now_ms, WorkerSupervisor};

/// The run events this module writes.
const CALL: &str = "run_draft_call";
const DECISION: &str = "run_draft_decision";
const APPLIED: &str = "run_drafted";
/// An answer to a draft's question, by the user or the coordinator: each
/// one starts a new round of the draft.
pub(super) const ANSWER: &str = "run_draft_answer";
/// The file whose code, test and commit rules the model gets.
const AGENTS_FILE: &str = "AGENTS.md";
/// The headings of `AGENTS.md` whose sections the model gets: those whose
/// title holds one of these, lowercased.
const RULE_HEADINGS: [&str; 3] = ["test", "code convention", "commit"];
/// A section the model gets is cut at this many characters, and the
/// sections of one file stop once they reach [`SECTIONS_MAX`].
const SECTION_MAX: usize = 12_000;
const SECTIONS_MAX: usize = 60_000;
/// A history record's text is cut at this many characters.
const HISTORY_TEXT_MAX: usize = 2_000;

const SYSTEM_PROMPT: &str = "You draft the task of a headless coding worker that herdr's TODO \
driver starts for one TODO item of a repository. The input is JSON: the item's text from \
TODO.md, the DECISIONS.md sections it names, the repository's code, test and commit rules from \
AGENTS.md, its registered checks (.herdr/checks.toml), the last 20 commits of master, the item's \
history, and the user's answers to your earlier questions. Decide one action and answer only \
with JSON matching the schema:\n\
- draft when the item can be done as it stands: `task` says what the worker must change and \
test, what to read first and what not to touch (the driver appends the commit subject, the \
paths, the checks and the worker's last line itself: do not repeat them); `message` is the \
exact commit subject, a lowercase conventional subject (`type: description` or `type(scope): \
description`, one line) in the style of the recent commits; `paths` are relative git glob \
pathspecs the commit may touch, inside the repository (no absolute path, no `..`, no `:`); \
`checks` are names of registered checks the verify runs, in order, only names the input lists;\n\
- escalate (question, 2 to 4 options, the recommended first) when the item needs the user \
first: a choice it leaves open, a permission or an account, a contradiction with a decision, \
or it is done or outdated.\n\
When the input has `rejected_drafts`, your earlier answers were refused for those reasons: fix \
them.";

/// The model's decision, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Draft {
        task: String,
        message: String,
        paths: Vec<String>,
        checks: Vec<String>,
    },
    Escalate {
        question: String,
        options: Vec<String>,
    },
}

impl Decision {
    fn to_json(&self) -> Value {
        match self {
            Decision::Draft {
                task,
                message,
                paths,
                checks,
            } => json!({
                "action": "draft", "task": task, "message": message, "paths": paths,
                "checks": checks,
            }),
            Decision::Escalate { question, options } => {
                json!({"action": "escalate", "question": question, "options": options})
            }
        }
    }
}

/// The output's JSON schema, which `claude -p --json-schema` enforces and
/// [`parse_decision`] checks again: the checks are the registered names.
pub(super) fn output_schema(checks: &ChecksFile) -> String {
    let names: Vec<&String> = checks.checks.keys().collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["action"],
        "properties": {
            "action": {"type": "string", "enum": ["draft", "escalate"]},
            "task": {"type": "string", "description": "draft only: the worker's task"},
            "message": {"type": "string", "description": "draft only: the exact commit subject, a lowercase conventional subject"},
            "paths": {"type": "array", "items": {"type": "string"}, "minItems": 1, "description": "draft only: relative git glob pathspecs the commit may touch"},
            "checks": {"type": "array", "items": {"type": "string", "enum": names}, "minItems": 1, "description": "draft only: the registered checks the verify runs, in order"},
            "question": {"type": "string", "description": "escalate only: the question for the user"},
            "options": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 4, "description": "escalate only: the answers the user can choose from, the recommended one first"},
        },
    })
    .to_string()
}

/// The output as the schema allows it, before the per-action checks.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecision {
    action: String,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    checks: Option<Vec<String>>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    options: Option<Vec<String>>,
}

/// Checks the model's output: the schema, that each action has its own
/// fields and only those, and that a draft is one `todo.run` would take:
/// the subject's shape, relative globs inside the repository, registered
/// checks named once.
pub(super) fn parse_decision(output: &Value, checks: &ChecksFile) -> Result<Decision, String> {
    let raw: RawDecision = serde_json::from_value(output.clone())
        .map_err(|error| format!("the output does not match the schema: {error}"))?;
    let action = raw.action.as_str();
    match action {
        "draft" => {
            only(
                action,
                &[
                    ("question", raw.question.is_some()),
                    ("options", raw.options.is_some()),
                ],
            )?;
            let task = nonempty(action, raw.task, "task")?;
            let message = raw.message.unwrap_or_default();
            check_message(&message).map_err(|why| format!("draft: {why}"))?;
            let paths = raw.paths.unwrap_or_default();
            check_paths(&paths).map_err(|why| format!("draft: {why}"))?;
            let names = raw.checks.unwrap_or_default();
            registered_checks(checks, &names).map_err(|why| format!("draft: {why}"))?;
            Ok(Decision::Draft {
                task,
                message,
                paths,
                checks: names,
            })
        }
        "escalate" => {
            only(
                action,
                &[
                    ("task", raw.task.is_some()),
                    ("message", raw.message.is_some()),
                    ("paths", raw.paths.is_some()),
                    ("checks", raw.checks.is_some()),
                ],
            )?;
            let question = nonempty(action, raw.question, "question")?;
            let options = escalation_options(raw.options)?;
            Ok(Decision::Escalate { question, options })
        }
        other => Err(format!("action {other:?} is not draft or escalate")),
    }
}

/// A decision as `run_draft_decision` records it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DecisionRecord {
    decision_id: String,
    /// The round decided: how many answers the draft had before it.
    round: u32,
    /// SHA-256 of the input the last call got, in hex.
    input_digest: String,
    model: Option<String>,
    /// The checked decision; none when every call failed.
    output: Option<Value>,
    /// Each failed or refused call's error.
    #[serde(default)]
    errors: Vec<String>,
}

/// A checked draft with its checks' argv, as the run takes it.
struct Drafted {
    task: String,
    message: String,
    paths: Vec<String>,
    registered: Vec<RunCheck>,
}

/// One markdown section: its heading's level and title, and its text with
/// the heading.
struct Section {
    level: usize,
    title: String,
    start: usize,
    end: usize,
}

/// The sections of a markdown text (headings inside code fences do not
/// count), each up to the next heading of its level or above.
fn sections(lines: &[&str]) -> Vec<Section> {
    let mut headings = Vec::new();
    let mut fenced = false;
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        if level == 0 || !line[level..].starts_with(' ') {
            continue;
        }
        headings.push((index, level, line[level..].trim().to_owned()));
    }
    headings
        .iter()
        .enumerate()
        .map(|(at, (start, level, title))| {
            let end = headings[at + 1..]
                .iter()
                .find(|(_, other, _)| other <= level)
                .map_or(lines.len(), |(index, _, _)| *index);
            Section {
                level: *level,
                title: title.clone(),
                start: *start,
                end,
            }
        })
        .collect()
}

/// Lowercased, with every run of whitespace one space.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The chosen sections as the model gets them, outermost only, each cut
/// at [`SECTION_MAX`] characters, until they reach [`SECTIONS_MAX`].
fn section_texts(lines: &[&str], chosen: Vec<&Section>) -> Vec<Value> {
    let mut taken: Vec<(usize, usize)> = Vec::new();
    let mut total = 0;
    let mut texts = Vec::new();
    for section in chosen {
        if taken
            .iter()
            .any(|(start, end)| section.start >= *start && section.end <= *end)
        {
            continue;
        }
        if total >= SECTIONS_MAX {
            break;
        }
        let text = lines[section.start..section.end].join("\n");
        let text = match text.char_indices().nth(SECTION_MAX) {
            Some((at, _)) => format!("{}\n[cut]", &text[..at]),
            None => text,
        };
        total += text.len();
        taken.push((section.start, section.end));
        texts.push(json!({"title": section.title, "text": text.trim_end()}));
    }
    texts
}

/// The `DECISIONS.md` sections the item names: those whose title its text
/// holds (8 characters at least), or that hold its id.
pub(super) fn decisions_named(decisions: &str, item_text: &str, item: &str) -> Vec<Value> {
    let lines: Vec<&str> = decisions.lines().collect();
    let all = sections(&lines);
    let text = normalized(item_text);
    let chosen = all
        .iter()
        .filter(|section| section.level == 2)
        .filter(|section| {
            let title = normalized(&section.title);
            (title.chars().count() >= 8 && text.contains(&title))
                || lines[section.start..section.end]
                    .iter()
                    .any(|line| line.contains(item))
        })
        .collect();
    section_texts(&lines, chosen)
}

/// The `AGENTS.md` sections on code, tests and commits.
pub(super) fn agents_rules(agents: &str) -> Vec<Value> {
    let lines: Vec<&str> = agents.lines().collect();
    let all = sections(&lines);
    let chosen = all
        .iter()
        .filter(|section| section.level >= 2)
        .filter(|section| {
            let title = section.title.to_lowercase();
            RULE_HEADINGS.iter().any(|heading| title.contains(heading))
        })
        .collect();
    section_texts(&lines, chosen)
}

fn cut_text(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text.to_owned(),
    }
}

impl WorkerSupervisor {
    /// The run's draft step: the decision recorded for this round, else
    /// the model's, recorded first; then applies it. A run that was aborted
    /// meanwhile is left as it is.
    pub(super) fn step_draft(&self, run: &mut Run) -> Result<(), String> {
        let store = self.run_store().map_err(|error| error.to_string())?;
        let answers = store
            .run_events_of(&run.info.run_id, ANSWER)
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let round = answers.len() as u32;
        let checks = read_checks(Path::new(&run.info.repo))?;
        let recorded = store
            .run_events_of(&run.info.run_id, DECISION)
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .filter_map(|body| serde_json::from_value::<DecisionRecord>(body).ok())
            .find(|record| record.round == round);
        let record = match recorded {
            // Decided before this driver: applied now, never asked again.
            Some(record) => record,
            None => {
                let record = self.decide_draft(run, round, &answers, &checks)?;
                #[cfg(test)]
                super::crashes_after(&run.info.repo, TodoStep::Draft)?;
                record
            }
        };
        // A handoff during the call: the new server applies the record.
        if self.handed_off() {
            return Ok(());
        }
        self.apply_draft(run, &record, &checks)
    }

    /// Asks the model for the round's draft (at most [`MAX_CALLS`] calls,
    /// counting those a previous server made, taking a recorded answer to
    /// the same input without asking again) and records each call and the
    /// decision.
    fn decide_draft(
        &self,
        run: &Run,
        round: u32,
        answers: &[Value],
        checks: &ChecksFile,
    ) -> Result<DecisionRecord, String> {
        let base = self.draft_input(run, answers, checks);
        let schema = output_schema(checks);
        let prompt = Prompt {
            schema: &schema,
            system: SYSTEM_PROMPT,
            intro: "Draft the worker's task for this item and answer with the JSON decision only.",
        };
        let store = self.run_store().map_err(|error| error.to_string())?;
        let calls = store
            .run_events_of(&run.info.run_id, CALL)
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let key = json!(round);
        let (mut errors, _) = decision::recorded_calls(&calls, ("round", &key), "");
        let mut decided = None;
        let mut digest;
        loop {
            // A call after a refused one learns why it was refused.
            let mut input = base.clone();
            if !errors.is_empty() {
                input["rejected_drafts"] = json!(errors);
            }
            let text = serde_json::to_string_pretty(&input).map_err(|error| error.to_string())?;
            digest = decision::digest(&text);
            let (_, answered) = decision::recorded_calls(&calls, ("round", &key), &digest);
            if let Some((decision, model)) = answered.and_then(|(output, model)| {
                parse_decision(&output, checks)
                    .ok()
                    .map(|decision| (decision, model))
            }) {
                decided = Some((decision, model));
                break;
            }
            if errors.len() >= MAX_CALLS {
                break;
            }
            let call = errors.len() + 1;
            let answered = self
                .call_model(&prompt, &text)
                .and_then(|(output, model)| Ok((parse_decision(&output, checks)?, model)));
            let note = match &answered {
                Ok((decision, model)) => json!({
                    "type": CALL, "round": round, "call": call, "input_digest": digest,
                    "model": model, "output": decision.to_json(),
                }),
                Err(error) => json!({
                    "type": CALL, "round": round, "call": call, "input_digest": digest,
                    "error": error,
                }),
            };
            self.write_note(&run.info.run_id, &note)?;
            match answered {
                Ok(answer) => {
                    decided = Some(answer);
                    break;
                }
                Err(error) => errors.push(error),
            }
        }
        let (output, model) = match decided {
            Some((decision, model)) => (Some(decision.to_json()), model),
            None => (None, None),
        };
        let record = DecisionRecord {
            decision_id: decision::decision_id(
                &run.info.run_id,
                &format!("draft:{round}"),
                &digest,
            ),
            round,
            input_digest: digest,
            model,
            output,
            errors,
        };
        let mut body = serde_json::to_value(&record).map_err(|error| error.to_string())?;
        body["type"] = json!(DECISION);
        self.write_note(&run.info.run_id, &body)?;
        Ok(record)
    }

    /// What the model drafts from: the item's text as the claim recorded
    /// it, the files of the run's base (the `DECISIONS.md` sections it
    /// names, the rules of `AGENTS.md`, `git log -20`), the registered
    /// checks, the item's history before this run and the answers so far.
    fn draft_input(&self, run: &Run, answers: &[Value], checks: &ChecksFile) -> Value {
        let repo = Path::new(&run.info.repo);
        let item_text = self
            .run_store()
            .ok()
            .and_then(|store| store.run_events_of(&run.info.run_id, "run_created").ok())
            .and_then(|created| created.into_iter().next())
            .and_then(|created| created["item_text"].as_str().map(str::to_owned))
            .unwrap_or_default();
        let base = run.info.base.clone().unwrap_or_else(|| "master".into());
        let at_base = |path: &str| git(repo, &["show", &format!("{base}:{path}")]).ok();
        let decisions = at_base(DECISIONS_FILE)
            .map(|text| decisions_named(&text, &item_text, &run.info.item))
            .unwrap_or_default();
        let rules = at_base(AGENTS_FILE)
            .map(|text| agents_rules(&text))
            .unwrap_or_default();
        let log = git(repo, &["log", "-20", "--format=%h %s", &base])
            .unwrap_or_else(|error| format!("(the log could not be read: {error})"));
        let checks_text = std::fs::read_to_string(repo.join(CHECKS_FILE)).unwrap_or_default();
        let history: Vec<Value> = self
            .run_store()
            .ok()
            .and_then(|store| {
                store
                    .item_history(Some(&run.info.repo), Some(&run.info.item))
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|stored| stored.event)
            .filter(|event| event.run_id.as_deref() != Some(run.info.run_id.as_str()))
            .map(|event| {
                json!({
                    "kind": event.kind, "ts_ms": event.ts_ms, "run_id": event.run_id,
                    "attempt": event.attempt,
                    "text": event.text.map(|text| cut_text(&text, HISTORY_TEXT_MAX)),
                })
            })
            .collect();
        let answers: Vec<Value> = answers
            .iter()
            .map(|answer| {
                json!({
                    "question": answer["question"], "options": answer["options"],
                    "answer": answer["answer"], "by": answer["by"],
                })
            })
            .collect();
        json!({
            "item": {"id": run.info.item, "text": item_text},
            "decisions": decisions,
            "agents_rules": rules,
            "checks_file": checks_text,
            "registered_checks": checks.checks.keys().collect::<Vec<_>>(),
            "git_log": log,
            "item_history": history,
            "answers": answers,
        })
    }

    /// Applies the recorded decision: a draft the run's checks file still
    /// takes starts the run with it; anything else asks the user.
    fn apply_draft(
        &self,
        run: &mut Run,
        record: &DecisionRecord,
        checks: &ChecksFile,
    ) -> Result<(), String> {
        let decision = record
            .output
            .as_ref()
            .map(|output| parse_decision(output, checks))
            .transpose();
        let (question, options) = match decision {
            Ok(Some(Decision::Draft {
                task,
                message,
                paths,
                checks: names,
            })) => {
                let drafted = Drafted {
                    registered: registered_checks(checks, &names)?,
                    task,
                    message,
                    paths,
                };
                return self.start_drafted(run, record, drafted, checks);
            }
            Ok(Some(Decision::Escalate { question, options })) => (question, options),
            Ok(None) => (
                format!(
                    "The task draft failed {} times ({}); answer with what the task should be, \
                     or abort the run.",
                    record.errors.len(),
                    record.errors.join("; ")
                ),
                draft_options(),
            ),
            Err(error) => (
                format!(
                    "The recorded task draft is not one this run takes now ({error}); answer \
                     with what the task should be, or abort the run."
                ),
                draft_options(),
            ),
        };
        self.escalate_draft(run, record, &question, &options)
    }

    /// Writes the draft to the run, in one transaction while it is still
    /// at its draft step, and moves it to its start.
    fn start_drafted(
        &self,
        run: &mut Run,
        record: &DecisionRecord,
        drafted: Drafted,
        checks: &ChecksFile,
    ) -> Result<(), String> {
        let Drafted {
            task,
            message,
            paths,
            registered,
        } = drafted;
        let contract_check = contract_check_of(checks, &registered);
        let names: Vec<String> = registered.iter().map(|check| check.name.clone()).collect();
        let body = json!({
            "type": APPLIED, "decision_id": record.decision_id, "round": record.round,
            "model": record.model, "task": task, "message": message, "paths": paths,
            "checks": registered, "contract_check": contract_check,
        });
        let store = self.run_store().map_err(|error| error.to_string())?;
        let applied = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Running
                    || current.info.step != TodoStep::Draft
                {
                    return Ok(None);
                }
                current.info.task = task.clone();
                current.info.message = message.clone();
                current.info.paths = paths.clone();
                current.info.checks = names.clone();
                current.checks = registered.clone();
                current.finish.contract_check = contract_check.clone();
                current.info.step = TodoStep::Start;
                tx.run_event(&mut current, &body, false, now_ms())?;
                Ok(Some(current))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        if let Some(current) = applied {
            *run = current;
            super::announce();
        }
        Ok(())
    }

    /// A `draft` event with the question, which the run waits on, a notice
    /// to the user and an entry in the user's `?` list.
    fn escalate_draft(
        &self,
        run: &mut Run,
        record: &DecisionRecord,
        question: &str,
        options: &[String],
    ) -> Result<(), String> {
        let mut event = new_event(TodoEventKind::Draft);
        event.error = Some(format!(
            "The task draft ({}) asks: {question}\nOptions: {}",
            record.decision_id,
            options.join(" | ")
        ));
        let store = self.run_store().map_err(|error| error.to_string())?;
        let escalated = store
            .transaction(|tx| {
                let Some(mut current) = tx.run(&run.info.run_id)? else {
                    return Ok(None);
                };
                if current.info.status != TodoRunStatus::Running
                    || current.info.step != TodoStep::Draft
                {
                    return Ok(None);
                }
                current.info.status = TodoRunStatus::Waiting;
                let mut body = json!({
                    "type": "run_event",
                    "kind": event.kind,
                    "step": current.info.step,
                    "status": current.info.status,
                    "attempt": current.info.attempt,
                    "event": event,
                    "question": question,
                    "options": options,
                });
                body[super::auto_review::ESCALATES] = json!(record.decision_id);
                let raised = tx.run_event(&mut current, &body, true, now_ms())?;
                Ok(Some((current, raised, body)))
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        let Some((current, raised, body)) = escalated else {
            return Ok(());
        };
        *run = current;
        super::announce();
        if let Some(escalation) = escalations::escalation_of(run, raised, &body) {
            escalations::list(escalation);
        }
        crate::workers::notify_user(crate::workers::UserNotice {
            title: format!(
                "{}: the task draft asks you (run {})",
                run.info.item, run.info.run_id
            ),
            body: format!("{question}\nOptions: {}", options.join(" | ")),
        });
        Ok(())
    }

    /// The draft the run started with, as `todo.review` shows it.
    pub(in crate::workers) fn applied_draft(&self, run_id: &str) -> Option<TodoDraft> {
        let applied = self
            .run_store()
            .ok()?
            .run_events_of(run_id, APPLIED)
            .ok()?
            .pop()?;
        let text = |key: &str| applied[key].as_str().map(str::to_owned);
        let list = |key: &str, field: Option<&str>| -> Vec<String> {
            applied[key]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| match field {
                            Some(field) => value[field].as_str(),
                            None => value.as_str(),
                        })
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        Some(TodoDraft {
            decision_id: text("decision_id")?,
            model: text("model"),
            task: text("task")?,
            message: text("message")?,
            paths: list("paths", None),
            checks: list("checks", Some("name")),
        })
    }
}

/// The record of an answer to a draft's question (`event`), which takes the
/// run back to its draft step.
pub(super) fn answer_note(event: i64, body: Option<&Value>, answer: &str, by: &str) -> Value {
    let field = |key: &str| body.map_or(Value::Null, |body| body[key].clone());
    json!({
        "type": ANSWER, "event": event, "decision_id": field(super::auto_review::ESCALATES),
        "question": field("question"), "options": field("options"), "answer": answer, "by": by,
    })
}

/// The choices a draft the model could not settle leaves the user.
fn draft_options() -> Vec<String> {
    vec![
        "draft again".into(),
        "draft again with my guidance (type it)".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks() -> ChecksFile {
        toml::from_str("[checks]\nworkers = [\"cargo\", \"test\"]\ntests = [\"cargo\"]\n").unwrap()
    }

    #[test]
    fn a_draft_is_checked_as_todo_run_checks_its_parameters() {
        let draft = json!({
            "action": "draft", "task": "do it", "message": "feat: add a",
            "paths": ["src/**"], "checks": ["workers"],
        });
        assert_eq!(
            parse_decision(&draft, &checks()),
            Ok(Decision::Draft {
                task: "do it".into(),
                message: "feat: add a".into(),
                paths: vec!["src/**".into()],
                checks: vec!["workers".into()],
            })
        );
        let escalate = json!({"action": "escalate", "question": "q?", "options": ["a", "b"]});
        assert!(matches!(
            parse_decision(&escalate, &checks()),
            Ok(Decision::Escalate { .. })
        ));
        let with = |key: &str, value: Value| {
            let mut draft = draft.clone();
            draft[key] = value;
            draft
        };
        for bad in [
            json!({"action": "merge"}),
            with("message", json!("Feat: Add a")),
            with("message", json!("add a")),
            with("task", json!(" ")),
            with("paths", json!([])),
            with("paths", json!(["../outside/**"])),
            with("paths", json!(["/etc/passwd"])),
            with("paths", json!([":(top)src"])),
            with("checks", json!([])),
            with("checks", json!(["unregistered"])),
            with("checks", json!(["workers", "workers"])),
            with("question", json!("q?")),
            with("extra", json!(1)),
            json!({"action": "escalate", "question": "q?", "options": ["a"]}),
            json!({"action": "escalate", "question": "q?", "options": ["a", "b"], "task": "t"}),
        ] {
            assert!(parse_decision(&bad, &checks()).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_schema_offers_only_the_registered_checks() {
        let schema: Value = serde_json::from_str(&output_schema(&checks())).unwrap();
        assert_eq!(
            schema["properties"]["checks"]["items"]["enum"],
            json!(["tests", "workers"])
        );
        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["draft", "escalate"])
        );
    }

    #[test]
    fn the_decisions_an_item_names_are_its_sections_by_title_or_id() {
        let decisions = "# Decisions\n\n## From a per-item coordinator agent to typed \
                         decision calls\n\nThe user chose.\n\n## Unrelated\n\nNo.\n\n## Other \
                         section\n\nSee [t-abcd2345].\n\n```\n## not a heading\n```\n";
        let item = "Read DECISIONS.md \"From a per-item coordinator\n  agent to typed decision \
                    calls\" first.";
        let named = decisions_named(decisions, item, "t-abcd2345");
        let titles: Vec<&str> = named
            .iter()
            .map(|section| section["title"].as_str().unwrap())
            .collect();
        assert_eq!(
            titles,
            [
                "From a per-item coordinator agent to typed decision calls",
                "Other section"
            ]
        );
        assert!(named[1]["text"]
            .as_str()
            .unwrap()
            .contains("## not a heading"));
    }

    #[test]
    fn the_agents_rules_are_its_code_test_and_commit_sections() {
        let agents = "# herdr\n\n## Principles\n\nx\n\n## Testing\n\nRun just check.\n\n### \
                      Flaky tests\n\nStress them.\n\n## Commit Style\n\nLowercase.\n\n## Code \
                      Conventions\n\nNo unwrap.\n\n## Docs\n\ny\n";
        let rules = agents_rules(agents);
        let titles: Vec<&str> = rules
            .iter()
            .map(|section| section["title"].as_str().unwrap())
            .collect();
        // The nested "Flaky tests" goes with "Testing".
        assert_eq!(titles, ["Testing", "Commit Style", "Code Conventions"]);
        assert!(rules[0]["text"].as_str().unwrap().contains("Stress them."));
    }
}
