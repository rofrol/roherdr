//! The branch a repository's TODO runs target: its default branch, read
//! from git, never a literal name. With an `origin` remote it is the branch
//! `git ls-remote --symref origin HEAD` names (asked live: the cached
//! `origin/HEAD` can be stale, and an old branch often still exists after a
//! rename), which must exist locally; without a remote it is the shared
//! checkout's current branch. Anything else refuses with what was asked and
//! what came back, never a guess. A run resolves it once at its preflight,
//! stores the name and each later step reads it back with [`commit_of`],
//! which stops when it no longer resolves instead of retargeting.

use std::collections::HashMap;
use std::path::Path;

use super::runs::git_with;

/// The branch of a run recorded before runs stored theirs: the only branch
/// such a run could have targeted.
pub(super) const LEGACY_TARGET_BRANCH: &str = "master";

/// The repository's default branch, `env` (the caller's environment) for
/// the question to `origin` when given.
pub(super) fn resolve(
    repo: &Path,
    env: Option<&HashMap<String, String>>,
) -> Result<String, String> {
    let remotes = git_with(repo, &["remote"], None)?;
    let remotes: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    if remotes.contains(&"origin") {
        return from_origin(repo, env);
    }
    if !remotes.is_empty() {
        return Err(format!(
            "{} has the remotes {} but no origin: its default branch is the one origin's HEAD \
             names, and herdr does not guess another",
            repo.display(),
            remotes.join(", ")
        ));
    }
    match git_with(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"], None) {
        Ok(branch) if !branch.trim().is_empty() => Ok(branch.trim().to_owned()),
        _ => Err(format!(
            "{} has no remote and a detached HEAD: without a remote the target branch is the \
             checkout's current branch, and it has none; check out the branch runs land on",
            repo.display()
        )),
    }
}

/// The branch `origin`'s `HEAD` points to, when the repository has it.
fn from_origin(repo: &Path, env: Option<&HashMap<String, String>>) -> Result<String, String> {
    const ASKED: &str = "git ls-remote --symref origin HEAD";
    let out = git_with(repo, &["ls-remote", "--symref", "origin", "HEAD"], env).map_err(|why| {
        format!(
            "cannot read the default branch of {}'s origin ({ASKED}): {why}; the target branch \
             is never guessed",
            repo.display()
        )
    })?;
    let named = out.lines().find_map(|line| {
        let (target, name) = line.strip_prefix("ref: ")?.split_once('\t')?;
        (name.trim() == "HEAD").then(|| target.trim().to_owned())
    });
    let Some(target) = named else {
        return Err(format!(
            "{ASKED} in {} named no branch (it printed {:?}); the target branch is never \
             guessed",
            repo.display(),
            out.trim()
        ));
    };
    let Some(branch) = target.strip_prefix("refs/heads/").filter(|b| !b.is_empty()) else {
        return Err(format!(
            "{ASKED} in {} named {target}, not a branch",
            repo.display()
        ));
    };
    if commit_of(repo, branch).is_err() {
        return Err(format!(
            "{ASKED} in {} named {branch}, which the repository has no local branch of; \
             fetch it and create it (`git branch {branch} origin/{branch}`)",
            repo.display()
        ));
    }
    Ok(branch.to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
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
        (root, repo)
    }

    /// A bare `origin` beside `repo` whose `HEAD` names `branch`, which is
    /// pushed to it.
    fn with_origin(root: &Path, repo: &Path, branch: &str) -> PathBuf {
        let origin = root.join("origin.git");
        git_in(root, &["init", "-q", "--bare", "-b", branch, "origin.git"]);
        git_in(
            repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        git_in(repo, &["push", "-q", "origin", branch]);
        git_in(repo, &["fetch", "-q", "origin"]);
        origin
    }

    #[test]
    fn origin_head_names_the_target_branch() {
        for branch in ["main", "master"] {
            let (root, repo) = repo_on(&format!("origin-{branch}"), branch);
            with_origin(&root, &repo, branch);
            assert_eq!(resolve(&repo, None).unwrap(), branch);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// The cached `origin/HEAD` still names `master`, which still exists,
    /// while origin's `HEAD` moved to `main`: the target is `main`.
    #[test]
    fn a_stale_cached_origin_head_is_not_read() {
        let (root, repo) = repo_on("stale-origin-head", "master");
        let origin = with_origin(&root, &repo, "master");
        git_in(&repo, &["remote", "set-head", "origin", "master"]);
        git_in(&repo, &["branch", "main"]);
        git_in(&repo, &["push", "-q", "origin", "main"]);
        git_in(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        assert_eq!(
            git_in(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD"]).trim(),
            "refs/remotes/origin/master"
        );
        assert_eq!(resolve(&repo, None).unwrap(), "main");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// With a remote, neither a detached HEAD nor a feature branch checked
    /// out changes the target: it is origin's.
    #[test]
    fn with_a_remote_the_checkouts_branch_is_not_the_target() {
        let (root, repo) = repo_on("checkout-not-target", "main");
        with_origin(&root, &repo, "main");
        git_in(&repo, &["checkout", "-q", "--detach"]);
        assert_eq!(resolve(&repo, None).unwrap(), "main");
        git_in(&repo, &["checkout", "-q", "-b", "feature"]);
        assert_eq!(resolve(&repo, None).unwrap(), "main");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn without_a_remote_the_current_branch_is_the_target_and_detached_refuses() {
        let (root, repo) = repo_on("no-remote", "main");
        assert_eq!(resolve(&repo, None).unwrap(), "main");
        git_in(&repo, &["checkout", "-q", "--detach"]);
        let refused = resolve(&repo, None).unwrap_err();
        assert!(refused.contains("detached HEAD"), "{refused}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unreachable_or_unhelpful_origin_refuses() {
        let (root, repo) = repo_on("unreachable-origin", "main");
        let missing = root.join("missing.git");
        git_in(
            &repo,
            &["remote", "add", "origin", &missing.display().to_string()],
        );
        let refused = resolve(&repo, None).unwrap_err();
        assert!(
            refused.contains("git ls-remote --symref origin HEAD")
                && refused.contains("never guessed"),
            "{refused}"
        );
        // origin's HEAD names a branch the repository has no local branch of.
        git_in(&repo, &["remote", "remove", "origin"]);
        let origin = with_origin(&root, &repo, "main");
        git_in(&repo, &["push", "-q", "origin", "main:trunk"]);
        git_in(&origin, &["symbolic-ref", "HEAD", "refs/heads/trunk"]);
        let refused = resolve(&repo, None).unwrap_err();
        assert!(
            refused.contains("named trunk") && refused.contains("no local branch"),
            "{refused}"
        );
        // A remote, but no origin.
        git_in(&repo, &["remote", "rename", "origin", "upstream"]);
        let refused = resolve(&repo, None).unwrap_err();
        assert!(refused.contains("but no origin"), "{refused}");
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
