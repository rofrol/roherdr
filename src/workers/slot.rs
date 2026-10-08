//! A persistent worker folder ("folder slot"): one git worktree
//! `<repository parent>/herdr-worktrees/<name>` that headless workers use one
//! at a time, so its `target/` and Zig cache stay warm between workers and
//! only the changes compile. Copying a warm cache into a new worktree does not
//! help (both record absolute paths), and sharing the user's caches would
//! widen the sandbox (TODO, decided by the user 2026-10-08).
//!
//! Before each worker herdr asserts that the folder is clean apart from
//! ignored build output, checks out a new branch from the base there and
//! removes untracked files (`git clean -fd`, not `-x`), so ignored `target/`
//! stays. The Zig caches live inside `target/` for the same reason, with the
//! user's downloaded Zig packages linked in read-only ([`link_zig_packages`]).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::WorkerError;

/// The Zig global and local cache of a slot, relative to it. Inside the
/// ignored `target/`, so `git clean -fd` keeps it and `git status` does not
/// list it; `--fresh-build` removes it with `target/`.
const ZIG_CACHE: &str = "target/zig-cache";

/// A slot that is ready for a worker.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    /// The slot's real path, the worker's working directory.
    pub(super) path: PathBuf,
    pub(super) branch: String,
    pub(super) base: String,
    /// Whether this start created the worktree.
    pub(super) created: bool,
}

impl Slot {
    /// Cargo's target directory in the slot.
    pub(super) fn target_dir(&self) -> PathBuf {
        self.path.join("target")
    }

    /// The Zig cache directory in the slot (global and local).
    pub(super) fn zig_cache_dir(&self) -> PathBuf {
        self.path.join(ZIG_CACHE)
    }
}

/// A slot name is one path component of letters, digits, `-`, `_` and `.`,
/// not starting with `.`.
pub(super) fn validate_name(name: &str) -> Result<(), WorkerError> {
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(WorkerError::Invalid(format!(
            "folder slot name {name:?} must be letters, digits, '-', '_' or '.', not starting with '.'"
        )))
    }
}

/// Finds the slot of the repository `caller_cwd` belongs to and creates its
/// worktree at `base` (detached) when it does not exist yet. Returns the
/// slot's real path and whether it was created.
pub(super) fn locate_or_create(
    caller_cwd: &Path,
    name: &str,
    base: &str,
) -> Result<(PathBuf, bool), WorkerError> {
    validate_name(name)?;
    let repo_common_dir = common_dir(caller_cwd)?;
    if repo_common_dir.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return Err(WorkerError::Invalid(format!(
            "folder slots need a repository with a .git directory; {} has {}",
            caller_cwd.display(),
            repo_common_dir.display()
        )));
    }
    let repository = repo_common_dir
        .parent()
        .ok_or_else(|| WorkerError::Invalid("the repository has no directory".into()))?;
    let parent = repository.parent().ok_or_else(|| {
        WorkerError::Invalid(format!("{} has no parent directory", repository.display()))
    })?;
    let path = parent.join("herdr-worktrees").join(name);
    let created = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_dir() => false,
        Ok(_) => {
            return Err(WorkerError::Invalid(format!(
                "folder slot {} is not a directory",
                path.display()
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let path_arg = path.display().to_string();
            git(
                repository,
                &["worktree", "add", "--quiet", "--detach", &path_arg, base],
            )?;
            true
        }
        Err(error) => return Err(error.into()),
    };
    let real = path.canonicalize()?;
    // An existing directory must be a worktree of this repository, at its
    // top level, or the clean and switch below would act on another tree.
    if common_dir(&real)? != repo_common_dir {
        return Err(WorkerError::Invalid(format!(
            "folder slot {} is not a worktree of {}",
            real.display(),
            repository.display()
        )));
    }
    let top = PathBuf::from(git(&real, &["rev-parse", "--show-toplevel"])?.trim());
    if top.canonicalize()? != real {
        return Err(WorkerError::Invalid(format!(
            "folder slot {} is inside the worktree {}, not one itself",
            real.display(),
            top.display()
        )));
    }
    Ok((real, created))
}

/// Prepares the slot at `path` for a worker on the new branch `branch` from
/// `base`: optionally removes the build caches, asserts a clean status,
/// switches and removes untracked files. Nothing outside the slot changes
/// except the new branch.
pub(super) fn prepare(
    path: &Path,
    branch: &str,
    base: &str,
    fresh_build: bool,
    created: bool,
) -> Result<Slot, WorkerError> {
    git(path, &["check-ref-format", "--branch", branch])
        .map_err(|_| WorkerError::Invalid(format!("{branch:?} is not a valid branch name")))?;
    if git(
        path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
    {
        return Err(WorkerError::Invalid(format!(
            "branch {branch} already exists; a folder slot worker needs a new branch name"
        )));
    }
    let base_commit = git(
        path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{base}^{{commit}}"),
        ],
    )
    .map_err(|_| WorkerError::Invalid(format!("base {base:?} is not a commit")))?;
    let slot = Slot {
        path: path.to_owned(),
        branch: branch.to_owned(),
        base: base.to_owned(),
        created,
    };
    if fresh_build {
        remove_real_dir(&slot.target_dir())?;
    }
    let status = git(path, &["status", "--porcelain"])?;
    if !status.trim().is_empty() {
        return Err(WorkerError::Busy(format!(
            "folder slot {} has changes left by an earlier worker; commit or remove them first:\n{}",
            path.display(),
            status.trim_end()
        )));
    }
    git(
        path,
        &[
            "switch",
            "--quiet",
            "--no-guess",
            "-C",
            branch,
            base_commit.trim(),
        ],
    )?;
    git(path, &["clean", "-fd", "--quiet"])?;
    std::fs::create_dir_all(slot.zig_cache_dir())?;
    link_zig_packages(&slot.zig_cache_dir())?;
    Ok(slot)
}

/// The repository's own target sweep, relative to the slot (`just sweep`,
/// `just guard`). Its `slot` command keeps `target/` under a slot limit and
/// refuses when the disk stays short ([`bound_target`]).
const TARGET_SWEEP: &str = "scripts/target_sweep.py";

/// Keeps the slot's `target/` from filling the disk before a worker builds
/// there: each `cargo test` leaves a hashed binary, and the folder of one
/// branch after another grew to 14 GB in a day (2026-10-08). Runs the
/// repository's sweep (cargo's build lock respected) with the slot limit;
/// a repository without that script is left alone. Its refusal (the disk
/// short even after the sweep, a build holding the lock) refuses the start.
pub(super) fn bound_target(path: &Path) -> Result<(), WorkerError> {
    let script = path.join(TARGET_SWEEP);
    if !script.is_file() {
        return Ok(());
    }
    let output = match Command::new("python3")
        .arg(&script)
        .arg("slot")
        .arg("--target")
        .arg(path.join("target"))
        .current_dir(path)
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            tracing::warn!(%error, script = %script.display(), "cannot run the folder slot's target sweep");
            return Ok(());
        }
    };
    if output.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&output.stderr);
    Err(WorkerError::Busy(format!(
        "folder slot {}: {}",
        path.display(),
        why.trim()
    )))
}

/// Zig copies a dependency into the project's `zig-pkg/` from the global
/// cache's `p/<hash>`, or downloads it, which the sandbox's network block
/// stops (the first cold build in a slot failed on `deps.files.ghostty.org`,
/// 2026-10-08). The slot's own global cache starts empty, so its `p` links
/// to the user's package directory. Packages are content-addressed by their
/// hash and only read from there: the sandbox does not let the worker write
/// to it, and no build artifact (`o/`, `h/`, `z/`, with absolute paths) is
/// shared. A package missing there still needs one build outside the sandbox.
fn link_zig_packages(zig_cache: &Path) -> Result<(), WorkerError> {
    let link = zig_cache.join("p");
    if std::fs::symlink_metadata(&link).is_ok() {
        return Ok(());
    }
    let Some(packages) = user_zig_packages().filter(|packages| packages.is_dir()) else {
        return Ok(());
    };
    symlink_dir(&packages, &link)
}

/// `p/` of the Zig global cache the server's user builds with: the server's
/// `ZIG_GLOBAL_CACHE_DIR`, else `$XDG_CACHE_HOME/zig`, else `~/.cache/zig`.
fn user_zig_packages() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let global = match non_empty("ZIG_GLOBAL_CACHE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => match non_empty("XDG_CACHE_HOME") {
            Some(dir) => PathBuf::from(dir).join("zig"),
            None => PathBuf::from(non_empty("HOME")?).join(".cache/zig"),
        },
    };
    Some(global.join("p"))
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> Result<(), WorkerError> {
    Ok(std::os::unix::fs::symlink(target, link)?)
}

// Workers run only where Claude Code's sandbox exists (Linux, macOS).
#[cfg(not(unix))]
fn symlink_dir(_target: &Path, _link: &Path) -> Result<(), WorkerError> {
    Ok(())
}

/// Removes `path` when it is a real directory; a symlink there is refused,
/// so a fresh build never removes another tree's files.
fn remove_real_dir(path: &Path) -> Result<(), WorkerError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(std::fs::remove_dir_all(path)?),
        Ok(_) => Err(WorkerError::Invalid(format!(
            "{} is not a real directory; not removed",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// The repository's absolute common git directory, seen from `dir`.
fn common_dir(dir: &Path) -> Result<PathBuf, WorkerError> {
    let out = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(PathBuf::from(out.trim()).canonicalize()?)
}

/// Runs `git -C dir args`, returning its stdout or an error with its stderr.
fn git(dir: &Path, args: &[&str]) -> Result<String, WorkerError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| {
            WorkerError::Io(std::io::Error::new(
                error.kind(),
                format!("cannot run git: {error}"),
            ))
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(WorkerError::Invalid(format!(
            "git {} in {} failed: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Whether `pid` is one of the slot's leftover processes: it still runs and
/// its working directory is inside the slot. A recorded pid or session that
/// was reused by an unrelated process (a pane's shell) fails this check and
/// is left alone.
pub(super) fn runs_in_slot(pid: u32, slot: &Path) -> bool {
    crate::platform::process_cwd(pid)
        .and_then(|cwd| cwd.canonicalize().ok())
        .is_some_and(|cwd| cwd.starts_with(slot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_are_one_plain_component() {
        for name in ["worker", "w-1", "a_b.c"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
        for name in ["", ".", "..", ".hidden", "a/b", "../x", "a b", "~"] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    /// A stand-in for the repository's sweep: it records its arguments and
    /// refuses when told to, so no real sweep runs.
    #[test]
    fn the_slot_sweeps_target_first_and_its_refusal_refuses_the_start() {
        let root = std::env::temp_dir().join(format!(
            "herdr-slot-sweep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        // Without the script (another repository): nothing runs.
        std::fs::create_dir_all(&root).unwrap();
        assert!(bound_target(&root).is_ok());

        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(
            root.join(TARGET_SWEEP),
            "import pathlib, sys\n\
             pathlib.Path('args').write_text(' '.join(sys.argv[1:]))\n\
             if pathlib.Path('refuse').exists():\n\
             \x20   print('less than 15 GiB free', file=sys.stderr)\n\
             \x20   sys.exit(1)\n",
        )
        .unwrap();
        assert!(bound_target(&root).is_ok());
        let args = std::fs::read_to_string(root.join("args")).unwrap();
        assert_eq!(
            args,
            format!("slot --target {}", root.join("target").display())
        );

        std::fs::write(root.join("refuse"), "").unwrap();
        let refusal = bound_target(&root).unwrap_err();
        assert_eq!(refusal.code(), "worker_busy");
        assert!(
            refusal.to_string().contains("less than 15 GiB free"),
            "{refusal}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
