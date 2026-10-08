//! `worker.verify`: herdr, not the worker's model, decides whether a
//! worker's commit is ready. The worker's `WORKER-DONE <sha>` line is a
//! summary; this checks what it claims, in the worker's directory and
//! outside its sandbox: the branch has exactly one new commit since the
//! base, its message is the expected subject alone (no body, no trailers),
//! its paths stay within the allowed globs, the worktree is clean, the
//! worker left no process behind, generated files regenerate to the
//! committed bytes, and a command (the tests) exits 0.
//!
//! A check that cannot run (git missing, the command not found, killed by
//! a signal) is `unavailable`, which is never a pass. Commands run as long
//! as they take: no timer of ours decides a verdict.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::api::schema::{
    WorkerCheckOutcome, WorkerGeneratedFile, WorkerVerdict, WorkerVerification, WorkerVerifyCheck,
};

/// How many of a command's last output lines the evidence keeps.
const TAIL_LINES: usize = 40;
/// And at most how many characters of them.
const TAIL_CHARS: usize = 4000;
/// How many entries of a list (paths, dirty files) the evidence names.
const LISTED: usize = 20;

/// What to check in `dir`.
pub(super) struct Request<'a> {
    pub(super) dir: &'a Path,
    pub(super) base: &'a str,
    pub(super) expected_message: &'a str,
    pub(super) allowed_paths: &'a [String],
    pub(super) command: Option<&'a str>,
    pub(super) generated: &'a [WorkerGeneratedFile],
    /// The worker's processes still running, found by the supervisor: its
    /// own, or a tool process working in `dir`. Empty when none remains.
    pub(super) processes: Vec<String>,
}

/// Runs every check and decides the verdict. When it ran anything that
/// may write (a generator, the command), it restores the worktree to the
/// head afterwards; it runs them only in a worktree it found clean, so
/// nothing of the worker's is lost.
pub(super) fn verify(request: &Request<'_>, now_ms: u64) -> WorkerVerification {
    let dir = request.dir;
    let mut checks = Vec::new();
    let head = git(dir, &["rev-parse", "--verify", "HEAD^{commit}"])
        .ok()
        .map(|out| out.trim().to_owned());
    let base_commit = git(
        dir,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", request.base),
        ],
    )
    .map(|out| out.trim().to_owned());
    let mut commits = Vec::new();

    // The commits from the base, and that the head descends from it.
    match (&base_commit, &head) {
        (Err(Git::Unavailable(error)), _) => checks.push(unavailable("commits", error)),
        (Err(Git::Failed(error)), _) => checks.push(failed(
            "commits",
            format!("the base {} is not a commit: {error}", request.base),
        )),
        (Ok(_), None) => checks.push(failed("commits", "the worktree has no HEAD commit".into())),
        (Ok(base), Some(head)) => match git(dir, &["merge-base", "--is-ancestor", base, head]) {
            Err(Git::Unavailable(error)) => checks.push(unavailable("commits", &error)),
            Err(Git::Failed(_)) => checks.push(failed(
                "commits",
                format!("HEAD {head} does not descend from the base {base}"),
            )),
            Ok(_) => match git(dir, &["rev-list", "--reverse", &format!("{base}..{head}")]) {
                Err(Git::Unavailable(error) | Git::Failed(error)) => {
                    checks.push(unavailable("commits", &error))
                }
                Ok(out) => {
                    commits = out.lines().map(str::to_owned).collect();
                    checks.push(match commits.len() {
                        1 => passed("commits", commits[0].clone()),
                        0 => failed("commits", format!("no commit since the base {base}")),
                        count => failed(
                            "commits",
                            format!(
                                "{count} commits since the base, expected one: {}",
                                commits.join(" ")
                            ),
                        ),
                    });
                }
            },
        },
    }

    // Each commit's message is the expected subject alone.
    if !commits.is_empty() {
        checks.push(check_messages(dir, &commits, request.expected_message));
    }

    // The changed paths stay within the allowed globs.
    if let (Ok(base), Some(head)) = (&base_commit, &head) {
        checks.push(check_paths(dir, base, head, request.allowed_paths));
    }

    // The worktree is clean; ignored files do not count.
    let clean = check_clean(dir);
    let tree_clean = clean.outcome == WorkerCheckOutcome::Passed;
    checks.push(clean);

    checks.push(if request.processes.is_empty() {
        passed("processes", String::new())
    } else {
        failed(
            "processes",
            format!("still running: {}", request.processes.join("; ")),
        )
    });

    // Generators and the command run only in a clean worktree, which is
    // then restored to the head: what they write is theirs, not the
    // worker's.
    let mut wrote = false;
    for generated in request.generated {
        let mut check = if tree_clean {
            wrote = true;
            let check = check_generated(dir, generated);
            restore(dir);
            check
        } else {
            skipped("generated", "the worktree is not clean")
        };
        check.path = Some(generated.path.clone());
        checks.push(check);
    }
    if let Some(command) = request.command {
        checks.push(if tree_clean {
            wrote = true;
            match run_shell(dir, command) {
                Ran::Exited(0, _) => passed("command", String::new()),
                Ran::Exited(code, tail) => {
                    failed("command", format!("exited with code {code}:\n{tail}"))
                }
                Ran::Unavailable(why) => unavailable("command", &why),
            }
        } else {
            skipped("command", "the worktree is not clean")
        });
    }
    if wrote {
        restore(dir);
    }

    let verdict = if checks
        .iter()
        .any(|check| check.outcome == WorkerCheckOutcome::Failed)
    {
        WorkerVerdict::Failed
    } else if checks
        .iter()
        .all(|check| check.outcome == WorkerCheckOutcome::Passed)
    {
        WorkerVerdict::Verified
    } else {
        WorkerVerdict::Unavailable
    };
    WorkerVerification {
        verdict,
        base: request.base.to_owned(),
        head,
        commits,
        checks,
        verified_ms: now_ms,
    }
}

fn check_messages(dir: &Path, commits: &[String], expected: &str) -> WorkerVerifyCheck {
    let expected = expected.trim_end_matches('\n');
    let mut problems = Vec::new();
    for sha in commits {
        let object = match git(dir, &["cat-file", "commit", sha]) {
            Ok(object) => object,
            Err(Git::Unavailable(error) | Git::Failed(error)) => {
                return unavailable("message", &error)
            }
        };
        // The message follows the headers' first blank line.
        let message = object
            .split_once("\n\n")
            .map_or("", |(_, message)| message)
            .trim_end_matches('\n');
        if message == expected {
            continue;
        }
        let short: String = sha.chars().take(12).collect();
        let (subject, rest) = message.split_once('\n').unwrap_or((message, ""));
        if subject != expected {
            problems.push(format!("{short}: subject {subject:?} is not {expected:?}"));
        }
        let extra: Vec<&str> = rest
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if !extra.is_empty() {
            let trailers: Vec<&str> = extra
                .iter()
                .copied()
                .filter(|line| is_trailer(line))
                .collect();
            problems.push(if trailers.is_empty() {
                format!("{short}: has a body: {:?}", extra[0])
            } else {
                format!("{short}: has trailers: {}", trailers.join("; "))
            });
        }
    }
    if problems.is_empty() {
        passed("message", String::new())
    } else {
        failed("message", problems.join("\n"))
    }
}

/// A `Token: value` line, as git's trailers are (`Co-Authored-By: ...`).
fn is_trailer(line: &str) -> bool {
    line.split_once(": ").is_some_and(|(token, _)| {
        !token.is_empty() && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

fn check_paths(dir: &Path, base: &str, head: &str, allowed: &[String]) -> WorkerVerifyCheck {
    let diff = |pathspecs: &[String]| -> Result<Vec<String>, Git> {
        let mut args = vec![
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            base,
            head,
            "--",
        ];
        args.extend(pathspecs.iter().map(String::as_str));
        Ok(git(dir, &args)?
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    };
    let changed = match diff(&[]) {
        Ok(changed) => changed,
        Err(Git::Unavailable(error) | Git::Failed(error)) => return unavailable("paths", &error),
    };
    // Git matches the globs itself, as `:(glob)` pathspecs.
    let inside = if allowed.is_empty() {
        Vec::new()
    } else {
        let specs: Vec<String> = allowed
            .iter()
            .map(|glob| format!(":(glob){glob}"))
            .collect();
        match diff(&specs) {
            Ok(inside) => inside,
            Err(Git::Unavailable(error) | Git::Failed(error)) => {
                return unavailable("paths", &error)
            }
        }
    };
    let outside: Vec<&String> = changed
        .iter()
        .filter(|path| !inside.contains(path))
        .collect();
    if outside.is_empty() {
        passed("paths", format!("{} changed", changed.len()))
    } else {
        failed(
            "paths",
            format!(
                "outside {}: {}",
                allowed.join(" "),
                listed(outside.iter().map(|path| path.as_str()))
            ),
        )
    }
}

fn check_clean(dir: &Path) -> WorkerVerifyCheck {
    match git(
        dir,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    ) {
        Err(Git::Unavailable(error) | Git::Failed(error)) => unavailable("clean_tree", &error),
        Ok(out) => {
            let entries: Vec<&str> = out.split('\0').filter(|entry| !entry.is_empty()).collect();
            if entries.is_empty() {
                passed("clean_tree", String::new())
            } else {
                failed("clean_tree", listed(entries.into_iter()))
            }
        }
    }
}

fn check_generated(dir: &Path, generated: &WorkerGeneratedFile) -> WorkerVerifyCheck {
    let path = generated.path.as_str();
    match git(dir, &["ls-files", "--error-unmatch", "--", path]) {
        Ok(_) => {}
        Err(Git::Unavailable(error)) => return unavailable("generated", &error),
        Err(Git::Failed(_)) => return failed("generated", format!("{path} is not committed")),
    }
    match run_shell(dir, &generated.command) {
        Ran::Exited(0, _) => {}
        Ran::Exited(code, tail) => {
            return failed(
                "generated",
                format!("the regenerate command exited with code {code}:\n{tail}"),
            )
        }
        Ran::Unavailable(why) => return unavailable("generated", &why),
    }
    match git(dir, &["diff", "--stat", "HEAD", "--", path]) {
        Err(Git::Unavailable(error) | Git::Failed(error)) => unavailable("generated", &error),
        Ok(stat) if stat.trim().is_empty() => passed("generated", String::new()),
        Ok(stat) => failed(
            "generated",
            format!(
                "regenerating it changes the committed file:\n{}",
                stat.trim_end()
            ),
        ),
    }
}

/// Puts the worktree back at its head: tracked files reset, untracked ones
/// removed, ignored ones (build output) kept. Only after it was found clean.
fn restore(dir: &Path) {
    for args in [&["reset", "-q", "--hard", "HEAD"][..], &["clean", "-fdq"]] {
        if let Err(Git::Unavailable(error) | Git::Failed(error)) = git(dir, args) {
            tracing::warn!(%error, dir = %dir.display(), "restoring a verified worktree failed");
        }
    }
}

fn listed<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.collect();
    let mut text = items
        .iter()
        .take(LISTED)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > LISTED {
        text.push_str(&format!(" and {} more", items.len() - LISTED));
    }
    text
}

fn check(check: &str, outcome: WorkerCheckOutcome, detail: String) -> WorkerVerifyCheck {
    WorkerVerifyCheck {
        check: check.to_owned(),
        outcome,
        path: None,
        detail,
    }
}

fn passed(name: &str, detail: String) -> WorkerVerifyCheck {
    check(name, WorkerCheckOutcome::Passed, detail)
}

fn failed(name: &str, detail: String) -> WorkerVerifyCheck {
    check(name, WorkerCheckOutcome::Failed, detail)
}

fn unavailable(name: &str, why: &str) -> WorkerVerifyCheck {
    check(name, WorkerCheckOutcome::Unavailable, why.to_owned())
}

fn skipped(name: &str, why: &str) -> WorkerVerifyCheck {
    check(name, WorkerCheckOutcome::Skipped, why.to_owned())
}

enum Git {
    /// Git could not run.
    Unavailable(String),
    /// It ran and refused; the message is its stderr.
    Failed(String),
}

fn git(dir: &Path, args: &[&str]) -> Result<String, Git> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| Git::Unavailable(format!("cannot run git: {error}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(Git::Failed(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

enum Ran {
    /// Its exit code and the tail of its output (stdout and stderr).
    Exited(i32, String),
    /// It could not run, or did not end by itself (a signal).
    Unavailable(String),
}

/// Runs `command` with `sh -c` in `dir`, stderr merged into stdout. Exit
/// 126 and 127 are the shell's "cannot execute" and "not found".
fn run_shell(dir: &Path, command: &str) -> Ran {
    let output = match Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => return Ran::Unavailable(format!("cannot run sh: {error}")),
    };
    let tail = tail(&String::from_utf8_lossy(&output.stdout));
    match output.status.code() {
        Some(code @ (126 | 127)) => {
            Ran::Unavailable(format!("{command:?} could not run (exit {code}):\n{tail}"))
        }
        Some(code) => Ran::Exited(code, tail),
        None => Ran::Unavailable(format!("{command:?} was ended by a signal:\n{tail}")),
    }
}

fn tail(output: &str) -> String {
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let text = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    let chars = text.chars().count();
    if chars > TAIL_CHARS {
        text.chars().skip(chars - TAIL_CHARS).collect()
    } else {
        text
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    const SUBJECT: &str = "feat: one change";

    /// A repository with a base commit and a generator for `gen.txt`
    /// (`./gen.sh`, which writes it from `src.txt`), and returns it with
    /// the base sha.
    fn repo(name: &str) -> (PathBuf, String) {
        let dir = std::env::temp_dir().join(format!(
            "herdr-verify-{name}-{}-{}",
            std::process::id(),
            super::super::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git_in(&dir, &["init", "-q", "-b", "master"]);
        std::fs::write(dir.join(".gitignore"), "/target\n").unwrap();
        std::fs::write(dir.join("src.txt"), "one\n").unwrap();
        std::fs::write(
            dir.join("gen.sh"),
            "#!/bin/sh\ntr a-z A-Z < src.txt > gen.txt\n",
        )
        .unwrap();
        std::fs::write(dir.join("gen.txt"), "ONE\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "a\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "init"]);
        let base = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        (dir, base)
    }

    /// The worker's change: `src/a.rs` edited and committed with `message`.
    fn commit(dir: &Path, message: &str) {
        std::fs::write(dir.join("src/a.rs"), "b\n").unwrap();
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-q", "-m", message]);
    }

    fn run(dir: &Path, base: &str, command: Option<&str>, generated: bool) -> WorkerVerification {
        let generated = if generated {
            vec![WorkerGeneratedFile {
                path: "gen.txt".into(),
                command: "sh gen.sh".into(),
            }]
        } else {
            Vec::new()
        };
        verify(
            &Request {
                dir,
                base,
                expected_message: SUBJECT,
                allowed_paths: &["src/**".into(), "gen.txt".into()],
                command,
                generated: &generated,
                processes: Vec::new(),
            },
            7,
        )
    }

    fn outcome(verification: &WorkerVerification, name: &str) -> WorkerCheckOutcome {
        verification
            .checks
            .iter()
            .find(|check| check.check == name)
            .unwrap_or_else(|| panic!("no {name} check: {verification:#?}"))
            .outcome
    }

    #[test]
    fn a_commit_that_passes_every_check_is_verified() {
        let (dir, base) = repo("ok");
        commit(&dir, SUBJECT);
        let verification = run(&dir, &base, Some("test -f src/a.rs"), true);
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Verified,
            "{verification:#?}"
        );
        assert_eq!(verification.commits.len(), 1);
        assert_eq!(
            verification.head.as_deref(),
            Some(verification.commits[0].as_str())
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_wrong_subject_fails() {
        let (dir, base) = repo("subject");
        commit(&dir, "feat: another change");
        let verification = run(&dir, &base, None, false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert_eq!(
            outcome(&verification, "message"),
            WorkerCheckOutcome::Failed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_co_author_trailer_fails() {
        let (dir, base) = repo("trailer");
        commit(
            &dir,
            &format!("{SUBJECT}\n\nCo-Authored-By: Someone <someone@example.com>"),
        );
        let verification = run(&dir, &base, None, false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let message = verification
            .checks
            .iter()
            .find(|check| check.check == "message")
            .unwrap();
        assert_eq!(message.outcome, WorkerCheckOutcome::Failed);
        assert!(message.detail.contains("Co-Authored-By"), "{message:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_path_outside_the_globs_fails() {
        let (dir, base) = repo("paths");
        std::fs::write(dir.join("README.md"), "x\n").unwrap();
        commit(&dir, SUBJECT);
        let verification = run(&dir, &base, None, false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let paths = verification
            .checks
            .iter()
            .find(|check| check.check == "paths")
            .unwrap();
        assert_eq!(paths.outcome, WorkerCheckOutcome::Failed);
        assert!(paths.detail.contains("README.md"), "{paths:?}");
        assert!(!paths.detail.contains("src/a.rs"), "{paths:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_commits_fail_and_each_is_reported() {
        let (dir, base) = repo("two");
        commit(&dir, SUBJECT);
        std::fs::write(dir.join("src/b.rs"), "b\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", SUBJECT]);
        let verification = run(&dir, &base, None, false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert_eq!(verification.commits.len(), 2);
        assert_eq!(
            outcome(&verification, "commits"),
            WorkerCheckOutcome::Failed
        );
        assert_eq!(
            outcome(&verification, "message"),
            WorkerCheckOutcome::Passed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_dirty_tree_fails_and_runs_nothing_in_it() {
        let (dir, base) = repo("dirty");
        commit(&dir, SUBJECT);
        std::fs::write(dir.join("src/a.rs"), "uncommitted\n").unwrap();
        std::fs::write(dir.join("stray.txt"), "x\n").unwrap();
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/ignored"), "x\n").unwrap();
        let verification = run(&dir, &base, Some("touch ran"), true);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let clean = verification
            .checks
            .iter()
            .find(|check| check.check == "clean_tree")
            .unwrap();
        assert_eq!(clean.outcome, WorkerCheckOutcome::Failed);
        assert!(clean.detail.contains("stray.txt"), "{clean:?}");
        assert!(
            !clean.detail.contains("target"),
            "ignored files count: {clean:?}"
        );
        assert_eq!(
            outcome(&verification, "command"),
            WorkerCheckOutcome::Skipped
        );
        assert_eq!(
            outcome(&verification, "generated"),
            WorkerCheckOutcome::Skipped
        );
        // The worker's uncommitted edit is left alone.
        assert!(!dir.join("ran").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("src/a.rs")).unwrap(),
            "uncommitted\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_command_fails_with_its_output() {
        let (dir, base) = repo("command");
        commit(&dir, SUBJECT);
        let verification = run(&dir, &base, Some("echo the test broke >&2; exit 3"), false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let command = verification
            .checks
            .iter()
            .find(|check| check.check == "command")
            .unwrap();
        assert_eq!(command.outcome, WorkerCheckOutcome::Failed);
        assert!(command.detail.contains("code 3"), "{command:?}");
        assert!(command.detail.contains("the test broke"), "{command:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_command_is_unavailable_never_verified() {
        let (dir, base) = repo("missing");
        commit(&dir, SUBJECT);
        let verification = run(&dir, &base, Some("herdr-no-such-binary --test"), false);
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Unavailable,
            "{verification:#?}"
        );
        assert_eq!(
            outcome(&verification, "command"),
            WorkerCheckOutcome::Unavailable
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_generated_file_edited_by_hand_fails_and_the_tree_is_restored() {
        let (dir, base) = repo("generated");
        std::fs::write(dir.join("gen.txt"), "ONE, edited by hand\n").unwrap();
        commit(&dir, SUBJECT);
        let verification = run(&dir, &base, None, true);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let generated = verification
            .checks
            .iter()
            .find(|check| check.check == "generated")
            .unwrap();
        assert_eq!(generated.outcome, WorkerCheckOutcome::Failed);
        assert_eq!(generated.path.as_deref(), Some("gen.txt"));
        // Restored to the commit's bytes, the tree clean again.
        assert_eq!(
            std::fs::read_to_string(dir.join("gen.txt")).unwrap(),
            "ONE, edited by hand\n"
        );
        assert!(git_in(&dir, &["status", "--porcelain"]).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_process_left_behind_fails() {
        let (dir, base) = repo("process");
        commit(&dir, SUBJECT);
        let verification = verify(
            &Request {
                dir: &dir,
                base: &base,
                expected_message: SUBJECT,
                allowed_paths: &["src/**".into()],
                command: None,
                generated: &[],
                processes: vec!["the worker's process 42".into()],
            },
            7,
        );
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert_eq!(
            outcome(&verification, "processes"),
            WorkerCheckOutcome::Failed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_base_the_head_does_not_descend_from_fails() {
        let (dir, _) = repo("ancestry");
        git_in(&dir, &["checkout", "-q", "-b", "other"]);
        std::fs::write(dir.join("src/c.rs"), "c\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "elsewhere"]);
        let other = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        git_in(&dir, &["checkout", "-q", "master"]);
        commit(&dir, SUBJECT);
        let verification = run(&dir, &other, None, false);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert_eq!(
            outcome(&verification, "commits"),
            WorkerCheckOutcome::Failed
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
