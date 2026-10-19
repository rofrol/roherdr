//! The counterfactual check: what proves that a test the commit adds or
//! changes needs the commit. A new test that passes without it proves
//! nothing, so the check takes the tests the commit adds or changes, puts
//! them — their whole file, or their whole `#[cfg(test)]` module — on the
//! run's base in the worker's own folder, and runs each by its exact name:
//! each one must fail there.
//!
//! A repository opts in through the `[counterfactual]` table of
//! `.herdr/checks.toml`, which the check reads at the commit's base, so a
//! commit cannot change its own configuration:
//!
//! ```toml
//! [counterfactual]
//! language = "rust"
//! test_globs = ["tests/**", "**/*_test.rs"]
//! build = ["cargo", "nextest", "run", "--no-run", "--locked"]
//! run = ["cargo", "nextest", "run", "--locked", "--no-tests=fail", "-E", "test(={test})"]
//! ```
//!
//! A repository without the table is not affected: the check reports that it
//! is not configured and nothing fails. A table that is incomplete or names
//! a language herdr does not scan is a failed check naming what is missing,
//! and anything the check cannot do soundly (a file whose test scopes cannot
//! be overlaid, a test whose filter runs nothing) is `unavailable`, which
//! never passes.
//!
//! The commit is found by the lines its diff adds: a `#[test]` or
//! `#[tokio::test]` function whose span (its attributes included) meets one
//! of them, in a file of `test_globs` or inside a `#[cfg(test)]` module. A
//! test the scan cannot attribute — one a macro generates — is reported
//! rather than guessed, so a commit that adds such a test never passes
//! silently. A line the commit only deletes is not in the diff's added
//! ranges, so a commit that only removes lines from a test is not "adds or
//! changes" it here.
//!
//! The worktree is put back at the commit however the check ends, also on a
//! panic, and that it is at the commit is asserted before the check reports.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    failed, git, has_file_at, passed, run_argv, tail, unavailable, Git, Ran, Request,
    WorkerVerifyCheck,
};
use crate::api::schema::{WorkerCounterfactualOutcome, WorkerCounterfactualTest};
use crate::workers::runs::{declared_counterfactual, CounterfactualConfig, CHECKS_FILE};

/// The check the verify reports this as.
const NAME: &str = "counterfactual";

/// What the check found: its check entry and each test the commit adds or
/// changes with what it did on the base.
#[derive(Debug)]
pub(super) struct Checked {
    pub(super) check: WorkerVerifyCheck,
    pub(super) tests: Vec<WorkerCounterfactualTest>,
}

impl Checked {
    /// The check did not apply: the repository does not configure it, or the
    /// commit adds no test. Nothing fails: it is reported `passed` with the
    /// reason, because the verify's verdict counts the checks that passed,
    /// and a `skipped` one would make the whole verification `unavailable`.
    fn not_applicable(why: impl Into<String>) -> Self {
        Self {
            check: passed(NAME, why.into()),
            tests: Vec::new(),
        }
    }

    /// The check could not be made sound, so it proves nothing: never a
    /// pass.
    fn unavailable(why: impl Into<String>) -> Self {
        Self {
            check: unavailable(NAME, &why.into()),
            tests: Vec::new(),
        }
    }

    /// The check ran and found what the commit claims is not so.
    fn failed(why: impl Into<String>) -> Self {
        Self {
            check: failed(NAME, why.into()),
            tests: Vec::new(),
        }
    }
}

/// Runs the check for the commit `head` of `request.dir`, whose base is
/// `base`. `tree_clean` is the verify's own finding: the check writes to the
/// worktree, so it runs only in one it may write to.
pub(super) fn check(request: &Request<'_>, base: &str, head: &str, tree_clean: bool) -> Checked {
    let dir = request.dir;
    let config = match read_config(dir, base) {
        Ok(Some(config)) => config,
        Ok(None) => {
            return Checked::not_applicable(format!(
                "not configured: {CHECKS_FILE} at the base {base} has no [counterfactual] table"
            ))
        }
        Err(ConfigError::Invalid(why)) => return Checked::failed(why),
        Err(ConfigError::Unavailable(why)) => return Checked::unavailable(why),
    };
    if !tree_clean {
        return Checked::not_applicable("the worktree is not clean");
    }
    let plans = match plans(dir, base, head, &config) {
        Ok(plans) => plans,
        Err(why) => return Checked::unavailable(why),
    };
    if plans.is_empty() {
        return Checked::not_applicable("the commit adds or changes no test");
    }
    // A file whose test scopes cannot be placed, and whose tests are
    // therefore unknown: nothing else can prove what it adds.
    let unsound: Vec<String> = plans
        .iter()
        .filter(|plan| plan.tests.is_empty())
        .filter_map(|plan| plan.error.clone())
        .collect();
    if !unsound.is_empty() {
        return Checked::unavailable(unsound.join("; "));
    }
    let root = match git(dir, &["rev-parse", "--show-toplevel"]) {
        Ok(root) => PathBuf::from(root.trim()),
        Err(Git::Unavailable(why) | Git::Failed(why)) => return Checked::unavailable(why),
    };
    let mut worktree = match Worktree::take(dir, head) {
        Ok(worktree) => worktree,
        Err(why) => return Checked::unavailable(why),
    };
    let outcomes = run_tests(request, &root, base, &plans, &config);
    let replaced = worktree.restore();
    let outcomes = match outcomes {
        Ok(outcomes) => outcomes,
        Err(why) => return Checked::unavailable(why),
    };
    if let Err(why) = replaced {
        return Checked::failed(format!(
            "the worktree was not put back at {head}, so nothing the check wrote can be \
             trusted: {why}"
        ));
    }
    report(&plans, &outcomes, base, request.new_api)
}

/// The check cannot judge the commit: its base or its head is unknown, which
/// the `commits` check already reports.
pub(super) fn not_checked(why: &str) -> Checked {
    Checked::not_applicable(why)
}

/// Why the repository's configuration cannot be used.
#[derive(Debug)]
enum ConfigError {
    /// The table is present but incomplete or names a language herdr does not
    /// scan: the check fails and says what is missing.
    Invalid(String),
    /// The table could not be read at all.
    Unavailable(String),
}

/// The validated `[counterfactual]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Config {
    test_globs: Vec<String>,
    build: Vec<String>,
    run: Vec<String>,
}

/// The `[counterfactual]` table of `.herdr/checks.toml` at `base`:
/// `Ok(None)` when the repository does not configure the check.
fn read_config(dir: &Path, base: &str) -> Result<Option<Config>, ConfigError> {
    match has_file_at(dir, base, CHECKS_FILE) {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(why) => return Err(ConfigError::Unavailable(why)),
    }
    let text = git(dir, &["show", &format!("{base}:{CHECKS_FILE}")]).map_err(|error| {
        ConfigError::Unavailable(format!(
            "cannot read {CHECKS_FILE} at {base}: {}",
            error_text(error)
        ))
    })?;
    let declared = declared_counterfactual(&text).map_err(|why| {
        ConfigError::Unavailable(format!(
            "cannot read the [counterfactual] table of {CHECKS_FILE} at {base}: {why}"
        ))
    })?;
    declared.as_ref().map(validate).transpose()
}

fn error_text(error: Git) -> String {
    match error {
        Git::Unavailable(why) | Git::Failed(why) => why,
    }
}

/// The table as the check uses it, or why it cannot run.
fn validate(config: &CounterfactualConfig) -> Result<Config, ConfigError> {
    let invalid = |why: String| Err(ConfigError::Invalid(format!("[counterfactual] {why}")));
    match config.language.as_deref() {
        Some("rust") => {}
        Some(other) => {
            return invalid(format!(
                "language {other:?} is not one herdr scans (only \"rust\")"
            ))
        }
        None => return invalid("names no language (herdr scans only \"rust\")".into()),
    }
    if config.test_globs.iter().all(|glob| glob.trim().is_empty()) {
        return invalid(
            "has no test_globs (the files whose whole content it takes from the commit)".into(),
        );
    }
    if config
        .build
        .first()
        .is_none_or(|program| program.is_empty())
    {
        return invalid("has no build program".into());
    }
    if config.run.first().is_none_or(|program| program.is_empty()) {
        return invalid("has no run program".into());
    }
    if !config
        .run
        .iter()
        .any(|argument| argument.contains("{test}"))
    {
        return invalid("has no run argument with {test} to name the test".into());
    }
    Ok(Config {
        test_globs: config
            .test_globs
            .iter()
            .map(|glob| glob.trim().to_owned())
            .filter(|glob| !glob.is_empty())
            .collect(),
        build: config.build.clone(),
        run: config.run.clone(),
    })
}

/// A changed file whose test scopes the check takes from the commit.
#[derive(Debug, Clone)]
struct Plan {
    /// Relative to the worktree's root.
    path: String,
    /// Whether the whole file comes from the commit (it matches
    /// `test_globs`), not only its `#[cfg(test)]` modules.
    whole: bool,
    /// The commit's own text of the file.
    head: String,
    /// Its `#[cfg(test)]` modules, the overlay's units when it is not whole.
    modules: Vec<Block>,
    /// The tests the commit adds or changes in it.
    tests: Vec<String>,
    /// Why its test scopes cannot be placed on the base; its tests are then
    /// unavailable.
    error: Option<String>,
}

/// The tests the commit adds or changes, one plan per file that has any.
fn plans(dir: &Path, base: &str, head: &str, config: &Config) -> Result<Vec<Plan>, String> {
    let changed = changed_files(dir, base, head)?;
    let test_files = matching_files(dir, base, head, &config.test_globs)?;
    let mut plans = Vec::new();
    for path in &changed {
        // Only a Rust source file can hold a test of the scanned language: a
        // document's example is not one.
        if !path.ends_with(".rs") {
            continue;
        }
        let added = added_lines(dir, base, head, path)?;
        if added.is_empty() {
            continue;
        }
        let Some(head_text) = file_at(dir, head, path) else {
            // The commit deleted it: it adds no test there.
            continue;
        };
        let scan = scan(&head_text);
        let whole = test_files.contains(path);
        let file_modules = file_modules(path);
        let tests: Vec<String> = scan
            .tests
            .iter()
            .filter(|test| touches(test, &added))
            .filter(|test| whole || test.cfg_test)
            .map(|test| {
                let mut name = file_modules.clone();
                name.extend(test.modules.iter().cloned());
                name.push(test.name.clone());
                name.join("::")
            })
            .collect();
        let error = if !scan.balanced {
            Some(format!(
                "{path}: its braces do not balance, so its test scopes cannot be read"
            ))
        } else {
            unattributed(&scan, &added, whole).map(|line| {
                format!(
                    "{path}:{line}: an added test attribute this scan cannot attribute to a \
                     test function (a macro-generated test), so the tests it makes cannot be \
                     proven"
                )
            })
        };
        if tests.is_empty() && error.is_none() {
            continue;
        }
        plans.push(Plan {
            path: path.clone(),
            whole,
            head: head_text,
            modules: scan.modules,
            tests,
            error,
        });
    }
    Ok(plans)
}

/// The paths `base..head` changed, root-relative.
fn changed_files(dir: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let out = git(
        dir,
        &["diff", "--name-only", "--no-renames", "-z", base, head],
    )
    .map_err(|error| {
        format!(
            "cannot read the changed paths of {base}..{head}: {}",
            error_text(error)
        )
    })?;
    Ok(out
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The changed paths that match the test globs, which git matches itself as
/// `:(glob)` pathspecs.
fn matching_files(
    dir: &Path,
    base: &str,
    head: &str,
    globs: &[String],
) -> Result<BTreeSet<String>, String> {
    let specs: Vec<String> = globs
        .iter()
        .map(|glob| format!(":(glob,top){glob}"))
        .collect();
    let mut args = vec![
        "diff",
        "--name-only",
        "--no-renames",
        "-z",
        base,
        head,
        "--",
    ];
    args.extend(specs.iter().map(String::as_str));
    let out = git(dir, &args).map_err(|error| {
        format!(
            "cannot read the changed paths matching {}: {}",
            globs.join(" "),
            error_text(error)
        )
    })?;
    Ok(out
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect())
}

/// The commit's text of `path`, `None` when the commit has no such file.
fn file_at(dir: &Path, commit: &str, path: &str) -> Option<String> {
    git(dir, &["show", &format!("{commit}:{path}")]).ok()
}

/// The head's line ranges, 1-based and inclusive, that the diff adds in
/// `path`; empty when it adds none.
fn added_lines(
    dir: &Path,
    base: &str,
    head: &str,
    path: &str,
) -> Result<Vec<(usize, usize)>, String> {
    let spec = format!(":(literal){path}");
    let diff = git(
        dir,
        &["diff", "--no-renames", "-U0", base, head, "--", &spec],
    )
    .map_err(|error| {
        format!(
            "cannot read the diff of {path} between {base} and {head}: {}",
            error_text(error)
        )
    })?;
    let mut ranges = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some((_, added)) = rest.split_once(" +") else {
            continue;
        };
        let added = added.split(' ').next().unwrap_or("");
        let (start, count) = match added.split_once(',') {
            Some((start, count)) => (start, count),
            None => (added, "1"),
        };
        let (Ok(start), Ok(count)) = (start.parse::<usize>(), count.parse::<usize>()) else {
            return Err(format!("cannot read the hunk header {line:?} of {path}"));
        };
        if count > 0 {
            ranges.push((start, start + count - 1));
        }
    }
    Ok(ranges)
}

/// Whether a test's span, its attributes included, meets an added line.
fn touches(test: &TestFn, added: &[(usize, usize)]) -> bool {
    added
        .iter()
        .any(|(first, last)| test.first_line <= *last && *first <= test.last_line)
}

/// The first added line that holds a test attribute the scan did not
/// attribute to a test function: a test a macro generates, which this check
/// cannot name and so cannot prove. Only a line that could be such a test
/// counts: one in a file of the test globs, or inside a `#[cfg(test)]`
/// module.
fn unattributed(scan: &Scan, added: &[(usize, usize)], whole: bool) -> Option<usize> {
    let lines: Vec<&str> = scan.code.lines().collect();
    for (first, last) in added {
        for line in *first..=*last {
            let in_cfg_test = scan
                .modules
                .iter()
                .any(|module| module.first_line <= line && line <= module.last_line);
            if !whole && !in_cfg_test {
                continue;
            }
            let text = lines.get(line.saturating_sub(1)).copied().unwrap_or("");
            if !line_has_test_attribute(text) {
                continue;
            }
            let covered = scan
                .tests
                .iter()
                .any(|test| test.first_line <= line && line <= test.last_line);
            let in_macro = scan
                .macro_tests
                .iter()
                .any(|(first, last)| *first <= line && line <= *last);
            if in_macro || !covered {
                return Some(line);
            }
        }
    }
    None
}

/// Whether a line of stripped source opens a test attribute.
fn line_has_test_attribute(line: &str) -> bool {
    let mut rest = line;
    while let Some((_, after)) = rest.split_once("#[") {
        let Some((attribute, tail)) = after.split_once(']') else {
            return false;
        };
        if attr_is_test(attribute) {
            return true;
        }
        rest = tail;
    }
    false
}

/// The worktree the check writes to, put back at the head however the check
/// ends.
struct Worktree<'a> {
    dir: &'a Path,
    head: &'a str,
    /// The branch the worktree was on, to return to; none when it was
    /// detached.
    branch: Option<String>,
    /// Whether the worktree is back at the head and that is proven.
    restored: bool,
}

impl<'a> Worktree<'a> {
    /// Records what to put back. Nothing is written yet.
    fn take(dir: &'a Path, head: &'a str) -> Result<Self, String> {
        let branch = match git(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
            Ok(name) => Some(name.trim().to_owned()).filter(|name| !name.is_empty()),
            Err(Git::Unavailable(why)) => return Err(why),
            // A detached HEAD: git exits 1 and says nothing.
            Err(Git::Failed(_)) => None,
        };
        Ok(Self {
            dir,
            head,
            branch,
            restored: false,
        })
    }

    /// Puts the worktree back at the head: the commit's branch checked out
    /// again, what the overlay wrote cleaned, and `HEAD` asserted to be the
    /// head, so a failed restore is reported instead of a check passing on
    /// another tree.
    fn restore(&mut self) -> Result<(), String> {
        let target = self.branch.clone().unwrap_or_else(|| self.head.to_owned());
        for args in [
            &["clean", "-fdq"][..],
            &["checkout", "-q", "--force", &target][..],
            &["clean", "-fdq"][..],
        ] {
            git(self.dir, args)
                .map_err(|error| format!("git {}: {}", args.join(" "), error_text(error)))?;
        }
        let at = git(self.dir, &["rev-parse", "--verify", "HEAD^{commit}"])
            .map_err(|error| format!("cannot read the worktree's HEAD: {}", error_text(error)))?;
        let at = at.trim();
        if at != self.head {
            return Err(format!(
                "the worktree is at {at}, not at the head {}",
                self.head
            ));
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for Worktree<'_> {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        if let Err(why) = self.restore() {
            // Before the check reports, `restore` is called and its failure
            // fails the check; here the check ended another way (a panic in
            // it), and the worktree still has to come back.
            tracing::error!(
                %why,
                dir = %self.dir.display(),
                "putting a verified worktree back after the counterfactual check failed"
            );
        }
    }
}

/// What a test did on the base.
#[derive(Debug, Clone)]
struct Outcome {
    outcome: WorkerCounterfactualOutcome,
    detail: String,
}

/// Takes the commit's test scopes to the base, builds it there and runs each
/// test there. The worktree is at the base when this returns, whatever the
/// result; the caller puts it back.
fn run_tests(
    request: &Request<'_>,
    root: &Path,
    base: &str,
    plans: &[Plan],
    config: &Config,
) -> Result<Vec<Vec<Outcome>>, String> {
    let dir = request.dir;
    for args in [
        &["clean", "-fdq"][..],
        &["checkout", "-q", "--force", "--detach", base][..],
    ] {
        git(dir, args).map_err(|error| {
            format!(
                "cannot move the worktree to the base {base}: {}",
                error_text(error)
            )
        })?;
    }
    let unplaced: Vec<Option<String>> = plans
        .iter()
        .map(|plan| overlaid(root, plan).err())
        .collect();
    let built = match run_argv(dir, &config.build, request.env) {
        Ran::Exited(0, _) => Ok(()),
        Ran::Exited(code, output) => Err(format!(
            "the base does not build with the commit's test scopes (exit {code}):\n{output}"
        )),
        Ran::Unavailable(why) => return Err(why),
    };
    let mut outcomes = Vec::with_capacity(plans.len());
    for (plan, failure) in plans.iter().zip(&unplaced) {
        let mut file = Vec::with_capacity(plan.tests.len());
        for test in &plan.tests {
            // A test whose scopes are not on the base cannot be judged,
            // whatever the base built: never a proof.
            if let Some(why) = failure {
                file.push(Outcome {
                    outcome: WorkerCounterfactualOutcome::Unavailable,
                    detail: format!("its test scopes are not on the base: {why}"),
                });
                continue;
            }
            file.push(match &built {
                Err(why) => Outcome {
                    outcome: WorkerCounterfactualOutcome::BaseUnbuildable,
                    detail: why.clone(),
                },
                Ok(()) => {
                    let argv: Vec<String> = config
                        .run
                        .iter()
                        .map(|argument| argument.replace("{test}", test))
                        .collect();
                    classify(&argv, &run_argv(dir, &argv, request.env))
                }
            });
        }
        outcomes.push(file);
    }
    Ok(outcomes)
}

/// Puts the plan's test scopes on the base: the whole file when it matches
/// the test globs, else its `#[cfg(test)]` modules in place of the base's
/// own, or appended when the base has none.
fn overlaid(root: &Path, plan: &Plan) -> Result<(), String> {
    if let Some(why) = &plan.error {
        return Err(why.clone());
    }
    if !plan.whole && plan.modules.is_empty() {
        return Ok(());
    }
    let path = root.join(&plan.path);
    if plan.whole {
        return write(&path, &plan.head);
    }
    let base = match std::fs::read_to_string(&path) {
        Ok(base) => base,
        Err(error) => {
            return Err(format!(
                "the base has no {} ({error}), so the commit's tests cannot be placed there",
                plan.path
            ))
        }
    };
    let scan = scan(&base);
    if !scan.balanced {
        return Err(format!(
            "{}: the base's braces do not balance, so its test scopes cannot be replaced",
            plan.path
        ));
    }
    let mut replacements: Vec<(usize, usize, &str)> = Vec::new();
    let mut appended = String::new();
    for module in &plan.modules {
        match scan
            .modules
            .iter()
            .find(|existing| existing.path == module.path)
        {
            Some(existing) => replacements.push((existing.start, existing.end, &module.text)),
            None if module.path.len() == 1 => {
                appended.push_str("\n\n");
                appended.push_str(&module.text);
                appended.push('\n');
            }
            None => {
                return Err(format!(
                    "{}: the base has no module {} to replace, and the commit's cannot be added \
                     at its nesting",
                    plan.path,
                    module.path.join("::")
                ))
            }
        }
    }
    let mut text = base;
    // From the last replacement to the first, so an earlier offset stays.
    replacements.sort_by_key(|replacement| std::cmp::Reverse(replacement.0));
    for (start, end, replacement) in replacements {
        text.replace_range(start..end, replacement);
    }
    text.push_str(&appended);
    write(&path, &text)
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot make {}: {error}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// What a test's run on the base says: it failed there (proven), it passed
/// (it proves nothing), or neither can be told (no such test, no test ran).
fn classify(argv: &[String], ran: &Ran) -> Outcome {
    let command = argv.join(" ");
    match ran {
        Ran::Unavailable(why) => Outcome {
            outcome: WorkerCounterfactualOutcome::Unavailable,
            detail: why.clone(),
        },
        Ran::Exited(code, output) if no_tests_ran(output) => Outcome {
            outcome: WorkerCounterfactualOutcome::Unavailable,
            detail: format!(
                "{command} ran no test for it, so it does not exist on the base as named \
                 (exit {code}):\n{output}"
            ),
        },
        Ran::Exited(0, output) => Outcome {
            outcome: WorkerCounterfactualOutcome::Passed,
            detail: format!("it passes on the base (exit 0):\n{output}"),
        },
        Ran::Exited(code, output) if shows_failure(output) => Outcome {
            outcome: WorkerCounterfactualOutcome::Failed,
            detail: format!("it failed on the base (exit {code}):\n{output}"),
        },
        Ran::Exited(code, output) => Outcome {
            outcome: WorkerCounterfactualOutcome::Unavailable,
            detail: format!(
                "{command} exited with {code} without showing that the test ran and failed:\n{output}"
            ),
        },
    }
}

/// Whether a run's output shows that it ran no test at all: the filter
/// matched nothing, which proves nothing and is never a pass.
fn no_tests_ran(output: &str) -> bool {
    output.lines().map(str::trim).any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let says = |phrase: &[&str]| words.windows(phrase.len()).any(|window| window == phrase);
        says(&["0", "tests", "run"])
            || says(&["running", "0", "tests"])
            || says(&["no", "tests", "to", "run"])
            || says(&["No", "tests", "to", "run"])
            || line.starts_with("test result: ok. 0 passed; 0 failed")
    })
}

/// Whether a run's output shows a test that ran and failed: a failing test
/// harness prints one of these.
fn shows_failure(output: &str) -> bool {
    output.lines().map(str::trim).any(|line| {
        line.starts_with("FAIL [")
            || line.starts_with("failures:")
            || line.starts_with("test result: FAILED")
            || line.contains("panicked at ")
    })
}

/// The check entry and the per-test report of what the outcomes prove.
fn report(plans: &[Plan], outcomes: &[Vec<Outcome>], base: &str, new_api: bool) -> Checked {
    let mut tests = Vec::new();
    let mut proven = 0usize;
    for (plan, file) in plans.iter().zip(outcomes) {
        for (test, outcome) in plan.tests.iter().zip(file) {
            if outcome.outcome == WorkerCounterfactualOutcome::Failed {
                proven += 1;
            }
            tests.push(WorkerCounterfactualTest {
                test: test.clone(),
                path: plan.path.clone(),
                base: outcome.outcome,
                proven: outcome.outcome == WorkerCounterfactualOutcome::Failed,
                detail: outcome.detail.clone(),
            });
        }
    }
    let of = |outcome: WorkerCounterfactualOutcome| -> Vec<&WorkerCounterfactualTest> {
        tests.iter().filter(|test| test.base == outcome).collect()
    };
    let passes = of(WorkerCounterfactualOutcome::Passed);
    if let Some(test) = passes.first() {
        return Checked {
            check: failed(
                NAME,
                format!(
                    "`{}` ({}) passes on the base {base}, so it proves nothing: a test the \
                     commit adds or changes must fail there{}{}",
                    test.test,
                    test.path,
                    more(&passes, 1),
                    evidence(test)
                ),
            ),
            tests,
        };
    }
    let unbuildable = of(WorkerCounterfactualOutcome::BaseUnbuildable);
    if let Some(test) = unbuildable.first() {
        let reason = format!(
            "the base {base} does not build with the commit's test scopes, so {} cannot be \
             shown to fail there",
            named(&unbuildable)
        );
        return if new_api {
            Checked {
                check: passed(
                    NAME,
                    format!("{reason}; --new-api accepts that, and nothing is proven"),
                ),
                tests,
            }
        } else {
            Checked {
                check: failed(
                    NAME,
                    format!(
                        "{reason}{}; a run of a commit that adds an API the base cannot build \
                         with passes --new-api\n{}",
                        more(&unbuildable, 1),
                        test.detail
                    ),
                ),
                tests,
            }
        };
    }
    let missing = of(WorkerCounterfactualOutcome::Unavailable);
    if !missing.is_empty() {
        let detail = missing
            .iter()
            .map(|test| format!("`{}` ({}): {}", test.test, test.path, test.detail))
            .collect::<Vec<_>>()
            .join("\n");
        return Checked {
            check: unavailable(
                NAME,
                &format!(
                    "cannot show that the tests the commit adds fail on the base {base}:\n{detail}"
                ),
            ),
            tests,
        };
    }
    Checked {
        check: passed(
            NAME,
            format!(
                "{proven} test(s) fail on the base {base} with the commit's test scopes: {}",
                named(&tests.iter().collect::<Vec<_>>())
            ),
        ),
        tests,
    }
}

/// The evidence of a test that proves nothing: what it printed on the base.
fn evidence(test: &WorkerCounterfactualTest) -> String {
    if test.detail.is_empty() {
        String::new()
    } else {
        format!("\n{}", tail(&test.detail))
    }
}

/// ` and N more` when a list has more than its first entries.
fn more(tests: &[&WorkerCounterfactualTest], shown: usize) -> String {
    if tests.len() > shown {
        format!(" (and {} more)", tests.len() - shown)
    } else {
        String::new()
    }
}

fn named(tests: &[&WorkerCounterfactualTest]) -> String {
    tests
        .iter()
        .map(|test| format!("`{}`", test.test))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A `#[test]` function a file's source holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TestFn {
    name: String,
    /// Its `mod`s, outer first, as the source nests them.
    modules: Vec<String>,
    /// Whether a `#[cfg(test)]` module holds it.
    cfg_test: bool,
    /// Its attributes' first line and its body's last, 1-based.
    first_line: usize,
    last_line: usize,
}

/// An inline module: the unit the overlay takes from the commit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Block {
    /// Its `mod`s, outer first.
    path: Vec<String>,
    /// Its bytes in the file, from its attributes to its closing brace.
    start: usize,
    end: usize,
    text: String,
    first_line: usize,
    last_line: usize,
}

/// What a Rust source file's items show.
#[derive(Debug, Default, Clone)]
struct Scan {
    /// Its `#[test]` functions.
    tests: Vec<TestFn>,
    /// Its topmost inline `#[cfg(test)]` modules.
    modules: Vec<Block>,
    /// Whether its braces matched: nothing in it is read soundly otherwise.
    balanced: bool,
    /// The lines of macro bodies that hold a test attribute: tests this scan
    /// cannot name.
    macro_tests: Vec<(usize, usize)>,
    /// The file as the scan reads it, strings and comments blanked, for the
    /// added lines nothing can be attributed to.
    code: String,
}

/// The modules a file's tests are in before its own: `src/a/b.rs` is `a::b`,
/// `src/a/mod.rs` is `a`, and a crate root (`lib.rs`, `main.rs`, or a file
/// outside `src/`, which cargo builds as its own test target) adds nothing,
/// as `cargo nextest`'s filter names it.
fn file_modules(path: &str) -> Vec<String> {
    let Some(rest) = path.strip_prefix("src/") else {
        return Vec::new();
    };
    let mut parts: Vec<String> = rest.split('/').map(str::to_owned).collect();
    let last = parts.pop().unwrap_or_default();
    if last == "lib.rs" || last == "main.rs" {
        return Vec::new();
    }
    if last != "mod.rs" {
        parts.push(last.strip_suffix(".rs").unwrap_or(&last).to_owned());
    }
    parts
}

/// Reads a source file's items.
fn scan(raw: &str) -> Scan {
    let code = strip(raw);
    let mut out = Scan {
        balanced: true,
        code: code.clone(),
        ..Scan::default()
    };
    let source = Source {
        raw,
        code: &code,
        bytes: code.as_bytes(),
        line_starts: line_starts(&code),
    };
    source.items(0, code.len(), &[], false, &mut out);
    out
}

/// The same bytes with the contents of string literals and comments blanked,
/// so an item, an attribute or a brace is only read where the compiler reads
/// one. Line breaks stay, so offsets and line numbers are the file's.
fn strip(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => i = skip_string(bytes, i + 1),
            b'\'' => {
                if is_char_literal(bytes, i) {
                    i = skip_char(bytes, i + 1);
                } else {
                    // A lifetime or a label: it is code.
                    out[i] = b'\'';
                    i += 1;
                }
            }
            _ if string_start(bytes, i).is_some() => {
                let (body, hashes, raw) = string_start(bytes, i).unwrap_or((i + 1, 0, false));
                i = if raw {
                    skip_raw(bytes, body, hashes)
                } else {
                    skip_string(bytes, body)
                };
            }
            byte => {
                out[i] = byte;
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether the `'` at `at` opens a char literal (`'a'`, `'\n'`, `'{'`) and
/// not a lifetime (`'a`, `'_`).
fn is_char_literal(bytes: &[u8], at: usize) -> bool {
    match bytes.get(at + 1) {
        Some(b'\\') => true,
        Some(_) => bytes.get(at + 2) == Some(&b'\''),
        None => false,
    }
}

/// The offset after a string literal's closing double quote, whose content
/// starts at `from`, an escaped character taken as one. A `'` is content
/// here (`"['^x$']"`), not a quote.
fn skip_string(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// The offset after a char literal's closing quote, whose content starts at
/// `from`.
fn skip_char(bytes: &[u8], from: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    bytes.len()
}

/// Where a string literal starting at `at` has its content, how many `#`s
/// close it and whether it is raw: `"..."`, `b"..."`, `c"..."`, `r"..."`,
/// `r#"..."#` and `br#"..."#`. `None` when no literal starts there, `b'x'`
/// among them.
fn string_start(bytes: &[u8], at: usize) -> Option<(usize, usize, bool)> {
    let mut j = at;
    if !matches!(bytes.get(j), Some(b'b') | Some(b'c') | Some(b'r')) {
        return None;
    }
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    if matches!(bytes.get(j), Some(b'b') | Some(b'c')) {
        j += 1;
    }
    let mut raw = false;
    if bytes.get(j) == Some(&b'r') {
        raw = true;
        j += 1;
    }
    let mut hashes = 0;
    if raw {
        while bytes.get(j) == Some(&b'#') {
            hashes += 1;
            j += 1;
        }
    }
    (bytes.get(j) == Some(&b'"')).then_some((j + 1, hashes, raw))
}

/// The offset after a raw string's closing quote: the first `"` followed by
/// as many `#`s as opened it.
fn skip_raw(bytes: &[u8], from: usize, hashes: usize) -> usize {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'"' && (0..hashes).all(|hash| bytes.get(i + 1 + hash) == Some(&b'#')) {
            return i + 1 + hashes;
        }
        i += 1;
    }
    bytes.len()
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (offset, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(offset + 1);
        }
    }
    starts
}

struct Source<'a> {
    /// The file as it is: what an overlay writes.
    raw: &'a str,
    /// The same bytes with strings and comments blanked: what the scan reads.
    code: &'a str,
    bytes: &'a [u8],
    line_starts: Vec<usize>,
}

impl Source<'_> {
    /// The 1-based line of a byte offset.
    fn line_of(&self, offset: usize) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(index) => index + 1,
            Err(index) => index,
        }
    }

    fn skip_trivia(&self, mut at: usize, end: usize) -> usize {
        while at < end && self.bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        at
    }

    /// The index of the close that matches the group opening at `open`.
    fn group_end(&self, open: usize, end: usize) -> Option<usize> {
        let (opening, closing) = match self.bytes[open] {
            b'{' => (b'{', b'}'),
            b'(' => (b'(', b')'),
            b'[' => (b'[', b']'),
            _ => return None,
        };
        let mut depth = 0usize;
        let mut i = open;
        while i < end {
            let byte = self.bytes[i];
            if byte == opening {
                depth += 1;
            } else if byte == closing {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            i += 1;
        }
        None
    }

    /// The identifier at `at` and the offset after it.
    fn ident_at(&self, at: usize) -> (&str, usize) {
        let mut end = at;
        while end < self.bytes.len() && ident_char(self.bytes[end]) {
            end += 1;
        }
        (&self.code[at..end], end)
    }

    /// The next identifier after `at`, and the offset after it.
    fn next_ident(&self, at: usize, end: usize) -> Option<(&str, usize)> {
        let at = self.skip_trivia(at, end);
        if at >= end || !ident_start(self.bytes[at]) {
            return None;
        }
        Some(self.ident_at(at))
    }

    /// The items of one nesting level, `start..end`, with `path` the modules
    /// the level is in and `in_cfg_test` whether one of them is.
    fn items(&self, start: usize, end: usize, path: &[String], in_cfg_test: bool, out: &mut Scan) {
        let mut i = start;
        let mut attributes: Vec<String> = Vec::new();
        let mut first_attribute: Option<(usize, usize)> = None;
        loop {
            i = self.skip_trivia(i, end);
            if i >= end {
                return;
            }
            let byte = self.bytes[i];
            if byte == b'#' {
                let mut at = i + 1;
                if at < end && self.bytes[at] == b'!' {
                    at += 1;
                }
                if at < end && self.bytes[at] == b'[' {
                    let Some(close) = self.group_end(at, end) else {
                        out.balanced = false;
                        return;
                    };
                    attributes.push(self.code[at + 1..close].trim().to_owned());
                    first_attribute.get_or_insert((i, self.line_of(i)));
                    i = close + 1;
                    continue;
                }
                i += 1;
                continue;
            }
            if byte == b';' {
                attributes.clear();
                first_attribute = None;
                i += 1;
                continue;
            }
            if !ident_start(byte) {
                if matches!(byte, b'(' | b'[' | b'{') {
                    let Some(close) = self.group_end(i, end) else {
                        out.balanced = false;
                        return;
                    };
                    i = close + 1;
                    continue;
                }
                i += 1;
                continue;
            }
            let (word, after) = self.ident_at(i);
            match word {
                "pub" | "unsafe" | "async" | "default" | "extern" | "auto" | "const" => {
                    i = after;
                }
                "mod" => {
                    let Some((name, after_name)) = self.next_ident(after, end) else {
                        i = after;
                        attributes.clear();
                        first_attribute = None;
                        continue;
                    };
                    let body = self.skip_trivia(after_name, end);
                    if body < end && self.bytes[body] == b'{' {
                        let Some(close) = self.group_end(body, end) else {
                            out.balanced = false;
                            return;
                        };
                        let cfg_test = attributes.iter().any(|attr| attr_is_cfg_test(attr));
                        let mut nested = path.to_vec();
                        nested.push(name.to_owned());
                        if cfg_test && !in_cfg_test {
                            let (start, first_line) =
                                first_attribute.unwrap_or((i, self.line_of(i)));
                            out.modules.push(Block {
                                path: nested.clone(),
                                start,
                                end: close + 1,
                                text: self.raw[start..close + 1].to_owned(),
                                first_line,
                                last_line: self.line_of(close),
                            });
                        }
                        self.items(body + 1, close, &nested, in_cfg_test || cfg_test, out);
                        i = close + 1;
                    } else {
                        // `mod name;`: the module is another file, and only a
                        // changed file of its own is read.
                        i = body + 1;
                    }
                    attributes.clear();
                    first_attribute = None;
                }
                "fn" => {
                    let Some((name, after_name)) = self.next_ident(after, end) else {
                        i = after;
                        attributes.clear();
                        first_attribute = None;
                        continue;
                    };
                    let (body, after_body) = self.fn_body(after_name, end, out);
                    let Some(body) = body else {
                        attributes.clear();
                        first_attribute = None;
                        i = after_body;
                        continue;
                    };
                    let Some(close) = self.group_end(body, end) else {
                        out.balanced = false;
                        return;
                    };
                    if attributes.iter().any(|attr| attr_is_test(attr)) {
                        let (_, first_line) = first_attribute.unwrap_or((i, self.line_of(i)));
                        out.tests.push(TestFn {
                            name: name.to_owned(),
                            modules: path.to_vec(),
                            cfg_test: in_cfg_test,
                            first_line,
                            last_line: self.line_of(close),
                        });
                    }
                    i = close + 1;
                    attributes.clear();
                    first_attribute = None;
                }
                _ => {
                    // A macro invocation, which may generate tests, or any
                    // other item, which cannot hold a `#[test]` function.
                    let after_trivia = self.skip_trivia(after, end);
                    if after_trivia < end && self.bytes[after_trivia] == b'!' {
                        let Some((open, close)) = self.macro_body(after_trivia, end) else {
                            out.balanced = false;
                            return;
                        };
                        self.macro_tests(open, close, out);
                        i = close + 1;
                    } else {
                        i = self.skip_item(i, end);
                    }
                    attributes.clear();
                    first_attribute = None;
                }
            }
        }
    }

    /// The `{` a function's body opens at, and where the item goes on when
    /// it has none (a trait's declaration).
    fn fn_body(&self, mut at: usize, end: usize, out: &mut Scan) -> (Option<usize>, usize) {
        while at < end {
            at = self.skip_trivia(at, end);
            if at >= end {
                return (None, end);
            }
            match self.bytes[at] {
                b'{' => return (Some(at), at),
                b';' => return (None, at + 1),
                // Parameters and return types: a `{` inside them (a const
                // generic's argument) is rare enough that reading it as the
                // body only widens a test's span, which reports rather than
                // passes.
                b'(' | b'[' => match self.group_end(at, end) {
                    Some(close) => at = close + 1,
                    None => {
                        out.balanced = false;
                        return (None, end);
                    }
                },
                _ => at += 1,
            }
        }
        (None, end)
    }

    /// The group a macro invocation's body is in, from its `!`: `name!(...)`,
    /// `name![...]`, `name!{...}` and `macro_rules! name { ... }`.
    fn macro_body(&self, bang: usize, end: usize) -> Option<(usize, usize)> {
        let mut at = self.skip_trivia(bang + 1, end);
        if at < end && ident_start(self.bytes[at]) {
            at = self.ident_at(at).1;
            at = self.skip_trivia(at, end);
        }
        if at < end && matches!(self.bytes[at], b'{' | b'(' | b'[') {
            return self.group_end(at, end).map(|close| (at, close));
        }
        None
    }

    /// The lines of a macro's body that hold a test attribute: tests this
    /// scan cannot name, which it reports rather than guesses.
    fn macro_tests(&self, open: usize, close: usize, out: &mut Scan) {
        for line in self.line_of(open)..=self.line_of(close) {
            let Some(start) = self.line_starts.get(line - 1) else {
                continue;
            };
            let end = self
                .line_starts
                .get(line)
                .copied()
                .unwrap_or(self.code.len())
                .min(self.code.len());
            if *start < self.code.len() && line_has_test_attribute(&self.code[*start..end]) {
                out.macro_tests.push((line, line));
            }
        }
    }

    /// The offset after the item that starts at `start`: after its `;` or
    /// after its first block.
    fn skip_item(&self, start: usize, end: usize) -> usize {
        let mut at = start;
        while at < end {
            at = self.skip_trivia(at, end);
            if at >= end {
                return end;
            }
            match self.bytes[at] {
                b'{' => return self.group_end(at, end).map_or(end, |close| close + 1),
                b'(' | b'[' => match self.group_end(at, end) {
                    Some(close) => at = close + 1,
                    None => return end,
                },
                b';' => return at + 1,
                _ => at += 1,
            }
        }
        end
    }
}

/// Whether an attribute's text is a test attribute: its path's last segment
/// is `test` (`test`, `tokio::test`, `tokio::test(flavor = "...")`), which
/// `cfg(test)` is not.
fn attr_is_test(attribute: &str) -> bool {
    let path = attribute
        .trim()
        .split(['(', '=', ' ', ','])
        .next()
        .unwrap_or("")
        .trim();
    path == "test" || path.ends_with("::test")
}

/// Whether an attribute is a `cfg` whose predicates hold `test`:
/// `#[cfg(test)]`, `#[cfg(all(test, unix))]`.
fn attr_is_cfg_test(attribute: &str) -> bool {
    let attribute = attribute.trim();
    let Some(rest) = attribute.strip_prefix("cfg") else {
        return false;
    };
    let Some(inner) = rest
        .trim()
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    else {
        return false;
    };
    inner
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|token| token == "test")
}

fn ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn ident_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::WorkerCheckOutcome;
    use super::*;
    use crate::api::schema::{WorkerCounterfactualOutcome, WorkerVerdict, WorkerVerification};
    use std::collections::HashMap;
    use std::process::Command;

    const SUBJECT: &str = "feat: one test";

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

    fn head_of(dir: &Path) -> String {
        git_in(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn write(dir: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let path = dir.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, text).unwrap();
        }
    }

    fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
        write(dir, files);
        git_in(dir, &["add", "."]);
        git_in(dir, &["commit", "-q", "-m", SUBJECT]);
        head_of(dir)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "herdr-counterfactual-{name}-{}-{}",
            std::process::id(),
            super::super::super::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const CHECKS: &str = "[checks]\n\
        ok = [\"true\"]\n\
        \n\
        [counterfactual]\n\
        language = \"rust\"\n\
        test_globs = [\"tests/**\", \"**/*_test.rs\"]\n\
        build = [\"sh\", \"build.sh\"]\n\
        run = [\"sh\", \"run.sh\", \"{test}\"]\n";

    const BUILD_SH: &str = "#!/bin/sh\n\
        echo \"build $@\" >> calls.log\n\
        if [ -f build.rc ]; then exit \"$(cat build.rc)\"; fi\n\
        exit 0\n";

    /// The test harness stand-in: it logs its arguments and reads the
    /// verdict for a test from `run.<test>.rc`, printing what `cargo
    /// nextest` prints when the filter matches nothing when there is none.
    const RUN_SH: &str = "#!/bin/sh\n\
        echo \"run $@\" >> calls.log\n\
        if [ ! -f \"run.$1.rc\" ]; then\n\
          echo \"running 0 tests\"\n\
          exit 0\n\
        fi\n\
        rc=$(cat \"run.$1.rc\")\n\
        if [ \"$rc\" = 0 ]; then\n\
          echo \"PASS [   0.001s] (1/1) binary $1\"\n\
          exit 0\n\
        fi\n\
        echo \"FAIL [   0.001s] (1/1) binary $1\"\n\
        exit \"$rc\"\n";

    /// A repository whose `[counterfactual]` build and run are scripts, so
    /// the procedure runs without cargo.
    fn repository(name: &str) -> (PathBuf, String) {
        let dir = temp_dir(name);
        git_in(&dir, &["init", "-q", "-b", "master"]);
        write(
            &dir,
            &[
                (".gitignore", "/target\nrun.*.rc\nbuild.rc\ncalls.log\n"),
                ("src/lib.rs", "pub fn base() {}\n"),
                (".herdr/checks.toml", CHECKS),
                ("build.sh", BUILD_SH),
                ("run.sh", RUN_SH),
            ],
        );
        let base = commit(&dir, &[]);
        (dir, base)
    }
    fn request<'a>(dir: &'a Path, base: &'a str, new_api: bool) -> Request<'a> {
        Request {
            dir,
            base,
            expected_message: SUBJECT,
            allowed_paths: &[],
            commands: Vec::new(),
            generated: &[],
            env: None,
            processes: Vec::new(),
            new_api,
        }
    }

    /// The check's finding for the commit `dir` is at.
    fn checked_commit(dir: &Path, base: &str, new_api: bool) -> Checked {
        let head = head_of(dir);
        check(&request(dir, base, new_api), base, &head, true)
    }

    fn reported(checked: &Checked) -> Vec<(String, WorkerCounterfactualOutcome, bool)> {
        checked
            .tests
            .iter()
            .map(|test| (test.test.clone(), test.base, test.proven))
            .collect()
    }

    fn calls(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
    }

    /// A `#[cfg(test)] mod tests` holding `body`.
    fn test_module(body: &str) -> String {
        format!("pub fn base() {{}}\n#[cfg(test)]\nmod tests {{\n{body}}}\n")
    }

    // ---- discovery ------------------------------------------------------

    /// The tests the commit adds or changes, as the check finds them.
    fn discovered(dir: &Path, base: &str) -> Vec<String> {
        let head = head_of(dir);
        let config = match read_config(dir, base).unwrap() {
            Some(config) => config,
            None => panic!("no [counterfactual] table"),
        };
        plans(dir, base, &head, &config)
            .unwrap()
            .into_iter()
            .flat_map(|plan| plan.tests)
            .collect()
    }

    fn discovery_repository(name: &str) -> (PathBuf, String) {
        let dir = temp_dir(name);
        git_in(&dir, &["init", "-q", "-b", "master"]);
        let tests = test_module("    #[test]\n    fn old() { assert!(true); }\n");
        write(
            &dir,
            &[
                (".gitignore", "/target\nrun.*.rc\nbuild.rc\ncalls.log\n"),
                (".herdr/checks.toml", CHECKS),
                ("build.sh", BUILD_SH),
                ("run.sh", RUN_SH),
                ("src/lib.rs", &tests),
            ],
        );
        let base = commit(&dir, &[]);
        (dir, base)
    }

    #[test]
    fn an_added_test_is_discovered() {
        let (dir, base) = discovery_repository("added");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module(
                    "    #[test]\n    fn old() { assert!(true); }\n\
                     \x20   #[test]\n    fn added() { assert!(false); }\n",
                ),
            )],
        );
        assert_eq!(discovered(&dir, &base), vec!["tests::added"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_test_body_is_discovered() {
        let (dir, base) = discovery_repository("body");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn old() { assert!(!false); }\n"),
            )],
        );
        assert_eq!(discovered(&dir, &base), vec!["tests::old"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_changed_test_attribute_is_discovered() {
        let (dir, base) = discovery_repository("attribute");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[tokio::test]\n    fn old() { assert!(true); }\n"),
            )],
        );
        assert_eq!(discovered(&dir, &base), vec!["tests::old"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_renamed_test_is_discovered_under_its_new_name() {
        let (dir, base) = discovery_repository("rename");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn renamed() { assert!(true); }\n"),
            )],
        );
        assert_eq!(discovered(&dir, &base), vec!["tests::renamed"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_in_a_new_test_file_is_discovered() {
        let (dir, base) = discovery_repository("newtestfile");
        commit(
            &dir,
            &[(
                "tests/new.rs",
                "mod inner {\n    #[test]\n    fn added() {}\n}\n",
            )],
        );
        // A test crate is its own root: no module comes from the file.
        assert_eq!(discovered(&dir, &base), vec!["inner::added"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_in_a_string_or_a_comment_is_not_discovered() {
        let (dir, base) = discovery_repository("notatest");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module(
                    "    // #[test]\n\
                     \x20   // fn commented() {}\n\
                     \x20   const T: &str = \"#[test]\\nfn quoted() {}\";\n\
                     \x20   #[test]\n    fn old() { assert!(true); }\n",
                ),
            )],
        );
        assert!(discovered(&dir, &base).is_empty());
        // Nothing is reported as an unattributable test either.
        let head = head_of(&dir);
        let checked = check(&request(&dir, &base, false), &base, &head, true);
        assert!(
            checked.check.detail.contains("adds or changes no test"),
            "{checked:#?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_is_not_rust_holds_no_test_of_the_scanned_language() {
        let (dir, base) = discovery_repository("notrust");
        commit(
            &dir,
            &[(
                "tests/example.md",
                "```rust\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn example() {}\n}\n```\n",
            )],
        );
        assert!(discovered(&dir, &base).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_a_macro_generates_is_unavailable_never_guessed() {
        let (dir, base) = discovery_repository("macro");
        commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module(
                    "    #[test]\n    fn old() { assert!(true); }\n\
                     \x20   cases! {\n        #[test]\n        fn made() {}\n    }\n",
                ),
            )],
        );
        let head = head_of(&dir);
        let checked = check(&request(&dir, &base, false), &base, &head, true);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Unavailable,
            "{checked:#?}"
        );
        assert!(
            checked.check.detail.contains("macro-generated"),
            "{checked:#?}"
        );
        assert!(checked.tests.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- the scan -------------------------------------------------------

    #[test]
    fn the_scan_finds_tests_and_the_modules_they_are_in() {
        let scan = scan(
            "pub fn a() {}\n\
             #[cfg(test)]\n\
             mod tests {\n\
                 use super::*;\n\
                 #[test]\n\
                 fn one() { assert_eq!(1, 1); }\n\
                 mod inner {\n\
                     #[tokio::test]\n\
                     async fn two() {}\n\
                 }\n\
             }\n",
        );
        assert!(scan.balanced);
        assert_eq!(
            scan.modules
                .iter()
                .map(|module| module.path.clone())
                .collect::<Vec<_>>(),
            vec![vec!["tests".to_owned()]]
        );
        assert_eq!(
            scan.tests
                .iter()
                .map(|test| (test.name.as_str(), test.modules.clone(), test.cfg_test))
                .collect::<Vec<_>>(),
            vec![
                ("one", vec!["tests".to_owned()], true),
                ("two", vec!["tests".to_owned(), "inner".to_owned()], true),
            ]
        );
    }

    #[test]
    fn a_test_in_a_string_a_comment_or_a_char_literal_is_not_a_test() {
        let scan = scan(
            "// #[test]\n\
             /* #[test] fn hidden() {} */\n\
             const D: &str = \"#[test] fn hidden() {}\";\n\
             const C: char = '{';\n\
             #[cfg(test)]\n\
             mod tests {\n\
                 // #[test]\n\
                 fn helper() {}\n\
             }\n",
        );
        assert!(scan.balanced);
        assert!(scan.tests.is_empty(), "{scan:#?}");
    }

    #[test]
    fn a_test_a_macro_generates_is_not_named() {
        let scan = scan(
            "#[cfg(test)]\n\
             mod tests {\n\
                 cases! {\n\
                     #[test]\n\
                     fn made_by_a_macro() {}\n\
                 }\n\
             }\n",
        );
        assert!(scan.balanced);
        assert!(scan.tests.is_empty());
        assert_eq!(scan.macro_tests, vec![(4, 4)]);
    }

    #[test]
    fn a_module_of_tests_declared_elsewhere_has_no_block_to_take() {
        let scan = scan("#[cfg(test)]\nmod tests;\n");
        assert!(scan.balanced);
        assert!(scan.modules.is_empty());
        assert!(scan.tests.is_empty());
    }

    #[test]
    fn a_file_whose_braces_do_not_balance_is_not_read() {
        let scan = scan("#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n");
        assert!(!scan.balanced);
    }

    #[test]
    fn a_string_holding_a_quote_a_brace_or_a_backslash_is_still_a_string() {
        let source = concat!(
            "const P: &str = r\"C:\\worktrees\\\";\n",
            "const R: &str = r#\"id = \"x\"\n[[rules]]\n\"#;\n",
            "const Q: &str = \"['^x$'] {}\";\n",
            "const B: char = '}';\n",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn one() {}\n}\n",
        );
        let scan = scan(source);
        assert!(scan.balanced);
        assert_eq!(
            scan.tests
                .iter()
                .map(|test| test.name.as_str())
                .collect::<Vec<_>>(),
            vec!["one"]
        );
    }

    fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    #[test]
    fn real_sources_scan_as_herdr_writes_them() {
        // The scan reads herdr's own sources: nested modules, impls, macros
        // and attributes must not confuse it, and a test it finds must have
        // the name `cargo nextest` lists it under.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = Vec::new();
        rust_sources(&root.join("src"), &mut sources);
        assert!(sources.len() > 100, "{} sources", sources.len());
        for path in &sources {
            let text = std::fs::read_to_string(path).unwrap();
            let scan = scan(&text);
            assert!(scan.balanced, "{} does not balance", path.display());
        }
        let text = std::fs::read_to_string(root.join("src/workers/verify.rs")).unwrap();
        let scan = scan(&text);
        assert!(
            scan.tests.iter().any(|test| test.name
                == "a_missing_command_is_unavailable_never_verified"
                && test.modules == vec!["tests".to_owned()]
                && test.cfg_test),
            "{:#?}",
            scan.tests
        );
    }

    // ---- overlay --------------------------------------------------------

    #[test]
    fn the_modules_of_a_file_are_its_path_under_src() {
        assert_eq!(file_modules("src/cli/handoff.rs"), vec!["cli", "handoff"]);
        assert_eq!(file_modules("src/a/mod.rs"), vec!["a"]);
        assert!(file_modules("src/lib.rs").is_empty());
        assert!(file_modules("src/main.rs").is_empty());
        assert!(file_modules("tests/cli.rs").is_empty());
    }

    #[test]
    fn a_module_the_base_has_is_replaced_and_one_it_lacks_is_appended() {
        let dir = temp_dir("overlay");
        let root = dir.clone();
        // The base has `mod tests`; the commit's replaces it.
        write(
            &dir,
            &[(
                "replaced.rs",
                "pub fn a() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() {}\n}\n",
            )],
        );
        let plan = Plan {
            path: "replaced.rs".into(),
            whole: false,
            head: String::new(),
            modules: scan(
                "pub fn a() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn added() {}\n}\n",
            )
            .modules,
            tests: Vec::new(),
            error: None,
        };
        overlaid(&root, &plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("replaced.rs")).unwrap(),
            "pub fn a() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn added() {}\n}\n"
        );

        // The base has no module: the commit's is appended.
        write(&dir, &[("appended.rs", "pub fn a() {}\n")]);
        let plan = Plan {
            path: "appended.rs".into(),
            whole: false,
            head: String::new(),
            modules: scan("#[cfg(test)]\nmod tests {\n    #[test]\n    fn added() {}\n}\n").modules,
            tests: Vec::new(),
            error: None,
        };
        overlaid(&root, &plan).unwrap();
        let text = std::fs::read_to_string(dir.join("appended.rs")).unwrap();
        assert!(
            text.starts_with("pub fn a() {}\n\n\n#[cfg(test)]\nmod tests {"),
            "{text}"
        );
        assert_eq!(scan(&text).tests.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_whose_braces_do_not_balance_cannot_be_overlaid() {
        let dir = temp_dir("unbalanced");
        let root = dir.clone();
        write(
            &dir,
            &[(
                "broken.rs",
                "pub fn a() {}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn old() {}\n",
            )],
        );
        let plan = Plan {
            path: "broken.rs".into(),
            whole: false,
            head: String::new(),
            modules: scan("#[cfg(test)]\nmod tests {\n    #[test]\n    fn added() {}\n}\n").modules,
            tests: Vec::new(),
            error: None,
        };
        let error = overlaid(&root, &plan).unwrap_err();
        assert!(error.contains("do not balance"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_file_is_taken_whole_from_the_commit() {
        let dir = temp_dir("whole");
        let root = dir.clone();
        write(&dir, &[("tests/x.rs", "#[test]\nfn old() {}\n")]);
        let plan = Plan {
            path: "tests/x.rs".into(),
            whole: true,
            head: "#[test]\nfn added() {}\n".into(),
            modules: Vec::new(),
            tests: Vec::new(),
            error: None,
        };
        overlaid(&root, &plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("tests/x.rs")).unwrap(),
            "#[test]\nfn added() {}\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_module_the_base_cannot_hold_at_its_nesting_is_unavailable() {
        let dir = temp_dir("nesting");
        let root = dir.clone();
        write(&dir, &[("nested.rs", "pub fn a() {}\n")]);
        let plan = Plan {
            path: "nested.rs".into(),
            whole: false,
            head: String::new(),
            modules: scan("mod inner {\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn added() {}\n    }\n}\n")
                .modules,
            tests: Vec::new(),
            error: None,
        };
        let error = overlaid(&root, &plan).unwrap_err();
        assert!(error.contains("inner::tests"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- the procedure --------------------------------------------------

    #[test]
    fn a_test_that_fails_on_the_base_is_proven_and_the_worktree_is_put_back() {
        let (dir, base) = repository("proven");
        write(&dir, &[("run.tests::added.rc", "1\n")]);
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert_eq!(
            reported(&checked),
            vec![(
                "tests::added".to_owned(),
                WorkerCounterfactualOutcome::Failed,
                true
            )]
        );
        assert_eq!(head_of(&dir), head);
        assert_eq!(git_in(&dir, &["status", "--porcelain"]).trim(), "");
        assert_eq!(calls(&dir), "build \nrun tests::added\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_that_passes_on_the_base_fails_the_verify() {
        let (dir, base) = repository("passes");
        write(&dir, &[("run.tests::added.rc", "0\n")]);
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert!(true); }\n"),
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Failed,
            "{checked:#?}"
        );
        assert!(
            checked.check.detail.contains("tests::added"),
            "{checked:#?}"
        );
        assert!(
            checked.check.detail.contains("proves nothing"),
            "{checked:#?}"
        );
        assert_eq!(
            reported(&checked),
            vec![(
                "tests::added".to_owned(),
                WorkerCounterfactualOutcome::Passed,
                false
            )]
        );
        assert_eq!(head_of(&dir), head);
        // --new-api never accepts a test that passes on the base.
        let checked = checked_commit(&dir, &base, true);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Failed,
            "{checked:#?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_test_the_base_does_not_have_is_unavailable() {
        let (dir, base) = repository("missing");
        let head = commit(
            &dir,
            &[(
                "tests/new.rs",
                "#[test]\nfn added() { assert_eq!(1, 2); }\n",
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Unavailable,
            "{checked:#?}"
        );
        assert_eq!(
            reported(&checked),
            vec![(
                "added".to_owned(),
                WorkerCounterfactualOutcome::Unavailable,
                false
            )]
        );
        assert!(
            checked.check.detail.contains("does not exist on the base"),
            "{checked:#?}"
        );
        assert_eq!(head_of(&dir), head);
        assert_eq!(git_in(&dir, &["status", "--porcelain"]).trim(), "");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_base_that_does_not_build_fails_unless_new_api() {
        let (dir, base) = repository("unbuildable");
        write(&dir, &[("build.rc", "1\n"), ("run.tests::added.rc", "1\n")]);
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Failed,
            "{checked:#?}"
        );
        assert!(checked.check.detail.contains("--new-api"), "{checked:#?}");
        assert_eq!(
            reported(&checked),
            vec![(
                "tests::added".to_owned(),
                WorkerCounterfactualOutcome::BaseUnbuildable,
                false
            )]
        );
        assert_eq!(head_of(&dir), head);
        assert_eq!(git_in(&dir, &["status", "--porcelain"]).trim(), "");

        let checked = checked_commit(&dir, &base, true);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert!(checked.check.detail.contains("--new-api"), "{checked:#?}");
        assert_eq!(
            reported(&checked),
            vec![(
                "tests::added".to_owned(),
                WorkerCounterfactualOutcome::BaseUnbuildable,
                false
            )]
        );
        // It changes nothing else: no test was run.
        let calls = calls(&dir);
        assert!(calls.starts_with("build "), "{calls}");
        assert!(!calls.contains("run tests::added"), "{calls}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_repository_without_the_table_is_not_affected() {
        let (dir, _) = repository("absent");
        // The base is the commit without the table: the check reads its
        // configuration there.
        let base = commit(
            &dir,
            &[(".herdr/checks.toml", "[checks]\nok = [\"true\"]\n")],
        );
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert!(true); }\n"),
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert!(
            checked.check.detail.contains("not configured"),
            "{checked:#?}"
        );
        assert!(checked.tests.is_empty());
        assert!(!dir.join("calls.log").exists());
        assert_eq!(head_of(&dir), head);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_incomplete_table_fails_naming_what_is_missing() {
        for (table, missing) in [
            (
                "language = \"python\"\ntest_globs = [\"tests/**\"]\n\
                 build = [\"sh\", \"build.sh\"]\nrun = [\"sh\", \"run.sh\", \"{test}\"]\n",
                "python",
            ),
            (
                "test_globs = [\"tests/**\"]\nbuild = [\"sh\", \"build.sh\"]\n\
                 run = [\"sh\", \"run.sh\", \"{test}\"]\n",
                "no language",
            ),
            (
                "language = \"rust\"\nbuild = [\"sh\", \"build.sh\"]\n\
                 run = [\"sh\", \"run.sh\", \"{test}\"]\n",
                "test_globs",
            ),
            (
                "language = \"rust\"\ntest_globs = [\"tests/**\"]\n\
                 run = [\"sh\", \"run.sh\", \"{test}\"]\n",
                "build",
            ),
            (
                "language = \"rust\"\ntest_globs = [\"tests/**\"]\nbuild = [\"sh\", \"build.sh\"]\n",
                "run",
            ),
            (
                "language = \"rust\"\ntest_globs = [\"tests/**\"]\n\
                 build = [\"sh\", \"build.sh\"]\nrun = [\"sh\", \"run.sh\"]\n",
                "{test}",
            ),
        ] {
            let (dir, _) = repository("incomplete");
            let checks = format!("[checks]\nok = [\"true\"]\n\n[counterfactual]\n{table}");
            let base = commit(&dir, &[(".herdr/checks.toml", checks.as_str())]);
            let head = commit(
                &dir,
                &[(
                    "src/lib.rs",
                    &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
                )],
            );
            let checked = checked_commit(&dir, &base, false);
            assert_eq!(
                checked.check.outcome,
                WorkerCheckOutcome::Failed,
                "{table}: {checked:#?}"
            );
            assert!(
                checked.check.detail.contains(missing),
                "{table}: {:#?}",
                checked.check
            );
            assert_eq!(head_of(&dir), head);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_commit_that_adds_no_test_needs_no_build() {
        let (dir, base) = repository("notest");
        let head = commit(
            &dir,
            &[("src/lib.rs", "pub fn base() {}\npub fn added() {}\n")],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert!(
            checked.check.detail.contains("adds or changes no test"),
            "{checked:#?}"
        );
        assert!(checked.tests.is_empty());
        assert!(!dir.join("calls.log").exists());
        assert_eq!(head_of(&dir), head);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_the_tests_the_commit_changes_are_proven() {
        let (dir, base) = discovery_repository("only-changed");
        write(
            &dir,
            &[("run.tests::added.rc", "1\n"), ("run.tests::old.rc", "0\n")],
        );
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module(
                    "    #[test]\n    fn old() { assert!(true); }\n\
                     \x20   #[test]\n    fn added() { assert_eq!(1, 2); }\n",
                ),
            )],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert_eq!(
            reported(&checked),
            vec![(
                "tests::added".to_owned(),
                WorkerCounterfactualOutcome::Failed,
                true
            )]
        );
        assert!(!calls(&dir).contains("run tests::old"), "{}", calls(&dir));
        assert_eq!(head_of(&dir), head);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_source_file_whose_tests_cannot_be_placed_is_unavailable() {
        let (dir, base) = repository("newfile");
        let head = commit(
            &dir,
            &[
                ("src/lib.rs", "pub fn base() {}\nmod added;\n"),
                (
                    "src/added.rs",
                    &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
                ),
            ],
        );
        let checked = checked_commit(&dir, &base, false);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Unavailable,
            "{checked:#?}"
        );
        assert!(
            checked
                .check
                .detail
                .contains("the base has no src/added.rs"),
            "{checked:#?}"
        );
        assert_eq!(head_of(&dir), head);
        assert_eq!(git_in(&dir, &["status", "--porcelain"]).trim(), "");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_panic_mid_check_still_puts_the_worktree_back() {
        let (dir, base) = repository("panic");
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
            )],
        );
        let committed = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        let panicked = {
            let dir = dir.clone();
            let base = base.clone();
            let head = head.clone();
            std::panic::catch_unwind(move || {
                let _worktree = Worktree::take(&dir, &head).unwrap();
                git(&dir, &["clean", "-fdq"]).unwrap();
                git(&dir, &["checkout", "-q", "--force", "--detach", &base]).unwrap();
                std::fs::write(dir.join("src/lib.rs"), "half an overlay\n").unwrap();
                panic!("a panic inside the check");
            })
        };
        assert!(panicked.is_err());
        assert_eq!(head_of(&dir), head);
        assert_eq!(git_in(&dir, &["status", "--porcelain"]).trim(), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
            committed
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_restore_that_cannot_make_it_back_is_reported() {
        let (dir, base) = repository("restore");
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
            )],
        );
        let wrong = "0".repeat(40);
        let mut worktree = Worktree {
            dir: &dir,
            head: &wrong,
            branch: None,
            restored: false,
        };
        let error = worktree.restore().unwrap_err();
        assert!(error.contains("git checkout"), "{error}");
        assert!(!worktree.restored);
        git_in(&dir, &["checkout", "-q", "--force", &head]);
        assert_eq!(Some(head_of(&dir)), Some(head));
        drop(worktree);
        let _ = std::fs::remove_dir_all(dir);
        let _ = base;
    }

    #[test]
    fn the_checks_environment_is_what_the_run_uses() {
        let (dir, _base) = repository("env");
        write(
            &dir,
            &[(
                "run.sh",
                "#!/bin/sh\necho \"run $@ probe=$PROBE\" >> calls.log\n\
                 echo \"FAIL [   0.001s] (1/1) binary $1\"\nexit 1\n",
            )],
        );
        let base = commit(&dir, &[]);
        let head = commit(
            &dir,
            &[(
                "src/lib.rs",
                &test_module("    #[test]\n    fn added() { assert_eq!(1, 2); }\n"),
            )],
        );
        assert_eq!(head, head_of(&dir));
        let env: HashMap<String, String> = [
            ("PATH", std::env::var("PATH").unwrap_or_default()),
            ("PROBE", "from the caller".into()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect();
        let mut request = request(&dir, &base, false);
        request.env = Some(&env);
        let checked = check(&request, &base, &head, true);
        assert_eq!(
            checked.check.outcome,
            WorkerCheckOutcome::Passed,
            "{checked:#?}"
        );
        assert!(
            calls(&dir).contains("probe=from the caller"),
            "{}",
            calls(&dir)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_verification_reports_its_tests_only_when_checked() {
        let mut verification = WorkerVerification {
            verdict: WorkerVerdict::Verified,
            base: "b".into(),
            head: Some("h".into()),
            commits: Vec::new(),
            guarded_paths: Vec::new(),
            checks: Vec::new(),
            counterfactual: Vec::new(),
            verified_ms: 7,
        };
        let json = serde_json::to_value(&verification).unwrap();
        assert!(json.get("counterfactual").is_none(), "{json}");
        verification.counterfactual = vec![WorkerCounterfactualTest {
            test: "tests::added".into(),
            path: "src/lib.rs".into(),
            base: WorkerCounterfactualOutcome::Failed,
            proven: true,
            detail: String::new(),
        }];
        let json = serde_json::to_value(&verification).unwrap();
        assert_eq!(json["counterfactual"][0]["test"], "tests::added");
        assert_eq!(json["counterfactual"][0]["base"], "failed");
        assert_eq!(json["counterfactual"][0]["proven"], true);
        assert!(json["counterfactual"][0].get("detail").is_none());
        let restored: WorkerVerification = serde_json::from_value(json).unwrap();
        assert_eq!(restored, verification);
    }
}
