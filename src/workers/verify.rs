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
//! a signal) is `unavailable`, which is never a pass. So is a command that
//! failed on its build environment before the project's own work, as its
//! output shows ([`environment_failure`]): a missing tool or a wrong
//! toolchain version says nothing about the commit. Commands run as long
//! as they take: no timer of ours decides a verdict.
//!
//! The command and the generators run with the caller's environment when
//! the request carries it: the herdr server may have been started without
//! the user's shell profile (`ZIG` from `~/.zshenv`), and its environment
//! would build with another toolchain than the user's.
//!
//! A repository that configures `[counterfactual]` in `.herdr/checks.toml`
//! is checked one more way, after its checks ran on the commit: the tests the
//! commit adds or changes are put on the commit's base and each one must fail
//! there ([`counterfactual`]), because a test that passes without the commit
//! proves nothing. It is one check beside the others, and everything it
//! cannot do soundly is `unavailable`, never a pass.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use super::runs::{declared_guarded_paths, CHECKS_FILE};
use crate::api::schema::{
    WorkerCheckOutcome, WorkerGeneratedFile, WorkerVerdict, WorkerVerification, WorkerVerifyCheck,
};

mod counterfactual;

/// How many of a command's last output lines the evidence keeps.
const TAIL_LINES: usize = 40;
/// And at most how many characters of them.
const TAIL_CHARS: usize = 4000;
/// How many entries of a list (paths, dirty files) the evidence names.
const LISTED: usize = 20;

/// The paths that decide whether a commit is acceptable: the files the
/// verify's checks and the automatic review read. A commit that changes one
/// is reported ([`WorkerVerification::guarded_paths`]) and is never approved
/// automatically: a coordinator reads them and approves it explicitly
/// (`herdr todo resume --action approve --accept-guarded-paths`). Editing
/// them is legitimate, so this changes no verdict. Kept in one place here,
/// with the reason for each; a repository that guards more declares them in
/// `.herdr/checks.toml`'s `[review] guarded_paths` (`runs::ChecksFile`), and
/// the guarded set is this list plus those, never instead of them: no
/// repository config can drop herdr's own guard. The configured list is read
/// at the commit's base, never at the commit: a commit cannot add or drop its
/// own guard.
///
/// The match is lexical, over the commit's changed pathnames:
///
/// - a rename is covered: the diff is read with `--no-renames`, so moving a
///   guarded file away shows its source as a deletion, which matches;
/// - a symlink is not: a link committed at a guarded pathname is guarded,
///   but the file it points to is another pathname and is not read; and
/// - a differently-cased pathname is not on a case-insensitive filesystem,
///   where git may store one spelling and the glob match another.
///
/// A repository that needs the last two guarded is out of this guard's
/// scope.
pub(super) const GUARDED_PATHS: [&str; 3] = [
    // The registered checks the verify runs (and the install it makes): a
    // commit could weaken the checks it is verified with.
    CHECKS_FILE,
    // The bug classes the automatic review is told to look for.
    ".herdr/review-checklist.md",
    // The cases `todo review-eval` replays the review against.
    ".herdr/review-golden/**",
];

/// What to check in `dir`.
pub(super) struct Request<'a> {
    pub(super) dir: &'a Path,
    pub(super) base: &'a str,
    pub(super) expected_message: &'a str,
    pub(super) allowed_paths: &'a [String],
    /// The command checks, run in order, each with its registered name
    /// when it has one.
    pub(super) commands: Vec<(Option<&'a str>, CheckCommand<'a>)>,
    pub(super) generated: &'a [WorkerGeneratedFile],
    /// The caller's environment, which the command and the generators run
    /// with instead of the server's; `None` keeps the server's.
    pub(super) env: Option<&'a HashMap<String, String>>,
    /// The worker's processes still running, found by the supervisor: its
    /// own, or a tool process working in `dir`. Empty when none remains.
    pub(super) processes: Vec<String>,
    /// The commit adds an API its base cannot build against
    /// (`herdr todo run --new-api`): the counterfactual check then accepts a
    /// base that does not build with the commit's tests instead of failing.
    /// It relaxes nothing else.
    pub(super) new_api: bool,
}

/// The command check: a shell command (`worker.verify --cmd`), or a
/// program and its arguments run without a shell (a registered check of
/// `todo.run`), so nothing in them is expanded or split; or a check that
/// cannot run, and why (a run whose caller's environment the server does
/// not have).
#[derive(Debug, Clone, Copy)]
pub(super) enum CheckCommand<'a> {
    Shell(&'a str),
    Argv(&'a [String]),
    Unavailable(&'a str),
}

/// Runs every check and decides the verdict. When it ran anything that
/// may write (a generator, the command), it restores the worktree to the
/// head afterwards; it runs them only in a worktree it found clean, so
/// nothing of the worker's is lost.
pub(super) fn verify(request: &Request<'_>, now_ms: u64) -> WorkerVerification {
    let dir = request.dir;
    let mut checks = Vec::new();
    // A counterfactual check killed while it wrote the worktree left it at the
    // base with the commit's tests overlaid: put it back before anything
    // below reads its `HEAD` or its tree.
    let recovered = counterfactual::recover_leftover(dir);
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

    // The changed paths that decide whether the commit is acceptable: they
    // are reported beside the other checks, not judged (the verdict stays
    // what the checks made it). A guard that cannot be read is `unavailable`,
    // never a pass: nothing approves a commit whose guard is unknown.
    let (guarded, guarded_check) = match (&base_commit, &head) {
        (Ok(base), Some(head)) => match guarded_paths(dir, base, head) {
            Ok(paths) => (
                paths.clone(),
                passed("guarded", listed(paths.iter().map(String::as_str))),
            ),
            Err(why) => (Vec::new(), unavailable("guarded", &why)),
        },
        _ => (
            Vec::new(),
            skipped("guarded", "the commit and its base are unknown"),
        ),
    };
    checks.push(guarded_check);

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
            let check = check_generated(dir, generated, request.env);
            restore(dir);
            check
        } else {
            skipped("generated", "the worktree is not clean")
        };
        check.path = Some(generated.path.clone());
        checks.push(check);
    }
    // Each command starts from the head: what an earlier one wrote is
    // restored first.
    for (name, command) in &request.commands {
        let ran = match *command {
            CheckCommand::Unavailable(why) => Some(Ran::Unavailable(why.to_owned())),
            _ if !tree_clean => None,
            CheckCommand::Shell(command) => {
                if std::mem::replace(&mut wrote, true) {
                    restore(dir);
                }
                Some(run_shell(dir, command, request.env))
            }
            CheckCommand::Argv(argv) => {
                if std::mem::replace(&mut wrote, true) {
                    restore(dir);
                }
                Some(run_argv(dir, argv, request.env))
            }
        };
        let mut check = match ran {
            None => skipped("command", "the worktree is not clean"),
            Some(Ran::Exited(0, _)) => passed("command", String::new()),
            Some(Ran::Exited(code, tail)) => {
                failed("command", format!("exited with code {code}:\n{tail}"))
            }
            Some(Ran::Unavailable(why)) => unavailable("command", &why),
        };
        check.name = name.map(str::to_owned);
        checks.push(check);
    }
    if wrote {
        restore(dir);
    }

    // The commit's own tests must fail without it: a test that passes on
    // the base proves nothing (see `counterfactual`). It runs after the
    // commands, from the head, and puts the worktree back itself.
    let counterfactual = match (&recovered, &base_commit, &head) {
        (Err(why), _, _) => counterfactual::not_recovered(why),
        (Ok(()), Ok(base), Some(head)) => counterfactual::check(request, base, head, tree_clean),
        _ => counterfactual::not_checked("the commit and its base are unknown"),
    };
    checks.push(counterfactual.check);

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
        guarded_paths: guarded,
        counterfactual: counterfactual.tests,
        verified_ms: now_ms,
    }
}

/// The changed paths of `base..head` that match a guard glob
/// ([`GUARDED_PATHS`] plus the repository's own), sorted. Git matches the
/// globs itself, as `:(glob)` pathspecs. An error names what could not be
/// read (the tree, the configured list, the diff); [`verify`] reports it as
/// an `unavailable` check and `todo resume` refuses an approval of the commit
/// then, so neither approves a commit whose guard is unknown. Both read the
/// same list from here.
pub(super) fn guarded_paths(dir: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let specs: Vec<String> = guarded_globs(dir, base)?
        .iter()
        // `top`: the globs name top-level paths even when `dir` is below the
        // worktree's root, as the diff reports them.
        .map(|glob| format!(":(glob,top){glob}"))
        .collect();
    let mut args = vec![
        "diff",
        "--name-only",
        // A rename shows its guarded source as a deletion, which matches.
        "--no-renames",
        "-z",
        base,
        head,
        "--",
    ];
    args.extend(specs.iter().map(String::as_str));
    let out = git(dir, &args)
        .map_err(|error| format!("cannot read the diff {base}..{head}: {}", git_error(error)))?;
    let mut paths: Vec<String> = out
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The guard globs: [`GUARDED_PATHS`] plus the repository's own
/// `[review] guarded_paths` as `.herdr/checks.toml` has them at `base`. An
/// absent file there is normal (herdr's own globs guard the commit alone); a
/// list that cannot be read or parsed is an error, never silently dropped.
fn guarded_globs(dir: &Path, base: &str) -> Result<Vec<String>, String> {
    let mut globs: Vec<String> = GUARDED_PATHS
        .iter()
        .map(|glob| (*glob).to_owned())
        .collect();
    if !has_file_at(dir, base, CHECKS_FILE)? {
        return Ok(globs);
    }
    let text = git(dir, &["show", &format!("{base}:{CHECKS_FILE}")])
        .map_err(|error| format!("cannot read {CHECKS_FILE} at {base}: {}", git_error(error)))?;
    let declared = declared_guarded_paths(&text).map_err(|why| {
        format!("cannot read the [review] guarded_paths of {CHECKS_FILE} at {base}: {why}")
    })?;
    globs.extend(
        declared
            .unwrap_or_default()
            .into_iter()
            .map(|glob| glob.trim().to_owned())
            .filter(|glob| !glob.is_empty()),
    );
    globs.dedup();
    Ok(globs)
}

/// Whether `base`'s tree has `path`. A missing `.herdr/checks.toml` at the
/// base is normal, and git's message for a missing file cannot be told from
/// another failure, so this asks the tree itself: an unreadable tree is an
/// error, an absent file is not.
fn has_file_at(dir: &Path, base: &str, path: &str) -> Result<bool, String> {
    let out = git(
        dir,
        &[
            "ls-tree",
            "--name-only",
            base,
            "--",
            &format!(":(top){path}"),
        ],
    )
    .map_err(|error| format!("cannot read the tree of {base}: {}", git_error(error)))?;
    Ok(!out.trim().is_empty())
}

/// A [`Git`] failure's text.
fn git_error(error: Git) -> String {
    match error {
        Git::Unavailable(why) | Git::Failed(why) => why,
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

fn check_generated(
    dir: &Path,
    generated: &WorkerGeneratedFile,
    env: Option<&HashMap<String, String>>,
) -> WorkerVerifyCheck {
    let path = generated.path.as_str();
    match git(dir, &["ls-files", "--error-unmatch", "--", path]) {
        Ok(_) => {}
        Err(Git::Unavailable(error)) => return unavailable("generated", &error),
        Err(Git::Failed(_)) => return failed("generated", format!("{path} is not committed")),
    }
    match run_shell(dir, &generated.command, env) {
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
        name: None,
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

#[derive(Debug)]
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

/// Runs `command` with `sh -c` in `dir`, stderr merged into stdout, with
/// `env` (without `HERDR_*`) in place of the server's environment when
/// given. Exit 126 and 127 are the shell's "cannot execute" and "not
/// found"; another failure whose output shows a broken build environment
/// ([`environment_failure`]) is unavailable too.
fn run_shell(dir: &Path, command: &str, env: Option<&HashMap<String, String>>) -> Ran {
    let mut shell = Command::new("sh");
    if let Some(env) = env {
        shell
            .env_clear()
            .envs(env.iter().filter(|(name, _)| !name.starts_with("HERDR_")));
    }
    let output = match shell
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
    let text = String::from_utf8_lossy(&output.stdout);
    let tail = tail(&text);
    match output.status.code() {
        Some(code @ (126 | 127)) => {
            Ran::Unavailable(format!("{command:?} could not run (exit {code}):\n{tail}"))
        }
        Some(0) => Ran::Exited(0, tail),
        Some(code) => match environment_failure(&text) {
            Some(line) => Ran::Unavailable(format!(
                "{command:?} failed on its build environment, not the project \
                 (exit {code}; {line:?}):\n{tail}"
            )),
            None => Ran::Exited(code, tail),
        },
        None => Ran::Unavailable(format!("{command:?} was ended by a signal:\n{tail}")),
    }
}

/// Runs `argv[0]` with the other arguments as they are (no shell) in
/// `dir`, with `env` (without `HERDR_*`) in place of the server's
/// environment when given. Its output is its stdout followed by its
/// stderr. A program that cannot start is unavailable, and so is a failure
/// whose output shows a broken build environment ([`environment_failure`]).
fn run_argv(dir: &Path, argv: &[String], env: Option<&HashMap<String, String>>) -> Ran {
    let Some((program, args)) = argv.split_first() else {
        return Ran::Unavailable("the check has no program".into());
    };
    let mut command = Command::new(program);
    if let Some(env) = env {
        command
            .env_clear()
            .envs(env.iter().filter(|(name, _)| !name.starts_with("HERDR_")));
    }
    let output = match command
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => output,
        Err(error) => return Ran::Unavailable(format!("cannot run {argv:?}: {error}")),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let tail = tail(&text);
    match output.status.code() {
        Some(0) => Ran::Exited(0, tail),
        Some(code) => match environment_failure(&text) {
            Some(line) => Ran::Unavailable(format!(
                "{argv:?} failed on its build environment, not the project \
                 (exit {code}; {line:?}):\n{tail}"
            )),
            None => Ran::Exited(code, tail),
        },
        None => Ran::Unavailable(format!("{argv:?} was ended by a signal:\n{tail}")),
    }
}

/// The line of a failed command's output that shows it failed on its
/// build environment before the project's own work: a tool missing or of
/// the wrong version. Conservative: any sign of the project's own failure
/// (a rustc error code, a failing test) keeps it `failed`, and so does an
/// output matching none of these patterns:
///
/// - Zig's version check (`does not meet the required build version`) and
///   herdr's build script (`requires Zig `, `zig executable not found`);
/// - a missing cargo subcommand (`error: no such command: `, as for
///   `cargo nextest` without cargo-nextest);
/// - a rustup toolchain not installed (`error: toolchain '...' is not
///   installed`);
/// - a missing linker (`error: linker `...` not found`);
/// - a program the shell did not find (`sh: ...: command not found`,
///   `zsh: command not found: ...`, or `sh: 1: ...: not found` from dash),
///   in a script the command ran.
fn environment_failure(output: &str) -> Option<&str> {
    let lines = || output.lines().map(str::trim);
    let project_failed = lines().any(|line| {
        line.contains("error[E")
            || line.contains("test result: FAILED")
            || line.starts_with("FAIL [")
    });
    if project_failed {
        return None;
    }
    lines().find(|line| {
        line.contains("does not meet the required build version")
            || line.contains("requires Zig ")
            || line.contains("zig executable not found")
            || line.starts_with("error: no such command: ")
            || (line.starts_with("error: toolchain '") && line.contains("is not installed"))
            || (line.starts_with("error: linker `") && line.ends_with("not found"))
            || (["sh: ", "bash: ", "zsh: ", "/bin/sh: "]
                .iter()
                .any(|shell| line.starts_with(shell))
                && (line.contains("command not found") || line.ends_with(": not found")))
    })
}

pub(super) fn tail(output: &str) -> String {
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
                commands: command
                    .map(|command| (None, CheckCommand::Shell(command)))
                    .into_iter()
                    .collect(),
                generated: &generated,
                env: None,
                processes: Vec::new(),
                new_api: false,
            },
            7,
        )
    }

    fn detail<'a>(verification: &'a WorkerVerification, name: &str) -> &'a str {
        &verification
            .checks
            .iter()
            .find(|check| check.check == name)
            .unwrap_or_else(|| panic!("no {name} check: {verification:#?}"))
            .detail
    }

    #[test]
    fn the_command_runs_with_the_callers_environment_not_the_servers() {
        let (dir, base) = repo("env");
        commit(&dir, SUBJECT);
        let env: HashMap<String, String> = [
            ("PATH", std::env::var("PATH").unwrap_or_default()),
            ("VERIFY_PROBE", "from the caller".into()),
            ("HERDR_PANE_ID", "p_caller".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
        // The test process has HOME; the caller's environment here does not.
        assert!(std::env::var_os("HOME").is_some());
        let verification = verify(
            &Request {
                dir: &dir,
                base: &base,
                expected_message: SUBJECT,
                allowed_paths: &["src/**".into()],
                commands: vec![(
                    None,
                    CheckCommand::Shell(
                        "echo \"probe=$VERIFY_PROBE home=${HOME-unset} pane=${HERDR_PANE_ID-unset}\"; exit 1",
                    ),
                )],
                generated: &[],
                env: Some(&env),
                processes: Vec::new(),
                new_api: false,
            },
            7,
        );
        assert_eq!(
            outcome(&verification, "command"),
            WorkerCheckOutcome::Failed
        );
        assert!(
            detail(&verification, "command")
                .contains("probe=from the caller home=unset pane=unset"),
            "{verification:#?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_toolchain_version_mismatch_is_unavailable() {
        let (dir, base) = repo("zig");
        commit(&dir, SUBJECT);
        let output = "   Compiling ghostty-vt v0.1.0\n\
            error: failed to run custom build command for `ghostty-vt v0.1.0`\n\
            error: Your Zig version v0.17.0 does not meet the required build version of v0.16.0\n\
            zig build for vendored libghostty-vt failed: exit status: 1. Building Herdr \
            requires Zig 0.16.0; check `zig version`";
        let verification = run(
            &dir,
            &base,
            Some(&format!("printf '%s\\n' '{output}'; exit 101")),
            false,
        );
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Unavailable,
            "{verification:#?}"
        );
        assert!(detail(&verification, "command").contains("build environment"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failing_test_fails_even_beside_an_environment_message() {
        let (dir, base) = repo("test-fails");
        commit(&dir, SUBJECT);
        let output = "sh: helper: command not found\n\
            thread 'tests::it_works' panicked at src/lib.rs:3:5\n\
            test result: FAILED. 0 passed; 1 failed";
        let verification = run(
            &dir,
            &base,
            Some(&format!("printf '%s\\n' '{output}'; exit 101")),
            false,
        );
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        assert_eq!(
            outcome(&verification, "command"),
            WorkerCheckOutcome::Failed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn environment_failure_patterns() {
        for (output, environment) in [
            ("error: Your Zig version v0.17.0 does not meet the required build version of v0.16.0", true),
            ("Building Herdr requires Zig 0.16.0; check `zig version`", true),
            ("zig executable not found (looked for \"zig\")", true),
            ("error: no such command: `nextest`", true),
            ("error: toolchain '1.95.0-aarch64-apple-darwin' is not installed", true),
            ("error: linker `cc` not found", true),
            ("sh: just: command not found", true),
            ("sh: line 1: just: command not found", true),
            ("zsh: command not found: just", true),
            ("sh: 1: just: not found", true),
            // The project's own failures.
            ("error[E0308]: mismatched types", false),
            ("test result: FAILED. 3 passed; 1 failed", false),
            ("FAIL [   0.012s] herdr workers::tests::it_works", false),
            ("error: could not compile `herdr` (lib) due to 2 previous errors", false),
            ("assertion failed: requires zig", false),
            // Ambiguous: the project's test prints it, not the shell.
            ("expected \"sh: x: command not found\"", false),
            // Both: the project's failure wins.
            ("error: no such command: `x`\nerror[E0425]: cannot find value", false),
        ] {
            assert_eq!(
                environment_failure(output).is_some(),
                environment,
                "{output:?}"
            );
        }
    }

    fn outcome(verification: &WorkerVerification, name: &str) -> WorkerCheckOutcome {
        verification
            .checks
            .iter()
            .find(|check| check.check == name)
            .unwrap_or_else(|| panic!("no {name} check: {verification:#?}"))
            .outcome
    }

    /// [`repo`] with the guarded files herdr knows committed in its base:
    /// the checks, the review's checklist and one case of its golden set.
    fn repo_with_guards(name: &str) -> (PathBuf, String) {
        let (dir, _) = repo(name);
        std::fs::create_dir_all(dir.join(".herdr/review-golden/r-1-a1")).unwrap();
        std::fs::write(
            dir.join(".herdr/checks.toml"),
            "[checks]\nok = [\"true\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".herdr/review-checklist.md"),
            "- a bug class (source: abc1234)\n",
        )
        .unwrap();
        std::fs::write(dir.join(".herdr/review-golden/r-1-a1/case.json"), "{}\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "init the guards"]);
        let base = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        (dir, base)
    }

    /// The verify of `base..HEAD` in `dir`, with only `allowed` paths.
    fn verify_change(dir: &Path, base: &str, allowed: &[&str]) -> WorkerVerification {
        let allowed: Vec<String> = allowed.iter().map(|glob| (*glob).to_owned()).collect();
        verify(
            &Request {
                dir,
                base,
                expected_message: SUBJECT,
                allowed_paths: &allowed,
                commands: Vec::new(),
                generated: &[],
                env: None,
                processes: Vec::new(),
                new_api: false,
            },
            7,
        )
    }

    /// Commits every guarded path and an ordinary source file in one
    /// commit.
    fn commit_the_guards(dir: &Path) {
        std::fs::write(
            dir.join(".herdr/checks.toml"),
            "[checks]\nok = [\"sh\", \"true\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".herdr/review-checklist.md"),
            "- another bug class (source: def5678)\n",
        )
        .unwrap();
        std::fs::write(
            dir.join(".herdr/review-golden/r-1-a1/case.json"),
            "{\"case\": 1}\n",
        )
        .unwrap();
        std::fs::write(dir.join("src/a.rs"), "b\n").unwrap();
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-q", "-m", SUBJECT]);
    }

    #[test]
    fn a_commit_that_edits_the_guarded_files_is_reported_without_a_verdict() {
        let (dir, base) = repo_with_guards("guarded");
        commit_the_guards(&dir);
        let verification = verify_change(&dir, &base, &["src/**", ".herdr/**"]);
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Verified,
            "{verification:#?}"
        );
        assert_eq!(
            verification.guarded_paths,
            [
                ".herdr/checks.toml",
                ".herdr/review-checklist.md",
                ".herdr/review-golden/r-1-a1/case.json"
            ]
        );
        // Every other check is what it is without the flag: editing the
        // guarded files is legitimate, it is only never approved unnoticed.
        assert_eq!(outcome(&verification, "paths"), WorkerCheckOutcome::Passed);
        assert_eq!(
            outcome(&verification, "message"),
            WorkerCheckOutcome::Passed
        );
        assert_eq!(
            outcome(&verification, "commits"),
            WorkerCheckOutcome::Passed
        );
        // The flag is visible beside the other checks, with the paths.
        assert_eq!(
            outcome(&verification, "guarded"),
            WorkerCheckOutcome::Passed
        );
        assert_eq!(
            detail(&verification, "guarded"),
            ".herdr/checks.toml, .herdr/review-checklist.md, \
             .herdr/review-golden/r-1-a1/case.json"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_commit_that_edits_no_guarded_file_reports_none() {
        let (dir, base) = repo_with_guards("unguarded");
        // An ordinary source file, beside the guarded ones in the base.
        commit(&dir, SUBJECT);
        let verification = verify_change(&dir, &base, &["src/**"]);
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Verified,
            "{verification:#?}"
        );
        assert!(verification.guarded_paths.is_empty(), "{verification:#?}");
        assert_eq!(
            outcome(&verification, "guarded"),
            WorkerCheckOutcome::Passed
        );
        assert_eq!(detail(&verification, "guarded"), "");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_repositorys_guarded_paths_add_to_herdrs_own() {
        let (dir, _) = repo_with_guards("guarded-config");
        // Declared at the base, so no commit can add or drop its own guard.
        std::fs::write(
            dir.join(".herdr/checks.toml"),
            "[checks]\nok = [\"true\"]\n\n[review]\nguarded_paths = [\"docs/**\"]\n",
        )
        .unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "declare the guards"]);
        let base = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/a.md"), "a\n").unwrap();
        std::fs::write(dir.join(".herdr/review-checklist.md"), "- b (source: c)\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", SUBJECT]);
        // The declared glob is guarded beside herdr's own, which no
        // repository can drop.
        let verification = verify_change(&dir, &base, &["docs/**", ".herdr/**"]);
        assert_eq!(
            verification.guarded_paths,
            [".herdr/review-checklist.md", "docs/a.md"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_repository_list_leaves_herdrs_own_guards() {
        let (dir, _) = repo_with_guards("guarded-config-empty");
        std::fs::write(
            dir.join(".herdr/checks.toml"),
            "[checks]\nok = [\"true\"]\n\n[review]\nguarded_paths = []\n",
        )
        .unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "declare no guards"]);
        let base = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(dir.join(".herdr/review-checklist.md"), "- b (source: c)\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", SUBJECT]);
        let verification = verify_change(&dir, &base, &[".herdr/**"]);
        assert_eq!(
            verification.guarded_paths,
            [".herdr/review-checklist.md"],
            "{verification:#?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_guard_that_cannot_be_read_is_unavailable_never_a_pass() {
        let (dir, _) = repo_with_guards("guarded-broken");
        // A base whose checks file cannot be read as one: herdr's own guards
        // would still apply, but a configured list it cannot read is not
        // silently dropped.
        std::fs::write(dir.join(".herdr/checks.toml"), "[nope]\nx = 1\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", "break the checks file"]);
        let base = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        std::fs::write(dir.join("src/a.rs"), "b\n").unwrap();
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-q", "-m", SUBJECT]);
        let head = git_in(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
        let why = guarded_paths(&dir, &base, &head).unwrap_err();
        assert!(
            why.contains(".herdr/checks.toml") && why.contains(&base),
            "{why}"
        );
        let verification = verify_change(&dir, &base, &["src/**"]);
        assert_eq!(verification.verdict, WorkerVerdict::Unavailable);
        assert_eq!(
            outcome(&verification, "guarded"),
            WorkerCheckOutcome::Unavailable
        );
        assert!(verification.guarded_paths.is_empty(), "{verification:#?}");
        assert!(detail(&verification, "guarded").contains(".herdr/checks.toml"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_base_that_cannot_be_read_is_unavailable_never_a_pass() {
        let (dir, _) = repo_with_guards("guarded-no-diff");
        commit(&dir, SUBJECT);
        let missing = "0".repeat(40);
        let why = guarded_paths(&dir, &missing, "HEAD").unwrap_err();
        assert!(why.contains("cannot read the tree of"), "{why}");
        assert!(why.contains(&missing), "{why}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_verification_names_guarded_paths_in_json_only_when_there_are_some() {
        let (dir, base) = repo_with_guards("guarded-json");
        commit(&dir, SUBJECT);
        let plain = serde_json::to_value(verify_change(&dir, &base, &["src/**"])).unwrap();
        assert!(plain.get("guarded_paths").is_none(), "{plain}");
        let _ = std::fs::remove_dir_all(dir);

        let (dir, base) = repo_with_guards("guarded-json-set");
        commit_the_guards(&dir);
        let flagged =
            serde_json::to_value(verify_change(&dir, &base, &["src/**", ".herdr/**"])).unwrap();
        assert_eq!(
            flagged["guarded_paths"],
            serde_json::json!([
                ".herdr/checks.toml",
                ".herdr/review-checklist.md",
                ".herdr/review-golden/r-1-a1/case.json"
            ]),
            "{flagged}"
        );
        // An older client reads a verification without the field.
        let mut older = flagged.clone();
        older.as_object_mut().unwrap().remove("guarded_paths");
        assert_eq!(
            serde_json::from_value::<WorkerVerification>(older)
                .unwrap()
                .guarded_paths,
            Vec::<String>::new()
        );
        let _ = std::fs::remove_dir_all(dir);
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
    fn every_named_check_runs_from_the_head_and_all_must_pass() {
        let (dir, base) = repo("named");
        commit(&dir, SUBJECT);
        let argv = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        // The first writes a file the second must not see.
        let writes: Vec<String> = argv(&["sh", "-c", "echo x > left.txt"]);
        let clean: Vec<String> = argv(&["test", "!", "-e", "left.txt"]);
        let fails: Vec<String> = argv(&["false"]);
        let checked = |commands: Vec<(Option<&str>, CheckCommand<'_>)>| {
            verify(
                &Request {
                    dir: &dir,
                    base: &base,
                    expected_message: SUBJECT,
                    allowed_paths: &["src/**".into()],
                    commands,
                    generated: &[],
                    env: None,
                    processes: Vec::new(),
                    new_api: false,
                },
                7,
            )
        };
        let verification = checked(vec![
            (Some("writes"), CheckCommand::Argv(&writes)),
            (Some("clean"), CheckCommand::Argv(&clean)),
        ]);
        assert_eq!(
            verification.verdict,
            WorkerVerdict::Verified,
            "{verification:#?}"
        );
        let names: Vec<_> = verification
            .checks
            .iter()
            .filter(|check| check.check == "command")
            .map(|check| check.name.as_deref())
            .collect();
        assert_eq!(names, [Some("writes"), Some("clean")]);

        let verification = checked(vec![
            (Some("clean"), CheckCommand::Argv(&clean)),
            (Some("fails"), CheckCommand::Argv(&fails)),
        ]);
        assert_eq!(verification.verdict, WorkerVerdict::Failed);
        let verification = checked(vec![
            (Some("clean"), CheckCommand::Argv(&clean)),
            (Some("lint"), CheckCommand::Unavailable("no environment")),
        ]);
        assert_eq!(verification.verdict, WorkerVerdict::Unavailable);
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
                commands: Vec::new(),
                generated: &[],
                env: None,
                processes: vec!["the worker's process 42".into()],
                new_api: false,
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
