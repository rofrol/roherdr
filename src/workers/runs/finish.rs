//! The steps of a `herdr todo run` after its cherry-pick: the install, the
//! TODO update, the push and the cleanup. Each records its intent before its
//! side effect and its result after it, and asks the repository first
//! whether a driver a crash cut off already did it: the installed build is
//! the target branch's, the target branch has the TODO commit, the push
//! destination has the target branch. Each step reads the branch the run stored
//! ([`Run::target_branch`]) and stops when it no longer resolves. A failed
//! install, TODO edit or push is an event the run waits on; nothing is
//! forced, rebased or retried on its own.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::json;

use super::{
    command_with, git, git_with, new_event, tail, todo_titles, Run, CHECKS_FILE, DECISIONS_FILE,
    RUN_ENV, TODO_FILE,
};
use crate::api::schema::{
    HistoryEventKind, TodoEventKind, TodoNextRun, TodoRunInfo, TodoRunParams, TodoRunPush,
    TodoRunStatus, TodoStep, WorkerAgentChoice,
};
use crate::workers::todo_editor::{self, SCRIPT_NAME};
use crate::workers::{lock, now_ms, WorkerError, WorkerSupervisor};

/// Held while a stop notice is settled, so a run's follow-up and its
/// owner's going away never both send it.
static STOP_NOTICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The note's lines as `todo_edit.py append-to` takes them: indented, no
/// blank lines.
fn indented(note: &str) -> String {
    note.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            if line.starts_with([' ', '\t']) {
                format!("{line}\n")
            } else {
                format!("  {line}\n")
            }
        })
        .collect()
}

/// The decision's section title and body: its first line when that starts
/// with `#` (the `#`s dropped), otherwise `fallback` and the whole text.
fn decision_section(decision: &str, fallback: &str) -> (String, String) {
    let text = decision.trim_start_matches(['\n', '\r']);
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let (title, body) = if first.starts_with('#') {
        (
            first.trim_start_matches('#').trim().to_owned(),
            rest.trim_start_matches(['\n', '\r']),
        )
    } else {
        (fallback.to_owned(), text)
    };
    let mut body = body.trim_end().to_owned();
    body.push('\n');
    (title, body)
}

/// Whether the build id a `build_id` command printed is the target branch's commit:
/// its first word is that sha or a prefix of it (a dirty build, `<sha>~<tree>`,
/// is not).
fn build_is(build: &str, target: &str) -> bool {
    let word = build.split_whitespace().next().unwrap_or_default();
    word.len() >= 7 && !word.contains('~') && target.starts_with(word)
}

fn describe_exit(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exited {code}"),
        None => "was ended by a signal".into(),
    }
}

/// Runs `argv` without a shell in `dir` with `env`, its stdout and stderr
/// into `log`. A failure names its exit and the log's last lines.
fn run_logged(
    dir: &Path,
    argv: &[String],
    env: &HashMap<String, String>,
    log: &Path,
) -> Result<(), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("the command has no program".into());
    };
    let out = File::create(log)
        .map_err(|error| format!("cannot create the log {}: {error}", log.display()))?;
    let err = out
        .try_clone()
        .map_err(|error| format!("cannot open the log {}: {error}", log.display()))?;
    let status = command_with(program, Some(env))
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|error| format!("cannot run {argv:?}: {error}"))?;
    if status.success() {
        return Ok(());
    }
    let output = std::fs::read(log)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default();
    Err(format!(
        "{argv:?} {} (log {}):\n{}",
        describe_exit(status),
        log.display(),
        tail(&output)
    ))
}

/// The first line the `build_id` command prints.
fn installed_build(
    dir: &Path,
    argv: &[String],
    env: &HashMap<String, String>,
) -> Result<String, String> {
    let Some((program, args)) = argv.split_first() else {
        return Err("no build_id command".into());
    };
    let output = command_with(program, Some(env))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run {argv:?}: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let first = text.lines().map(str::trim).find(|line| !line.is_empty());
    match (output.status.success(), first) {
        (true, Some(line)) => Ok(line.to_owned()),
        (true, None) => Err(format!("{argv:?} printed nothing")),
        (false, _) => Err(format!(
            "{argv:?} {}: {}",
            describe_exit(output.status),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

/// The newest commit in `since..target` whose subject is `subject`.
fn commit_with_subject(
    repo: &Path,
    since: &str,
    target: &str,
    subject: &str,
) -> Result<Option<String>, String> {
    Ok(git(
        repo,
        &[
            "log",
            "--format=%H %s",
            &format!("{since}..refs/heads/{target}"),
        ],
    )?
    .lines()
    .find_map(|line| {
        let (sha, line_subject) = line.split_once(' ')?;
        (line_subject == subject).then(|| sha.to_owned())
    }))
}

/// The branches some worktree of the repository has checked out.
fn checked_out(repo: &Path) -> Result<BTreeSet<String>, String> {
    Ok(git(repo, &["worktree", "list", "--porcelain"])?
        .lines()
        .filter_map(|line| line.strip_prefix("branch refs/heads/"))
        .map(str::to_owned)
        .collect())
}

/// Whether every commit of `branch` has its change on `target` (`git
/// cherry` marks none `+`): a cherry-picked branch is merged though its
/// commits are not ancestors of `target`.
fn merged(repo: &Path, target: &str, branch: &str) -> bool {
    git(
        repo,
        &[
            "cherry",
            &format!("refs/heads/{target}"),
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok_and(|out| !out.lines().any(|line| line.starts_with('+')))
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--quiet",
            "--verify",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Writes `bytes` to `path` through a temporary file renamed over it.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.herdr-todo.tmp"));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {error}", path.display())
        })
}

impl WorkerSupervisor {
    /// Records a failure the coordinator decides on: the run waits on it.
    fn wait_on_failure(
        &self,
        run: &mut Run,
        kind: TodoEventKind,
        why: String,
    ) -> Result<(), String> {
        run.info.status = TodoRunStatus::Waiting;
        let mut event = new_event(kind);
        event.error = Some(why);
        self.record_run_event(run, &event)?;
        Ok(())
    }

    /// Runs the registered install in the repository's checkout with the
    /// caller's environment, then records the build its `build_id` command
    /// names. A build id that already is the target branch's (an install a crash did
    /// not record, or one made by hand) is not installed again. Without
    /// `[install]` the step is skipped.
    pub(super) fn step_install(&self, run: &mut Run) -> Result<(), String> {
        let repo = run.landing_dir();
        let Some(install) = run.finish.install.clone() else {
            run.info.step = TodoStep::Todo;
            self.run_step(
                run,
                json!({
                    "type": "run_install_skipped",
                    "reason": format!("{CHECKS_FILE} registers no [install]"),
                    "step": run.info.step,
                }),
            )?;
            return Ok(());
        };
        let Some(env) = lock(&RUN_ENV).get(&run.info.run_id).cloned() else {
            return self.wait_on_failure(
                run,
                TodoEventKind::InstallFailed,
                "this server has not got the caller's environment for the install (a restart, \
                 or a `herdr todo resume` that sent none); `herdr todo resume --action \
                 retry-install` from the coordinator's shell sends it and installs"
                    .into(),
            );
        };
        let branch = run.target_branch().to_owned();
        let target = run.target_commit()?;
        if !install.build_id.is_empty() {
            if let Ok(build) = installed_build(&repo, &install.build_id, &env) {
                if build_is(&build, &target) {
                    run.info.installed_build = Some(build.clone());
                    run.info.step = TodoStep::Todo;
                    self.run_step(
                        run,
                        json!({
                            "type": "run_installed",
                            "build": build,
                            "already": true,
                            "step": run.info.step,
                        }),
                    )?;
                    return Ok(());
                }
            }
        }
        let log = self
            .shared
            .dir
            .join(format!("run-{}-install.log", run.info.run_id));
        self.run_step(
            run,
            json!({
                "type": "run_install_intent",
                "command": install.command,
                "target": target,
                "target_branch": branch,
                "log": log.display().to_string(),
            }),
        )?;
        if let Err(why) = run_logged(&repo, &install.command, &env, &log) {
            return self.wait_on_failure(run, TodoEventKind::InstallFailed, why);
        }
        let build = if install.build_id.is_empty() {
            "installed".to_owned()
        } else {
            installed_build(&repo, &install.build_id, &env)
                .unwrap_or_else(|error| format!("unknown ({error})"))
        };
        run.info.installed_build = Some(build.clone());
        run.info.step = TodoStep::Todo;
        self.run_step(
            run,
            json!({
                "type": "run_installed",
                "build": build,
                "already": false,
                "step": run.info.step,
            }),
        )?;
        Ok(())
    }

    /// Appends the coordinator's note to the item in `TODO.md`, or removes
    /// the item and adds the closing decision to `DECISIONS.md` as a
    /// section, with herdr's own TODO editor ([`todo_editor`], the same in
    /// every repository), and commits that by path. The
    /// edit is made on copies of `HEAD`'s files first; a file with changes
    /// another session left uncommitted is not touched (a `todo_failed`
    /// event). A commit the target branch already has with the step's message is not
    /// made again. Without a note or decision the step is skipped.
    pub(super) fn step_todo(&self, run: &mut Run) -> Result<(), String> {
        let repo = run.landing_dir();
        let item = run.info.item.clone();
        let (message, files) = match (&run.finish.note, &run.finish.close) {
            (None, None) => {
                run.info.step = TodoStep::Push;
                self.run_step(
                    run,
                    json!({
                        "type": "run_todo_skipped",
                        "reason": "the approval gave no TODO note or closing decision",
                        "step": run.info.step,
                    }),
                )?;
                return Ok(());
            }
            (Some(_), _) => (format!("docs(todo): note on {item}"), vec![TODO_FILE]),
            (None, Some(_)) => (
                format!("docs(todo): close {item}"),
                vec![TODO_FILE, DECISIONS_FILE],
            ),
        };
        let since = run
            .info
            .picked
            .clone()
            .or_else(|| run.info.base.clone())
            .ok_or("the run has no base")?;
        let target = run.target_branch().to_owned();
        run.target_commit()?;
        if let Some(commit) = commit_with_subject(&repo, &since, &target, &message)? {
            return self.todo_committed(run, commit, &message, true);
        }
        let on = git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|branch| branch.trim().to_owned())
            .unwrap_or_default();
        if on != target {
            return self.wait_on_failure(
                run,
                TodoEventKind::TodoFailed,
                format!(
                    "the landing worktree {} is not on {target}; nothing was edited",
                    repo.display()
                ),
            );
        }
        let scratch = self
            .shared
            .dir
            .join(format!("run-{}-todo", run.info.run_id));
        let edited = self.todo_edit(run, &repo, &scratch, &files);
        let _ = std::fs::remove_dir_all(&scratch);
        let edited = match edited {
            Ok(edited) => edited,
            Err(why) => return self.wait_on_failure(run, TodoEventKind::TodoFailed, why),
        };
        for (file, bytes) in &edited {
            let status = git(&repo, &["status", "--porcelain", "--", file])?;
            // The edit a crash left uncommitted is this step's own.
            let ours = std::fs::read(repo.join(file)).is_ok_and(|now| &now == bytes);
            if !status.trim().is_empty() && !ours {
                return self.wait_on_failure(
                    run,
                    TodoEventKind::TodoFailed,
                    format!(
                        "{file} in {} has uncommitted changes:\n{}\ncommit or remove them, then \
                         retry-todo; nothing was edited",
                        repo.display(),
                        status.trim_end()
                    ),
                );
            }
        }
        let target_commit = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        self.run_step(
            run,
            json!({
                "type": "run_todo_intent",
                "message": message,
                "files": files,
                "target": target_commit,
            }),
        )?;
        for (file, bytes) in &edited {
            if let Err(why) = write_atomically(&repo.join(file), bytes) {
                return self.wait_on_failure(run, TodoEventKind::TodoFailed, why);
            }
        }
        let mut args = vec!["commit", "-q", "-m", message.as_str(), "--"];
        args.extend(files.iter().copied());
        if let Err(why) = git(&repo, &args) {
            return self.wait_on_failure(
                run,
                TodoEventKind::TodoFailed,
                format!("committing {}: {why}", files.join(" ")),
            );
        }
        let commit = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        self.todo_committed(run, commit, &message, false)
    }

    fn todo_committed(
        &self,
        run: &mut Run,
        commit: String,
        message: &str,
        already: bool,
    ) -> Result<(), String> {
        // A close records the item's last text and the items added since
        // the claim with this event, in the item's history.
        let (item_text, follow_ups) = if run.finish.note.is_none() && run.finish.close.is_some() {
            self.closed_item(run, &commit)
        } else {
            (None, Vec::new())
        };
        run.info.todo_commit = Some(commit.clone());
        run.info.step = TodoStep::Push;
        self.run_step(
            run,
            json!({
                "type": "run_todo_committed",
                "commit": commit,
                "message": message,
                "already": already,
                "step": run.info.step,
                "item_text": item_text,
                "follow_ups": follow_ups,
            }),
        )?;
        Ok(())
    }

    /// The closed item's text before the close commit `commit`, and the
    /// ids of the items in TODO.md after it that were not there at the
    /// run's claim (or, for a run claimed before claims were recorded, at
    /// its base).
    fn closed_item(&self, run: &Run, commit: &str) -> (Option<String>, Vec<String>) {
        let repo = Path::new(&run.info.repo);
        let todo_at = |rev: &str| git(repo, &["show", &format!("{rev}:{TODO_FILE}")]).ok();
        let item_text = todo_at(&format!("{commit}^"))
            .and_then(|text| todo_titles::item_text(&text, &run.info.item));
        let claimed: Option<BTreeSet<String>> = self
            .run_store()
            .ok()
            .and_then(|store| {
                store
                    .item_history(Some(&run.info.repo), Some(&run.info.item))
                    .ok()
            })
            .and_then(|records| {
                records.into_iter().find(|record| {
                    record.event.kind == HistoryEventKind::Claimed
                        && record.event.run_id.as_deref() == Some(run.info.run_id.as_str())
                })
            })
            .map(|record| record.ids_at_claim.into_iter().collect());
        let before = claimed.or_else(|| {
            run.info
                .base
                .as_deref()
                .and_then(todo_at)
                .map(|text| todo_titles::item_ids(&text))
        });
        let follow_ups = match (before, todo_at(commit)) {
            (Some(before), Some(after)) => todo_titles::item_ids(&after)
                .into_iter()
                .filter(|id| !before.contains(id))
                .collect(),
            _ => Vec::new(),
        };
        (item_text, follow_ups)
    }

    /// The step's files as herdr's `todo_edit.py` leaves them, edited on
    /// copies of `HEAD`'s files in `scratch`.
    fn todo_edit(
        &self,
        run: &Run,
        repo: &Path,
        scratch: &Path,
        files: &[&str],
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let _ = std::fs::remove_dir_all(scratch);
        // The editor gets a directory of its own: Python puts it first on
        // the import path.
        let editor = scratch.join("editor");
        std::fs::create_dir_all(&editor)
            .map_err(|error| format!("cannot create {}: {error}", editor.display()))?;
        let script = todo_editor::write_script(&editor)?;
        for file in files {
            let text = git(repo, &["show", &format!("HEAD:{file}")])
                .map_err(|error| format!("{} has no {file}: {error}", run.target_branch()))?;
            std::fs::write(scratch.join(file), text)
                .map_err(|error| format!("cannot write a copy of {file}: {error}"))?;
        }
        let edit = |file: &str, args: &[&str]| -> Result<(), String> {
            let output = command_with("python3", None)
                .arg(&script)
                .arg("--file")
                .arg(scratch.join(file))
                .args(args)
                .current_dir(repo)
                .stdin(Stdio::null())
                .output()
                .map_err(|error| format!("cannot run {SCRIPT_NAME}: {error}"))?;
            if output.status.success() {
                return Ok(());
            }
            Err(format!(
                "{SCRIPT_NAME} --file {file} {}: {}{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim(),
                String::from_utf8_lossy(&output.stdout).trim()
            ))
        };
        let item = run.info.item.as_str();
        let text_file = scratch.join("text.md");
        let text_path = text_file.display().to_string();
        let write_text = |text: &str| {
            std::fs::write(&text_file, text)
                .map_err(|error| format!("cannot write {}: {error}", text_file.display()))
        };
        match (&run.finish.note, &run.finish.close) {
            (Some(note), _) => {
                write_text(&indented(note))?;
                edit(
                    TODO_FILE,
                    &["append-to", item, "--text-file", text_path.as_str()],
                )?;
            }
            (None, Some(decision)) => {
                let titles = std::fs::read_to_string(scratch.join(TODO_FILE))
                    .map(|text| todo_titles::parse_titles(&text))
                    .unwrap_or_default();
                let fallback = titles.get(item).cloned().unwrap_or_else(|| item.to_owned());
                let (title, body) = decision_section(decision, &fallback);
                write_text(&body)?;
                edit(
                    DECISIONS_FILE,
                    &[
                        "add-section",
                        title.as_str(),
                        "--text-file",
                        text_path.as_str(),
                    ],
                )?;
                edit(TODO_FILE, &["remove", item])?;
            }
            (None, None) => {}
        }
        files
            .iter()
            .map(|file| {
                std::fs::read(scratch.join(file))
                    .map(|bytes| ((*file).to_owned(), bytes))
                    .map_err(|error| format!("cannot read the edited {file}: {error}"))
            })
            .collect()
    }

    /// Where the run pushes: what its preflight recorded from git's
    /// configuration ([`super::super::target_branch::push_of`]), or, for a
    /// run recorded before runs stored it, `origin`'s branch of the same
    /// name when the repository has `origin`.
    fn push_plan(run: &Run, repo: &Path) -> TodoRunPush {
        if let Some(push) = &run.info.push {
            return push.clone();
        }
        let legacy = "a run recorded before runs stored their push destination".to_owned();
        if git(repo, &["remote", "get-url", "origin"]).is_err() {
            return TodoRunPush {
                remote: None,
                remote_from: None,
                dst_ref: None,
                dst_from: None,
                skipped: Some("the repository has no origin remote".into()),
            };
        }
        TodoRunPush {
            remote: Some("origin".into()),
            remote_from: Some(legacy.clone()),
            dst_ref: Some(format!("refs/heads/{}", run.target_branch())),
            dst_from: Some(legacy),
            skipped: None,
        }
    }

    /// Pushes the target branch where the run's push destination names
    /// ([`Self::push_plan`]) with the caller's environment, with an
    /// explicit refspec and only as a fast-forward: it fetches that remote
    /// branch first, and when it is not an ancestor of the local one the
    /// push is refused (a `push_failed` event); it is never forced. A
    /// remote that lacks the branch is refused too: herdr never creates a
    /// branch on a remote (a fork's branch would leak upstream that way),
    /// the first push is the user's. When the remote already has the
    /// commit (a push a crash did not record) nothing is pushed. A run
    /// whose branch git names no single destination for skips the step,
    /// saying why.
    pub(super) fn step_push(&self, run: &mut Run) -> Result<(), String> {
        let repo = run.landing_dir();
        let plan = Self::push_plan(run, &repo);
        let (Some(remote_name), Some(dst_ref)) = (plan.remote.clone(), plan.dst_ref.clone()) else {
            run.info.step = TodoStep::Cleanup;
            self.run_step(
                run,
                json!({
                    "type": "run_push_skipped",
                    "reason": plan
                        .skipped
                        .clone()
                        .unwrap_or_else(|| "the run names no push destination".into()),
                    "step": run.info.step,
                }),
            )?;
            return Ok(());
        };
        let Some(env) = lock(&RUN_ENV).get(&run.info.run_id).cloned() else {
            return self.wait_on_failure(
                run,
                TodoEventKind::PushFailed,
                "this server has not got the caller's environment for the push (a restart, or \
                 a `herdr todo resume` that sent none); `herdr todo resume --action retry-push` \
                 from the coordinator's shell sends it and pushes"
                    .into(),
            );
        };
        let branch = run.target_branch().to_owned();
        let target_commit = run.target_commit()?;
        let theirs = format!("{remote_name}'s {dst_ref}");
        let remote = match git_with(
            &repo,
            &["ls-remote", "--", &remote_name, &dst_ref],
            Some(&env),
        ) {
            Ok(out) => out
                .lines()
                .find_map(|line| {
                    let (sha, name) = line.split_once('\t')?;
                    (name.trim() == dst_ref).then(|| sha.trim().to_owned())
                })
                .filter(|sha| !sha.is_empty()),
            Err(why) => {
                return self.wait_on_failure(
                    run,
                    TodoEventKind::PushFailed,
                    format!("reading {theirs}: {why}"),
                )
            }
        };
        let Some(remote) = remote else {
            return self.wait_on_failure(
                run,
                TodoEventKind::PushFailed,
                format!(
                    "{remote_name} has no {dst_ref}: herdr never creates a branch on a remote \
                     (that is how a fork's branch would leak upstream); the first push is yours \
                     (`git push {remote_name} {branch}:{dst_ref}`), then retry-push"
                ),
            );
        };
        if let Err(why) = git_with(
            &repo,
            &["fetch", "--quiet", "--", &remote_name, &dst_ref],
            Some(&env),
        ) {
            return self.wait_on_failure(
                run,
                TodoEventKind::PushFailed,
                format!("fetching {theirs}: {why}"),
            );
        }
        if git(
            &repo,
            &["merge-base", "--is-ancestor", &target_commit, &remote],
        )
        .is_ok()
        {
            return self.pushed(run, target_commit, Some(remote), true);
        }
        if git(
            &repo,
            &["merge-base", "--is-ancestor", &remote, &target_commit],
        )
        .is_err()
        {
            return self.wait_on_failure(
                run,
                TodoEventKind::PushFailed,
                format!(
                    "{theirs} {remote} is not an ancestor of {branch} {target_commit}: the push \
                     would not be a fast-forward, and it is never forced; bring {branch} up to \
                     date with {theirs}, then retry-push"
                ),
            );
        }
        if let Err(why) = super::check_private_details(&repo, &remote, &target_commit, Some(&env)) {
            return self.wait_on_failure(run, TodoEventKind::PushFailed, why);
        }
        self.run_step(
            run,
            json!({
                "type": "run_push_intent",
                "target": target_commit,
                "target_branch": branch,
                "remote": remote_name,
                "dst_ref": dst_ref,
                "origin": remote,
            }),
        )?;
        // Push exactly the inspected commit, not a ref another process could
        // advance between the privacy check and Git reading the push source;
        // without a `+`, the remote refuses anything but a fast-forward.
        let refspec = format!("{target_commit}:{dst_ref}");
        if let Err(why) = git_with(
            &repo,
            &["push", "--quiet", "--", &remote_name, &refspec],
            Some(&env),
        ) {
            return self.wait_on_failure(run, TodoEventKind::PushFailed, why);
        }
        #[cfg(test)]
        super::crashes_after(&run.info.repo, TodoStep::Push)?;
        self.pushed(run, target_commit, Some(remote), false)
    }

    fn pushed(
        &self,
        run: &mut Run,
        target_commit: String,
        before: Option<String>,
        already: bool,
    ) -> Result<(), String> {
        run.info.pushed = Some(target_commit.clone());
        run.info.step = TodoStep::Cleanup;
        self.run_step(
            run,
            json!({
                "type": "run_pushed",
                "target": target_commit,
                "origin_before": before,
                "already": already,
                "step": run.info.step,
            }),
        )?;
        Ok(())
    }

    /// Deletes the run's attempt branches whose changes the target branch has, and
    /// the ones earlier runs of the repository kept, except a branch a
    /// worktree has checked out (the folder slot's): that one is kept and
    /// recorded, for a later run's cleanup once the slot moved on. A branch
    /// with a change the target branch lacks (a rejected attempt) stays. Then the run
    /// is done.
    pub(super) fn step_cleanup(&self, run: &mut Run) -> Result<(), String> {
        let own: Vec<String> = (1..=run.info.attempt)
            .map(|attempt| super::branch_of(&run.info.item, &run.info.run_id, attempt))
            .collect();
        let swept = self.sweep_branches(run, &own)?;
        run.info.kept_branches = swept.kept.clone();
        self.run_step(
            run,
            json!({
                "type": "run_cleaned",
                "deleted": swept.deleted,
                "kept": swept.kept,
                "unmerged": swept.unmerged,
                "errors": swept.errors,
            }),
        )?;
        run.info.step = TodoStep::Done;
        run.info.status = TodoRunStatus::Done;
        let mut event = new_event(TodoEventKind::Done);
        event.commits = run
            .info
            .picked
            .iter()
            .chain(run.info.todo_commit.iter())
            .cloned()
            .collect();
        self.record_run_event(run, &event)?;
        let env = lock(&RUN_ENV).remove(&run.info.run_id);
        self.follow_up(&run.info.run_id, env);
        Ok(())
    }

    /// Settles what a done run's close or note named after it: starts the next
    /// item's run ([`Self::start_next`]) or settles the stop reason's notice
    /// ([`Self::stop_notice`]; the item's history has the reason with the
    /// `done` event). Each is recorded, so a server that starts settles only
    /// what is left ([`WorkerSupervisor::resume_runs`]).
    pub(super) fn follow_up(&self, run_id: &str, env: Option<HashMap<String, String>>) {
        let Ok(run) = self.load_run(run_id) else {
            return;
        };
        if !run.follow_up_pending() {
            return;
        }
        if let Some(next) = run.finish.next.clone() {
            if run.info.next_run_id.is_none() && run.info.next_refusal.is_none() {
                if let Err(error) = self.start_next(&run, &next, env) {
                    tracing::warn!(run_id, error, "cannot start the todo run's next item");
                }
            }
        }
        if run.finish.stop_reason.is_some() {
            self.stop_notice(run_id);
        }
    }

    /// Hands a done run's stop reason to the user as a notification, once,
    /// when no coordinator is there to go on with the chain: the run has no
    /// owner pane, or its owner pane or agent is gone (herdr's own events,
    /// never a timer). While the owner is there the stop stays recorded on
    /// the run and in the item's history only; the owner going away later
    /// sends it then ([`Self::owner_gone_stop_notices`]).
    pub(in crate::workers) fn stop_notice(&self, run_id: &str) {
        // The run's follow-up and its owner's event may settle it at once.
        let _settling = lock(&STOP_NOTICE);
        let Ok(run) = self.load_run(run_id) else {
            return;
        };
        let Some(reason) = run.finish.stop_reason.clone() else {
            return;
        };
        if run.info.status != TodoRunStatus::Done
            || run.finish.stop_notified
            || run.finish.stop_acknowledged
        {
            return;
        }
        if let Some(owner) = run.owner_pane.as_deref() {
            if self.gone_owner(owner).is_none() {
                return;
            }
        }
        let approved = if run.finish.close.is_some() {
            "closed"
        } else {
            "approved"
        };
        crate::workers::notify_user(crate::workers::UserNotice {
            pane_id: None,
            title: format!(
                "todo run {run_id}: {} {approved}; no next item started",
                run.info.item
            ),
            body: reason.clone(),
        });
        let noted = self.update_run(
            run_id,
            json!({"type": "run_stop_notified", "reason": reason}),
            |current| current.finish.stop_notified = true,
        );
        if let Err(error) = noted {
            tracing::warn!(run_id, error, "cannot record the stop reason's notice");
        }
    }

    /// Records that the user acknowledged done run `run_id`'s stop reason
    /// (`todo.ack_stop`): clients stop listing the run, and a notice held
    /// back while its owner is there is never sent. Acknowledging again
    /// changes nothing; a run without a stop reason is refused.
    pub(crate) fn todo_ack_stop(&self, run_id: &str) -> Result<TodoRunInfo, WorkerError> {
        let _settling = lock(&STOP_NOTICE);
        let run = self.load_run(run_id)?;
        if run.info.status != TodoRunStatus::Done || run.finish.stop_reason.is_none() {
            return Err(WorkerError::Invalid(format!(
                "todo run {run_id} is not done with a stop reason"
            )));
        }
        if !run.finish.stop_acknowledged {
            self.update_run(
                run_id,
                json!({"type": "run_stop_acknowledged"}),
                |current| current.finish.stop_acknowledged = true,
            )
            .map_err(|error| WorkerError::Io(std::io::Error::other(error)))?;
        }
        self.todo_status(run_id)
    }

    /// Sends the stop notices [`Self::stop_notice`] held back while owner
    /// pane `pane_id` was there, now that it is gone.
    pub(in crate::workers) fn owner_gone_stop_notices(&self, pane_id: &str) {
        let runs = match self.run_store().map(|store| store.runs(None)) {
            Ok(Ok(runs)) => runs,
            Ok(Err(error)) => {
                tracing::warn!(%error, "cannot read the todo runs' stop notices");
                return;
            }
            Err(_) => return,
        };
        for run in runs {
            if run.owner_pane.as_deref() == Some(pane_id) && run.stop_notice_pending() {
                self.stop_notice(&run.info.run_id);
            }
        }
    }

    /// Settles the follow-up of a run a previous server left done, in a
    /// thread of its own: the next run's usage gate reads the provider.
    pub(super) fn spawn_follow_up(&self, run_id: &str) {
        let supervisor = self.clone();
        let id = run_id.to_owned();
        if let Err(error) = crate::thread_spawn::spawn_named("herdr-todo-next", move || {
            // A driver of this process that holds the run settles it.
            let Some(_claim) = super::Driving::claim(&id) else {
                return;
            };
            supervisor.follow_up(&id, None);
        }) {
            tracing::warn!(%error, run_id, "cannot settle the todo run's follow-up");
        }
    }

    /// Starts `next` as `todo.run` would, with the done run's repository,
    /// owner and checks (unless `next` names its own) and the caller's
    /// environment when this server has it: the intent first, then the
    /// started run or the refusal (a `next_refused` event naming it). An
    /// intent without a result is a start a crash cut off: a run of the
    /// item created since the intent is that start, else it starts now.
    fn start_next(
        &self,
        run: &Run,
        next: &TodoNextRun,
        env: Option<HashMap<String, String>>,
    ) -> Result<(), String> {
        let run_id = run.info.run_id.as_str();
        if let Some(intent) = run.finish.next_intent_ms {
            let started = self
                .run_store()
                .map_err(|error| error.to_string())?
                .runs(Some(&run.info.repo))
                .map_err(|error| format!("the worker store failed: {error}"))?
                .into_iter()
                .find(|other| {
                    other.info.run_id != run.info.run_id
                        && other.info.item == next.item
                        && other.info.created_ms >= intent
                });
            if let Some(started) = started {
                return self.next_started(run_id, &next.item, started.info.run_id, true);
            }
        } else {
            let intent = now_ms();
            self.update_run(
                run_id,
                json!({"type": "run_next_intent", "item": next.item, "at_ms": intent}),
                |current| current.finish.next_intent_ms = Some(intent),
            )?;
        }
        #[cfg(test)]
        if super::crashes_before(&run.info.repo, TodoStep::Done) {
            return Ok(());
        }
        // The worktree this run landed in, as a `todo run` from there: its
        // TODO.md, checks, branch and push (the repository's checkout for a
        // run recorded before runs stored it).
        let params = TodoRunParams {
            cwd: run.landing_dir().display().to_string(),
            item: next.item.clone(),
            task: next.task.clone(),
            message: next.message.clone(),
            paths: next.paths.clone(),
            checks: if next.checks.is_empty() {
                run.info.checks.clone()
            } else {
                next.checks.clone()
            },
            owner_pane_id: run.owner_pane.clone(),
            owner_session_id: run.owner_session.clone(),
            workspace_id: run.workspace.clone(),
            env,
            ignore_usage: false,
            // A chained run names its own API additions nowhere: it is not
            // the run that landed the commit it starts from.
            new_api: false,
            // Set below from `next.agent` or `[workers] default_agent`.
            agent: None,
            // The next run's own model, or none for its agent's default.
            model: None,
            // The next run is reviewed and answered as the closed one was.
            auto_review: run.finish.auto_review,
            auto_answer: run.finish.auto_answer,
        };
        // The next run's model: its own `--next-model`, else the closed
        // run's, so a chain on another provider keeps that provider (and its
        // gate) instead of falling back to pi's default. A credit or Claude
        // run never inherits a pi model.
        let choice = Self::choice_of(None, next.agent);
        let model = match choice {
            WorkerAgentChoice::Pi => next.model.clone().or_else(|| run.finish.model.clone()),
            WorkerAgentChoice::Claude | WorkerAgentChoice::PiAnthropic => next.model.clone(),
        };
        let params = TodoRunParams { model, ..params };
        let wait_usage = run.finish.usage_dispatch;
        let mut started = self.start_run_as(params.clone(), false, None, wait_usage, choice);
        // A disk the slot's own build artifacts filled: its sweep frees
        // them, then preflight looks once more.
        let mut swept = String::new();
        if matches!(started, Err(WorkerError::Preflight(_)))
            && super::short_of_disk(&run.landing_dir())
        {
            swept = match super::sweep_slot(Path::new(&run.info.repo)) {
                Ok(()) => " (after the folder slot's sweep)".to_owned(),
                Err(error) => format!(" (after the folder slot's sweep: {error})"),
            };
            started = self.start_run_as(params, false, None, wait_usage, choice);
        }
        match started {
            Ok(started) => self.next_started(run_id, &next.item, started.run_id, false),
            Err(refused) => {
                let why = format!(
                    "the next item {} did not start ({}): {refused}{swept}",
                    next.item,
                    refused.code()
                );
                crate::workers::notify_user(crate::workers::UserNotice {
                    pane_id: None,
                    title: format!(
                        "todo run {run_id}: {} done; the next item {} did not start",
                        run.info.item, next.item
                    ),
                    body: why.clone(),
                });
                let mut event = new_event(TodoEventKind::NextRefused);
                event.error = Some(why.clone());
                let body = json!({
                    "type": "run_event",
                    "kind": event.kind,
                    "step": TodoStep::Done,
                    "status": TodoRunStatus::Done,
                    "attempt": run.info.attempt,
                    "event": event,
                });
                self.update_run(run_id, body, |current| {
                    current.info.next_refusal = Some(why);
                })
            }
        }
    }

    fn next_started(
        &self,
        run_id: &str,
        item: &str,
        next_run_id: String,
        already: bool,
    ) -> Result<(), String> {
        self.update_run(
            run_id,
            json!({
                "type": "run_next_started",
                "item": item,
                "run_id": next_run_id,
                "already": already,
            }),
            |current| current.info.next_run_id = Some(next_run_id.clone()),
        )
    }

    /// Appends `event` to an ended run and writes the run as the store has
    /// it now with `change` applied: another run's driver may write it
    /// meanwhile (the cleanup of its kept branches), so no copy read
    /// earlier is written back.
    pub(super) fn update_run(
        &self,
        run_id: &str,
        event: serde_json::Value,
        change: impl FnOnce(&mut Run),
    ) -> Result<(), String> {
        let store = self.run_store().map_err(|error| error.to_string())?;
        store
            .transaction(|tx| {
                let Some(mut current) = tx.run(run_id)? else {
                    return Ok(());
                };
                change(&mut current);
                tx.run_event(&mut current, &event, false, now_ms())?;
                Ok(())
            })
            .map_err(|error| format!("the worker store failed: {error}"))?;
        super::announce();
        Ok(())
    }
}

/// What [`WorkerSupervisor::sweep_branches`] did.
struct Swept {
    deleted: Vec<String>,
    kept: Vec<String>,
    unmerged: Vec<String>,
    errors: Vec<String>,
}

impl WorkerSupervisor {
    /// Before an attempt's worker starts: a folder slot still on a branch an
    /// earlier, finished run kept is detached (preflight found it clean and
    /// free), and the kept branches no worktree has checked out any more are
    /// deleted, so no run's branch outlives the next run's start. Recorded
    /// only when it changed something; a git failure is recorded, not fatal:
    /// the worker's start does not need it.
    pub(super) fn release_slot(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
        let kept: BTreeSet<String> = self
            .run_store()
            .map_err(|error| error.to_string())?
            .runs(Some(&run.info.repo))
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .filter(|other| other.info.run_id != run.info.run_id)
            .flat_map(|other| other.info.kept_branches)
            .collect();
        if kept.is_empty() {
            return Ok(());
        }
        let mut errors = Vec::new();
        let slot = match super::slot_dir(&repo) {
            Ok(slot) => Some(slot).filter(|slot| slot.is_dir()),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        let head = slot.as_deref().and_then(|slot| {
            git(slot, &["symbolic-ref", "--quiet", "--short", "HEAD"])
                .ok()
                .map(|head| head.trim().to_owned())
        });
        let detached = match (slot, head) {
            (Some(slot), Some(head)) if kept.contains(&head) => {
                match git(&slot, &["switch", "--quiet", "--detach"]) {
                    Ok(_) => Some(head),
                    Err(error) => {
                        errors.push(error);
                        None
                    }
                }
            }
            _ => None,
        };
        let swept = self.sweep_branches(run, &[])?;
        errors.extend(swept.errors);
        if detached.is_none() && swept.deleted.is_empty() && errors.is_empty() {
            return Ok(());
        }
        self.run_step(
            run,
            json!({
                "type": "run_slot_released",
                "detached": detached,
                "deleted": swept.deleted,
                "errors": errors,
            }),
        )?;
        Ok(())
    }

    /// Deletes `own` and the branches earlier runs of the repository kept
    /// when the run's target branch has their changes and no worktree has them checked
    /// out; keeps a checked-out one of `own`, and leaves an unmerged one.
    /// Earlier runs no longer keep what is deleted or gone.
    fn sweep_branches(&self, run: &Run, own: &[String]) -> Result<Swept, String> {
        let repo = PathBuf::from(&run.info.repo);
        let checked_out = checked_out(&repo)?;
        let earlier: Vec<Run> = self
            .run_store()
            .map_err(|error| error.to_string())?
            .runs(Some(&run.info.repo))
            .map_err(|error| format!("the worker store failed: {error}"))?
            .into_iter()
            .filter(|other| {
                other.info.run_id != run.info.run_id && !other.info.kept_branches.is_empty()
            })
            .collect();
        let mut deleted = Vec::new();
        let mut kept = Vec::new();
        let mut unmerged = Vec::new();
        let mut errors = Vec::new();
        let mut gone = BTreeSet::new();
        let candidates = own.iter().chain(
            earlier
                .iter()
                .flat_map(|other| other.info.kept_branches.iter()),
        );
        for branch in candidates {
            if gone.contains(branch) || deleted.contains(branch) {
                continue;
            }
            if !branch_exists(&repo, branch) {
                gone.insert(branch.clone());
                continue;
            }
            if !merged(&repo, run.target_branch(), branch) {
                unmerged.push(branch.clone());
            } else if checked_out.contains(branch) {
                if own.contains(branch) {
                    kept.push(branch.clone());
                }
            } else {
                // `-D`: a cherry-picked branch is merged by its changes, not
                // by its commits, which `-d` would ask for.
                match git(&repo, &["branch", "-D", branch]) {
                    Ok(_) => deleted.push(branch.clone()),
                    Err(error) => errors.push(error),
                }
            }
        }
        let stays = |branch: &String| !deleted.contains(branch) && !gone.contains(branch);
        for other in earlier {
            let kept: Vec<&String> = other
                .info
                .kept_branches
                .iter()
                .filter(|b| stays(b))
                .collect();
            if kept.len() == other.info.kept_branches.len() {
                continue;
            }
            let event = json!({
                "type": "run_branches_deleted",
                "by": run.info.run_id,
                "kept": kept,
            });
            // The ended run as the store has it now: its own follow-up
            // (the next item's start) may be writing it.
            self.update_run(&other.info.run_id, event, |current| {
                current.info.kept_branches.retain(stays);
            })?;
        }
        Ok(Swept {
            deleted,
            kept,
            unmerged,
            errors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_indented_without_blank_lines() {
        assert_eq!(
            indented("Done by w1.\n\n  kept\n\tTab\n"),
            "  Done by w1.\n  kept\n\tTab\n"
        );
    }

    #[test]
    fn a_decision_titles_its_section_by_its_heading_or_the_item() {
        assert_eq!(
            decision_section("## Chosen (2026-10-08)\n\n- why\n", "Item"),
            ("Chosen (2026-10-08)".into(), "- why\n".into())
        );
        assert_eq!(
            decision_section("- why\n- more", "Item"),
            ("Item".into(), "- why\n- more\n".into())
        );
    }

    #[test]
    fn only_a_clean_build_of_master_counts_as_installed() {
        let master = "0834ccd97e4760302d48537411f607e0fd75dc0d";
        assert!(build_is("0834ccd9 docs(todo): driver", master));
        assert!(build_is(master, master));
        assert!(!build_is("0834ccd9~1a2b3c4d build", master));
        assert!(!build_is("be9b9a7b feat: x", master));
        assert!(!build_is("0834", master));
        assert!(!build_is("", master));
    }
}
