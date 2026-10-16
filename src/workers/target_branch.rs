//! Where a TODO run lands and where it pushes. The target is the branch
//! checked out in the worktree `todo run` is started from (the
//! coordinator's checkout), read at the preflight with that worktree's real
//! path; a detached HEAD refuses, and no branch is ever guessed. The push
//! goes where git's own configuration sends that branch ([`push_of`]), and
//! nowhere when git names no single destination. A run stores all of it and
//! each later step reads it back: [`commit_of`] stops when the branch no
//! longer resolves, and [`check_landing`] when the worktree no longer has it
//! checked out cleanly, instead of retargeting.

use std::path::{Path, PathBuf};

use super::runs::git_with;
use crate::api::schema::TodoRunPush;

/// The branch of a run recorded before runs stored theirs: the only branch
/// such a run could have targeted.
pub(super) const LEGACY_TARGET_BRANCH: &str = "master";

/// The worktree a run lands in and the branch it has checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Landing {
    /// The worktree's real path.
    pub(super) worktree: PathBuf,
    pub(super) branch: String,
}

/// The worktree `dir` is in and the branch it has checked out; refused for
/// a detached HEAD.
pub(super) fn checked_out(dir: &Path) -> Result<Landing, String> {
    let worktree = worktree_of(dir)?;
    match git_with(
        &worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        None,
    ) {
        Ok(branch) if !branch.trim().is_empty() => Ok(Landing {
            worktree,
            branch: branch.trim().to_owned(),
        }),
        _ => Err(format!(
            "{} has a detached HEAD: a run lands on the branch checked out where `todo run` \
             starts, and there is none; check out the branch runs land on",
            worktree.display()
        )),
    }
}

/// The real path of the worktree `dir` is in, whatever it has checked out:
/// where a start without a caller's directory (a chained run, the queue, an
/// item coordinator) later reads TODO.md, the checks and the branch, as
/// `todo run` started there would.
pub(super) fn worktree_of(dir: &Path) -> Result<PathBuf, String> {
    let top = git_with(dir, &["rev-parse", "--show-toplevel"], None)
        .map_err(|why| format!("{} is not in a git worktree: {why}", dir.display()))?;
    let top = PathBuf::from(top.trim());
    top.canonicalize()
        .map_err(|error| format!("cannot resolve the worktree {}: {error}", top.display()))
}

/// The branch checked out in the worktree of `dir` ([`checked_out`]).
pub(super) fn resolve(dir: &Path) -> Result<String, String> {
    checked_out(dir).map(|landing| landing.branch)
}

/// The commit of the stored target `branch`, or the error naming it when
/// it no longer resolves (deleted or renamed during the run).
pub(super) fn commit_of(repo: &Path, branch: &str) -> Result<String, String> {
    git_with(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}^{{commit}}"),
        ],
        None,
    )
    .map(|commit| commit.trim().to_owned())
    .ok()
    .filter(|commit| !commit.is_empty())
    .ok_or_else(|| {
        format!(
            "the run's target branch {branch} no longer exists in {} (deleted or renamed?); \
             the run does not retarget to another branch",
            repo.display()
        )
    })
}

/// Right before the cherry-pick: `worktree` still has `branch` checked
/// out, has no uncommitted change or cherry-pick in progress, and the
/// branch still contains the run's `base`. Otherwise the error names what
/// differs.
pub(super) fn check_landing(worktree: &Path, branch: &str, base: &str) -> Result<(), String> {
    let on = git_with(
        worktree,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        None,
    )
    .map_err(|why| {
        if worktree.is_dir() {
            format!(
                "the landing worktree {} is on a detached HEAD, not {branch} ({why})",
                worktree.display()
            )
        } else {
            format!(
                "the landing worktree {} no longer exists; nothing was picked",
                worktree.display()
            )
        }
    })?;
    let on = on.trim();
    if on != branch {
        return Err(format!(
            "the landing worktree {} is on {on}, not {branch}",
            worktree.display()
        ));
    }
    if git_with(
        worktree,
        &["rev-parse", "--quiet", "--verify", "CHERRY_PICK_HEAD"],
        None,
    )
    .is_ok()
    {
        return Err(format!(
            "the landing worktree {} has a cherry-pick in progress",
            worktree.display()
        ));
    }
    let dirty = git_with(
        worktree,
        &["status", "--porcelain", "--untracked-files=no"],
        None,
    )?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "the landing worktree {} has uncommitted changes:\n{}",
            worktree.display(),
            dirty.trim_end()
        ));
    }
    if git_with(
        worktree,
        &["merge-base", "--is-ancestor", base, "HEAD"],
        None,
    )
    .is_err()
    {
        return Err(format!(
            "{branch} in {} no longer contains the run's base {base} (rewritten?)",
            worktree.display()
        ));
    }
    Ok(())
}

/// Where git pushes `branch` from `worktree`: the remote from
/// `branch.<B>.pushRemote`, else `remote.pushDefault`, else
/// `branch.<B>.remote` (else git's default `origin`); the destination ref
/// from `push.default` (git's default `simple`) and, for `upstream`,
/// `branch.<B>.merge`. Read from git's configuration key by key, and only
/// when `git rev-parse B@{push}` names a single destination: otherwise (an
/// upstream of another name under `simple`, no upstream, `nothing`) the
/// run does not push, and [`TodoRunPush::skipped`] says why.
pub(super) fn push_of(worktree: &Path, branch: &str) -> TodoRunPush {
    let skipped = |why: String| TodoRunPush {
        remote: None,
        remote_from: None,
        dst_ref: None,
        dst_from: None,
        skipped: Some(why),
    };
    let at_push = format!("{branch}@{{push}}");
    let named = match git_with(
        worktree,
        &["rev-parse", "--symbolic-full-name", &at_push],
        None,
    ) {
        Ok(named) if !named.trim().is_empty() => named.trim().to_owned(),
        Ok(_) => {
            return skipped(format!(
                "git names no push destination for {branch} (`git rev-parse {at_push}` printed \
                 nothing)"
            ))
        }
        Err(why) => {
            return skipped(format!(
                "git names no single push destination for {branch} ({why}); with \
                 `push.default simple` the branch's upstream must be the branch it pushes to"
            ))
        }
    };
    let get = |key: &str| {
        git_with(worktree, &["config", "--get", key], None)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let push_remote = format!("branch.{branch}.pushRemote");
    let branch_remote = format!("branch.{branch}.remote");
    let (remote, remote_from) = [push_remote.as_str(), "remote.pushDefault", &branch_remote]
        .into_iter()
        .find_map(|key| get(key).map(|remote| (remote, key.to_owned())))
        .unwrap_or_else(|| ("origin".to_owned(), "git's default remote".to_owned()));
    if remote == "." {
        return skipped(format!(
            "{remote_from} sends {branch} to the local repository (`.`), not a remote"
        ));
    }
    if get(&format!("remote.{remote}.push")).is_some() {
        return skipped(format!(
            "remote.{remote}.push maps what is pushed to {remote}; herdr does not apply push \
             refspecs, so it does not push"
        ));
    }
    let mode = get("push.default");
    let mode_from = match &mode {
        Some(mode) => format!("push.default {mode}"),
        None => "push.default simple (git's default)".to_owned(),
    };
    let merge = format!("branch.{branch}.merge");
    let (dst_ref, dst_from) = match mode.as_deref().unwrap_or("simple") {
        "simple" | "current" | "matching" => (format!("refs/heads/{branch}"), mode_from),
        "upstream" | "tracking" => match get(&merge) {
            Some(dst) => (dst, format!("{mode_from} and {merge}")),
            None => return skipped(format!("{mode_from} needs {merge}, which is not set")),
        },
        other => {
            return skipped(format!(
                "push.default {other} names no single destination for {branch}"
            ))
        }
    };
    // What git named must be the tracking ref of what the configuration
    // names, so herdr pushes where git would.
    let Some(short) = dst_ref.strip_prefix("refs/heads/") else {
        return skipped(format!(
            "{dst_from} names {dst_ref}, not a branch on {remote}"
        ));
    };
    let tracking = format!("refs/remotes/{remote}/{short}");
    if named != tracking {
        return skipped(format!(
            "git names {named} for {at_push}, but the configuration names {remote}'s {dst_ref} \
             (tracked as {tracking}); herdr does not push where the two differ"
        ));
    }
    TodoRunPush {
        remote: Some(remote),
        remote_from: Some(remote_from),
        dst_ref: Some(dst_ref),
        dst_from: Some(dst_from),
        skipped: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

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

    /// A new directory for the test `name`, with a repository `repo` in it
    /// on `branch` with one commit.
    fn repo_on(name: &str, branch: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "herdr-target-branch-{name}-{}-{}",
            std::process::id(),
            super::super::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_in(&repo, &["init", "-q", "-b", branch]);
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-q", "-m", "init"]);
        (root, repo.canonicalize().unwrap())
    }

    /// A bare remote `name` beside `repo`.
    fn with_remote(root: &Path, repo: &Path, name: &str) -> PathBuf {
        let remote = root.join(format!("{name}.git"));
        git_in(root, &["init", "-q", "--bare", &format!("{name}.git")]);
        git_in(
            repo,
            &["remote", "add", name, &remote.display().to_string()],
        );
        remote
    }

    #[test]
    fn the_checked_out_branch_is_the_target_and_detached_refuses() {
        let (root, repo) = repo_on("checked-out", "main");
        std::fs::create_dir_all(repo.join("sub")).unwrap();
        let landing = checked_out(&repo.join("sub")).unwrap();
        assert_eq!(landing.worktree, repo);
        assert_eq!(landing.branch, "main");
        // With remotes, still the checkout's branch: nothing is asked of
        // origin, and no remote has to be called origin.
        with_remote(&root, &repo, "upstream");
        git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        assert_eq!(resolve(&repo).unwrap(), "feature");
        git_in(&repo, &["checkout", "-q", "--detach"]);
        let refused = checked_out(&repo).unwrap_err();
        assert!(refused.contains("detached HEAD"), "{refused}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A linked worktree on another branch is its own landing.
    #[test]
    fn a_linked_worktree_lands_on_its_own_branch() {
        let (root, repo) = repo_on("linked", "main");
        let linked = root.join("linked");
        git_in(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "bootstrap",
                &linked.display().to_string(),
            ],
        );
        let landing = checked_out(&linked).unwrap();
        assert_eq!(landing.worktree, linked.canonicalize().unwrap());
        assert_eq!(landing.branch, "bootstrap");
        assert_eq!(resolve(&repo).unwrap(), "main");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stored_branch_that_no_longer_exists_is_named() {
        let (root, repo) = repo_on("stored-gone", "main");
        let head = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
        assert_eq!(commit_of(&repo, "main").unwrap(), head);
        git_in(&repo, &["branch", "-m", "main", "trunk"]);
        let gone = commit_of(&repo, "main").unwrap_err();
        assert!(
            gone.contains("target branch main no longer exists"),
            "{gone}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `master` tracking `origin/master` pushes to origin's `master`.
    #[test]
    fn a_branch_tracking_its_namesake_pushes_there() {
        let (root, repo) = repo_on("push-namesake", "master");
        with_remote(&root, &repo, "origin");
        git_in(&repo, &["push", "-q", "-u", "origin", "master"]);
        let push = push_of(&repo, "master");
        assert_eq!(push.skipped, None, "{push:?}");
        assert_eq!(push.remote.as_deref(), Some("origin"));
        assert_eq!(push.remote_from.as_deref(), Some("branch.master.remote"));
        assert_eq!(push.dst_ref.as_deref(), Some("refs/heads/master"));
        assert!(
            push.dst_from.as_deref().unwrap().contains("simple"),
            "{push:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fork branch whose upstream is another remote's `main`: with only
    /// `pushRemote`, `simple` names no single destination; with its
    /// upstream on the fork too, it pushes to the fork's branch.
    #[test]
    fn a_fork_branch_pushes_to_the_fork_only_once_git_names_it() {
        let (root, repo) = repo_on("push-fork", "main");
        with_remote(&root, &repo, "origin");
        with_remote(&root, &repo, "fork");
        git_in(&repo, &["push", "-q", "origin", "main"]);
        git_in(&repo, &["checkout", "-q", "-b", "bootstrap"]);
        git_in(&repo, &["push", "-q", "fork", "bootstrap"]);
        git_in(&repo, &["fetch", "-q", "origin"]);
        git_in(&repo, &["branch", "-q", "-u", "origin/main", "bootstrap"]);
        // No pushRemote: simple wants the upstream's name, which differs.
        let push = push_of(&repo, "bootstrap");
        assert!(
            push.skipped
                .as_deref()
                .unwrap()
                .contains("no single push destination"),
            "{push:?}"
        );
        assert_eq!(push.remote, None);
        // pushRemote alone: still no single destination under simple.
        git_in(&repo, &["config", "branch.bootstrap.pushRemote", "fork"]);
        let push = push_of(&repo, "bootstrap");
        assert!(push.skipped.is_some(), "{push:?}");
        // The upstream on the fork too: git names fork/bootstrap.
        git_in(
            &repo,
            &["branch", "-q", "-u", "fork/bootstrap", "bootstrap"],
        );
        let push = push_of(&repo, "bootstrap");
        assert_eq!(push.skipped, None, "{push:?}");
        assert_eq!(push.remote.as_deref(), Some("fork"));
        assert_eq!(
            push.remote_from.as_deref(),
            Some("branch.bootstrap.pushRemote")
        );
        assert_eq!(push.dst_ref.as_deref(), Some("refs/heads/bootstrap"));
        // push.default current: pushRemote alone names fork/bootstrap.
        git_in(&repo, &["branch", "-q", "-u", "origin/main", "bootstrap"]);
        git_in(&repo, &["config", "push.default", "current"]);
        let push = push_of(&repo, "bootstrap");
        assert_eq!(push.remote.as_deref(), Some("fork"), "{push:?}");
        assert_eq!(push.dst_ref.as_deref(), Some("refs/heads/bootstrap"));
        assert_eq!(push.dst_from.as_deref(), Some("push.default current"));
        // remote.pushDefault applies when the branch names no pushRemote.
        git_in(&repo, &["config", "--unset", "branch.bootstrap.pushRemote"]);
        git_in(&repo, &["config", "remote.pushDefault", "fork"]);
        let push = push_of(&repo, "bootstrap");
        assert_eq!(push.remote_from.as_deref(), Some("remote.pushDefault"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `push.default upstream` pushes to the upstream's ref; without an
    /// upstream nothing is pushed.
    #[test]
    fn upstream_mode_pushes_to_the_merge_ref_and_no_upstream_skips() {
        let (root, repo) = repo_on("push-upstream", "work");
        with_remote(&root, &repo, "origin");
        git_in(&repo, &["push", "-q", "origin", "work:trunk"]);
        git_in(&repo, &["fetch", "-q", "origin"]);
        let push = push_of(&repo, "work");
        assert!(push.skipped.is_some(), "{push:?}");
        git_in(&repo, &["branch", "-q", "-u", "origin/trunk", "work"]);
        git_in(&repo, &["config", "push.default", "upstream"]);
        let push = push_of(&repo, "work");
        assert_eq!(
            push.dst_ref.as_deref(),
            Some("refs/heads/trunk"),
            "{push:?}"
        );
        assert_eq!(
            push.dst_from.as_deref(),
            Some("push.default upstream and branch.work.merge")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_landing_check_names_a_switch_a_change_and_a_rewrite() {
        let (root, repo) = repo_on("landing-check", "main");
        let base = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
        check_landing(&repo, "main", &base).unwrap();
        std::fs::write(repo.join("a.txt"), "changed\n").unwrap();
        let dirty = check_landing(&repo, "main", &base).unwrap_err();
        assert!(dirty.contains("uncommitted changes"), "{dirty}");
        git_in(&repo, &["checkout", "-q", "--", "a.txt"]);
        git_in(&repo, &["checkout", "-q", "-b", "other"]);
        let switched = check_landing(&repo, "main", &base).unwrap_err();
        assert!(switched.contains("is on other, not main"), "{switched}");
        git_in(&repo, &["checkout", "-q", "main"]);
        git_in(&repo, &["commit", "-q", "--amend", "-m", "rewritten"]);
        let rewritten = check_landing(&repo, "main", &base).unwrap_err();
        assert!(rewritten.contains("no longer contains"), "{rewritten}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Non-test code of `src/workers` names no branch literally: every site
    /// asks this module, and [`LEGACY_TARGET_BRANCH`] is the one `"master"`.
    #[test]
    fn workers_code_has_no_master_literal_but_the_legacy_fallback() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/workers");
        let literal = format!("\"{}\"", LEGACY_TARGET_BRANCH);
        let mut found = Vec::new();
        let mut dirs = vec![root];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (number, line) in non_test_lines(&text) {
                    if line.contains(&literal) {
                        found.push(format!("{}:{number}: {}", path.display(), line.trim()));
                    }
                }
            }
        }
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(
            found[0].contains("target_branch.rs")
                && found[0].contains("const LEGACY_TARGET_BRANCH"),
            "{found:#?}"
        );
    }

    /// The lines of `text` (numbered from 1) before its inline test module
    /// (`#[cfg(test)] mod tests { ... }`, or a `cfg` with more than `test`).
    fn non_test_lines(text: &str) -> Vec<(usize, &str)> {
        let lines: Vec<&str> = text.lines().collect();
        let end = (1..lines.len())
            .find(|&at| {
                let line = lines[at].trim();
                let gate = lines[at - 1].trim();
                line.ends_with("mod tests {") && gate.starts_with("#[cfg(") && gate.contains("test")
            })
            .map_or(lines.len(), |at| at - 1);
        lines[..end]
            .iter()
            .enumerate()
            .map(|(at, line)| (at + 1, *line))
            .collect()
    }
}
