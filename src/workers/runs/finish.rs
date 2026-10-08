//! The steps of a `herdr todo run` after its cherry-pick: the install, the
//! TODO update, the push and the cleanup. Each records its intent before its
//! side effect and its result after it, and asks the repository first
//! whether a driver a crash cut off already did it: the installed build is
//! `master`'s, `master` has the TODO commit, `origin` has `master`. A failed
//! install, TODO edit or push is an event the run waits on; nothing is
//! forced, rebased or retried on its own.

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::json;

use super::{
    command_with, git, git_with, new_event, tail, todo_titles, Run, CHECKS_FILE, DECISIONS_FILE,
    RUN_ENV, TODO_EDIT, TODO_FILE,
};
use crate::api::schema::{TodoEventKind, TodoRunStatus, TodoStep};
use crate::workers::{lock, WorkerSupervisor};

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

/// Whether the build id a `build_id` command printed is `master`'s commit:
/// its first word is that sha or a prefix of it (a dirty build, `<sha>~<tree>`,
/// is not).
fn build_is(build: &str, master: &str) -> bool {
    let word = build.split_whitespace().next().unwrap_or_default();
    word.len() >= 7 && !word.contains('~') && master.starts_with(word)
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

/// The newest commit in `since..master` whose subject is `subject`.
fn commit_with_subject(repo: &Path, since: &str, subject: &str) -> Result<Option<String>, String> {
    Ok(git(
        repo,
        &["log", "--format=%H %s", &format!("{since}..master")],
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

/// Whether every commit of `branch` has its change on `master` (`git
/// cherry` marks none `+`): a cherry-picked branch is merged though its
/// commits are not ancestors of `master`.
fn merged(repo: &Path, branch: &str) -> bool {
    git(repo, &["cherry", "master", &format!("refs/heads/{branch}")])
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
    /// names. A build id that already is `master`'s (an install a crash did
    /// not record, or one made by hand) is not installed again. Without
    /// `[install]` the step is skipped.
    pub(super) fn step_install(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
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
        let master = git(&repo, &["rev-parse", "--verify", "master^{commit}"])?
            .trim()
            .to_owned();
        if !install.build_id.is_empty() {
            if let Ok(build) = installed_build(&repo, &install.build_id, &env) {
                if build_is(&build, &master) {
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
                "master": master,
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
    /// section, with `scripts/todo_edit.py`, and commits that by path. The
    /// edit is made on copies of `HEAD`'s files first; a file with changes
    /// another session left uncommitted is not touched (a `todo_failed`
    /// event). A commit `master` already has with the step's message is not
    /// made again. Without a note or decision the step is skipped.
    pub(super) fn step_todo(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
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
        if let Some(commit) = commit_with_subject(&repo, &since, &message)? {
            return self.todo_committed(run, commit, &message, true);
        }
        let on = git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|branch| branch.trim().to_owned())
            .unwrap_or_default();
        if on != "master" {
            return self.wait_on_failure(
                run,
                TodoEventKind::TodoFailed,
                format!(
                    "the shared checkout {} is not on master; nothing was edited",
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
        let master = git(&repo, &["rev-parse", "HEAD"])?.trim().to_owned();
        self.run_step(
            run,
            json!({
                "type": "run_todo_intent",
                "message": message,
                "files": files,
                "master": master,
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
            }),
        )?;
        Ok(())
    }

    /// The step's files as `todo_edit.py` leaves them, edited on copies of
    /// `HEAD`'s files in `scratch`.
    fn todo_edit(
        &self,
        run: &Run,
        repo: &Path,
        scratch: &Path,
        files: &[&str],
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let script = repo.join(TODO_EDIT);
        if !script.is_file() {
            return Err(format!("{} has no {TODO_EDIT}", repo.display()));
        }
        let _ = std::fs::remove_dir_all(scratch);
        std::fs::create_dir_all(scratch)
            .map_err(|error| format!("cannot create {}: {error}", scratch.display()))?;
        for file in files {
            let text = git(repo, &["show", &format!("HEAD:{file}")])
                .map_err(|error| format!("master has no {file}: {error}"))?;
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
                .map_err(|error| format!("cannot run {TODO_EDIT}: {error}"))?;
            if output.status.success() {
                return Ok(());
            }
            Err(format!(
                "{TODO_EDIT} --file {file} {}: {}{}",
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

    /// Pushes `master` to `origin` with the caller's environment, only as
    /// a fast-forward: when `origin`'s `master` is not an ancestor of the
    /// local one the push is refused (a `push_failed` event), and it is
    /// never forced. When `origin` already has `master` (a push a crash did
    /// not record) nothing is pushed. A repository without `origin` skips
    /// the step.
    pub(super) fn step_push(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
        if git(&repo, &["remote", "get-url", "origin"]).is_err() {
            run.info.step = TodoStep::Cleanup;
            self.run_step(
                run,
                json!({
                    "type": "run_push_skipped",
                    "reason": "the repository has no origin remote",
                    "step": run.info.step,
                }),
            )?;
            return Ok(());
        }
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
        let master = git(&repo, &["rev-parse", "--verify", "master^{commit}"])?
            .trim()
            .to_owned();
        let remote = match git_with(
            &repo,
            &["ls-remote", "origin", "refs/heads/master"],
            Some(&env),
        ) {
            Ok(out) => out
                .split_whitespace()
                .next()
                .map(str::to_owned)
                .filter(|sha| !sha.is_empty()),
            Err(why) => {
                return self.wait_on_failure(
                    run,
                    TodoEventKind::PushFailed,
                    format!("reading origin's master: {why}"),
                )
            }
        };
        if let Some(remote) = &remote {
            if let Err(why) = git_with(
                &repo,
                &["fetch", "--quiet", "origin", "refs/heads/master"],
                Some(&env),
            ) {
                return self.wait_on_failure(
                    run,
                    TodoEventKind::PushFailed,
                    format!("fetching origin's master: {why}"),
                );
            }
            if git(&repo, &["merge-base", "--is-ancestor", &master, remote]).is_ok() {
                return self.pushed(run, master, Some(remote.clone()), true);
            }
            if git(&repo, &["merge-base", "--is-ancestor", remote, &master]).is_err() {
                return self.wait_on_failure(
                    run,
                    TodoEventKind::PushFailed,
                    format!(
                        "origin's master {remote} is not an ancestor of master {master}: the push \
                         would not be a fast-forward, and it is never forced; bring master up to \
                         date with origin, then retry-push"
                    ),
                );
            }
        }
        self.run_step(
            run,
            json!({"type": "run_push_intent", "master": master, "origin": remote}),
        )?;
        if let Err(why) = git_with(
            &repo,
            &[
                "push",
                "--quiet",
                "origin",
                "refs/heads/master:refs/heads/master",
            ],
            Some(&env),
        ) {
            return self.wait_on_failure(run, TodoEventKind::PushFailed, why);
        }
        #[cfg(test)]
        super::crashes_after(&run.info.repo, TodoStep::Push)?;
        self.pushed(run, master, remote, false)
    }

    fn pushed(
        &self,
        run: &mut Run,
        master: String,
        before: Option<String>,
        already: bool,
    ) -> Result<(), String> {
        run.info.pushed = Some(master.clone());
        run.info.step = TodoStep::Cleanup;
        self.run_step(
            run,
            json!({
                "type": "run_pushed",
                "master": master,
                "origin_before": before,
                "already": already,
                "step": run.info.step,
            }),
        )?;
        Ok(())
    }

    /// Deletes the run's attempt branches whose changes `master` has, and
    /// the ones earlier runs of the repository kept, except a branch a
    /// worktree has checked out (the folder slot's): that one is kept and
    /// recorded, for a later run's cleanup once the slot moved on. A branch
    /// with a change `master` lacks (a rejected attempt) stays. Then the run
    /// is done.
    pub(super) fn step_cleanup(&self, run: &mut Run) -> Result<(), String> {
        let repo = PathBuf::from(&run.info.repo);
        let checked_out = checked_out(&repo)?;
        let own: Vec<String> = (1..=run.info.attempt)
            .map(|attempt| super::branch_of(&run.info.item, attempt))
            .collect();
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
            if !merged(&repo, branch) {
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
        for mut other in earlier {
            let before = other.info.kept_branches.len();
            other
                .info
                .kept_branches
                .retain(|branch| !deleted.contains(branch) && !gone.contains(branch));
            if other.info.kept_branches.len() == before {
                continue;
            }
            let event = json!({
                "type": "run_branches_deleted",
                "by": run.info.run_id,
                "kept": other.info.kept_branches,
            });
            self.run_write(&mut other, event, false)?;
        }
        run.info.kept_branches = kept.clone();
        self.run_step(
            run,
            json!({
                "type": "run_cleaned",
                "deleted": deleted,
                "kept": kept,
                "unmerged": unmerged,
                "errors": errors,
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
        lock(&RUN_ENV).remove(&run.info.run_id);
        Ok(())
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
