//! A repository whose worktree is the home directory (a dotfiles
//! repository). Herdr treats it as any other repository only where the
//! user opted in, because HOME holds the live configuration of every tool
//! the user runs, herdr and the agents among them (TODO, decided in round
//! 20261012-025700-fc04):
//!
//! - A pane whose directory is in it is a candidate, never a claim: only an
//!   explicit `herdr coordinator start --repo ~` claims it. Repositories
//!   nested under HOME resolve to themselves (the nearest `.git` wins).
//! - Such a repository often keeps its git directory outside HOME (a bare
//!   repository that a `git` wrapper or alias pairs with HOME as the work
//!   tree), so herdr's own discovery, which looks for `.git` upward, does
//!   not find it. The explicit claim names that directory (the CLI asks the
//!   user's git in HOME for it); herdr validates it and records it here,
//!   in the state directory, so a restarted or handed-off server still runs
//!   git there with `--git-dir` and `--work-tree`.
//! - Its folder slot lives under herdr's state directory, never beside HOME
//!   ([`super::slot`]); its workers never write HOME itself
//!   ([`check_worker_writes`]); and landing a run's commit into it waits for
//!   the owner's approval of the whole patch ([`super::runs`]).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{lock, WorkerError};

/// The folder under a slots directory that holds the HOME repository's
/// folder slots: HOME's own name is the user's, which says nothing.
pub(super) const SLOTS_NAME: &str = "home";

/// The registrations, in herdr's state directory.
const REGISTRY_FILE: &str = "home-repositories.json";

/// The registered git directories, by home directory (both real paths);
/// read from [`REGISTRY_FILE`] once.
static REGISTERED: Mutex<Option<BTreeMap<PathBuf, PathBuf>>> = Mutex::new(None);

#[cfg(test)]
static HOME_FOR_TEST: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Held by a test that sets the home directory: one such test at a time
/// in a test process.
#[cfg(test)]
static HOME_TESTS: Mutex<()> = Mutex::new(());

/// A test's home directory: [`HomeForTest::at`] makes a directory the home
/// directory herdr sees, with no registration; dropped, the server's
/// `HOME` again, and the registrations it made are forgotten.
#[cfg(test)]
pub(crate) struct HomeForTest {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl HomeForTest {
    pub(crate) fn set() -> Self {
        Self {
            _serial: lock(&HOME_TESTS),
        }
    }

    pub(crate) fn at(&self, home: &Path) {
        *lock(&HOME_FOR_TEST) = Some(real(home));
        *lock(&REGISTERED) = Some(BTreeMap::new());
        super::forget_repositories();
    }
}

#[cfg(test)]
impl Drop for HomeForTest {
    fn drop(&mut self) {
        *lock(&HOME_FOR_TEST) = None;
        *lock(&REGISTERED) = Some(BTreeMap::new());
        super::forget_repositories();
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    repositories: Vec<Registration>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Registration {
    home: PathBuf,
    git_dir: PathBuf,
}

fn real(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_owned())
}

/// The home directory, real: the server's `HOME`.
pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(home) = lock(&HOME_FOR_TEST).clone() {
        return Some(home);
    }
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    Some(real(Path::new(&home)))
}

/// Whether `dir` is the home directory itself.
pub(crate) fn is_home(dir: &Path) -> bool {
    home_dir().is_some_and(|home| real(dir) == home)
}

/// Whether `repo` (a repository as tenures and runs name it) is the
/// repository rooted at the home directory.
pub(crate) fn is_home_repository(repo: &str) -> bool {
    is_home(Path::new(repo))
}

/// The git directory registered for the home directory, kept outside it.
pub(crate) fn registered_git_dir() -> Option<PathBuf> {
    let home = home_dir()?;
    let mut registered = lock(&REGISTERED);
    registered.get_or_insert_with(load).get(&home).cloned()
}

/// The repository `dir` names when herdr's own discovery cannot see it:
/// the home directory, when `dir` is HOME and HOME's git directory is
/// registered.
pub(crate) fn repository_of_home_dir(dir: &Path) -> Option<PathBuf> {
    if !is_home(dir) {
        return None;
    }
    registered_git_dir()?;
    home_dir()
}

/// The repository whose common git directory is `common_dir`, when that is
/// the registered git directory of HOME: HOME, not its parent (a worktree
/// of the HOME repository, its folder slot among them).
pub(crate) fn repository_of_common_dir(common_dir: &Path) -> Option<PathBuf> {
    let git_dir = registered_git_dir()?;
    (real(common_dir) == git_dir).then(home_dir).flatten()
}

/// The global options git needs to run in `dir`: `--git-dir` and
/// `--work-tree` when `dir` is HOME and its git directory is registered,
/// else none (git finds the repository itself).
pub(crate) fn git_dir_args(dir: &Path) -> Vec<OsString> {
    if !is_home(dir) {
        return Vec::new();
    }
    match (registered_git_dir(), home_dir()) {
        (Some(git_dir), Some(home)) => vec![
            "--git-dir".into(),
            git_dir.into_os_string(),
            "--work-tree".into(),
            home.into_os_string(),
        ],
        _ => Vec::new(),
    }
}

/// Registers `git_dir` as the git directory of the repository rooted at
/// `home`, which must be the home directory. It must be a repository's
/// common directory (not a linked worktree's own), and git must see `home`
/// as the top of its work tree with it. A home directory with a `.git` of
/// its own needs no registration; one whose `.git` is another repository
/// is refused. Returns the home directory.
pub(crate) fn register(home: &Path, git_dir: &Path) -> Result<PathBuf, WorkerError> {
    let invalid = |why: String| Err(WorkerError::Invalid(why));
    let Some(real_home) = home_dir() else {
        return invalid("the server has no home directory (HOME is not set)".into());
    };
    if real(home) != real_home {
        return invalid(format!(
            "{} is not the home directory {}; only the home directory's repository is \
             registered with its git directory",
            home.display(),
            real_home.display()
        ));
    }
    if !git_dir.is_absolute() {
        return invalid(format!(
            "the git directory {} is not absolute",
            git_dir.display()
        ));
    }
    let git_dir = git_dir.canonicalize().map_err(|error| {
        WorkerError::Invalid(format!("the git directory {}: {error}", git_dir.display()))
    })?;
    let common = git_with_dir(
        &git_dir,
        None,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(|why| {
        WorkerError::Invalid(format!(
            "{} is not a git directory: {why}",
            git_dir.display()
        ))
    })?;
    if real(Path::new(common.trim())) != git_dir {
        return invalid(format!(
            "{} is the git directory of a linked worktree; name its repository's common \
             directory {}",
            git_dir.display(),
            common.trim()
        ));
    }
    let top = git_with_dir(
        &git_dir,
        Some(&real_home),
        &["rev-parse", "--show-toplevel"],
    )
    .map_err(|why| {
        WorkerError::Invalid(format!(
            "git does not see {} as the work tree of {}: {why}",
            real_home.display(),
            git_dir.display()
        ))
    })?;
    if real(Path::new(top.trim())) != real_home {
        return invalid(format!(
            "the work tree of {} is {}, not the home directory {}",
            git_dir.display(),
            top.trim(),
            real_home.display()
        ));
    }
    // `--work-tree` makes any repository's top HOME: it must also track
    // files that are there.
    let tracked = git_with_dir(&git_dir, Some(&real_home), &["ls-files"]).unwrap_or_default();
    if !tracked
        .lines()
        .any(|file| std::fs::symlink_metadata(real_home.join(file)).is_ok())
    {
        return invalid(format!(
            "{} tracks no file of the home directory {}; it is not the home directory's \
             repository",
            git_dir.display(),
            real_home.display()
        ));
    }
    let own = real_home.join(".git");
    if std::fs::symlink_metadata(&own).is_ok() {
        let own_common = git_with_dir(
            &own,
            None,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )
        .ok()
        .map(|common| real(Path::new(common.trim())));
        if own_common.as_deref() == Some(git_dir.as_path()) {
            // Herdr finds `~/.git` itself.
            return Ok(real_home);
        }
        return invalid(format!(
            "the home directory has a .git of its own ({}), not {}; claim it without naming \
             another git directory",
            own.display(),
            git_dir.display()
        ));
    }
    let mut registered = lock(&REGISTERED);
    let map = registered.get_or_insert_with(load);
    if map.get(&real_home) != Some(&git_dir) {
        let mut next = map.clone();
        next.insert(real_home.clone(), git_dir.clone());
        save(&next)?;
        *map = next;
    }
    Ok(real_home)
}

/// Refuses a worker whose sandbox would let it write the live home
/// directory: its working directory or another directory it may write is
/// HOME itself or contains it, or lies in `~/.claude`; or it works in the
/// repository rooted at HOME (`repository`) outside a folder slot.
pub(crate) fn check_worker_writes(
    writable: &[&Path],
    repository: Option<&str>,
    in_slot: bool,
) -> Result<(), WorkerError> {
    let Some(home) = home_dir() else {
        return Ok(());
    };
    let agent_config = home.join(".claude");
    for dir in writable {
        let dir = real(dir);
        if home.starts_with(&dir) {
            return Err(WorkerError::Invalid(format!(
                "a worker may not write {}: it holds the live home directory {}; HOME stays \
                 read-only to workers",
                dir.display(),
                home.display()
            )));
        }
        if dir.starts_with(&agent_config) {
            return Err(WorkerError::Invalid(format!(
                "a worker may not write {}: it is in {}, the agents' live configuration, \
                 which only its owner changes",
                dir.display(),
                agent_config.display()
            )));
        }
    }
    if !in_slot && repository.is_some_and(is_home_repository) {
        return Err(WorkerError::Invalid(format!(
            "a worker of the repository rooted at the home directory {} runs only in its folder \
             slot (--folder-slot), never in the live home directory",
            home.display()
        )));
    }
    Ok(())
}

/// The default directory of the HOME repository's folder slots:
/// `worktrees` in herdr's state directory.
pub(super) fn default_slots_root() -> PathBuf {
    // Never the user's state directory in a test.
    #[cfg(test)]
    if let Some(home) = lock(&HOME_FOR_TEST).clone() {
        return home.join(".local/state/herdr/worktrees");
    }
    crate::config::state_dir().join("worktrees")
}

fn load() -> BTreeMap<PathBuf, PathBuf> {
    // Tests register in memory only, never in the user's state directory.
    if cfg!(test) {
        return BTreeMap::new();
    }
    let path = crate::config::state_dir().join(REGISTRY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "cannot read the home repositories");
            return BTreeMap::new();
        }
    };
    match serde_json::from_str::<Registry>(&text) {
        Ok(registry) => registry
            .repositories
            .into_iter()
            .map(|registration| (registration.home, registration.git_dir))
            .collect(),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "cannot parse the home repositories");
            BTreeMap::new()
        }
    }
}

/// Writes the registrations whole: a temporary file renamed over the old.
fn save(map: &BTreeMap<PathBuf, PathBuf>) -> Result<(), WorkerError> {
    if cfg!(test) {
        return Ok(());
    }
    let dir = crate::config::state_dir();
    std::fs::create_dir_all(&dir)?;
    let registry = Registry {
        repositories: map
            .iter()
            .map(|(home, git_dir)| Registration {
                home: home.clone(),
                git_dir: git_dir.clone(),
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&registry)
        .map_err(|error| WorkerError::Io(std::io::Error::other(error)))?;
    let path = dir.join(REGISTRY_FILE);
    let temp = dir.join(format!("{REGISTRY_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// `git --git-dir git_dir [--work-tree work_tree] args`, its stdout.
fn git_with_dir(git_dir: &Path, work_tree: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("--git-dir").arg(git_dir);
    if let Some(work_tree) = work_tree {
        command
            .arg("--work-tree")
            .arg(work_tree)
            .arg("-C")
            .arg(work_tree);
    }
    let output = command
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A home directory whose repository keeps its git directory outside
    /// it, as a dotfiles repository does: `<root>/home` with `.zshrc`
    /// committed to the bare `<root>/dotfiles.git`, and no `.git` in HOME.
    pub(crate) struct HomeLayout {
        pub(crate) root: PathBuf,
        pub(crate) home: PathBuf,
        pub(crate) git_dir: PathBuf,
    }

    impl HomeLayout {
        pub(crate) fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "herdr-home-{name}-{}-{}",
                std::process::id(),
                crate::workers::now_ms()
            ));
            let _ = std::fs::remove_dir_all(&root);
            let home = root.join("home");
            std::fs::create_dir_all(&home).unwrap();
            let git_dir = root.join("dotfiles.git");
            run(
                &root,
                &["init", "-q", "--bare", "-b", "master", "dotfiles.git"],
            );
            std::fs::write(home.join(".zshrc"), "export EDITOR=vi\n").unwrap();
            let layout = Self {
                root: real(&root),
                home: real(&home),
                git_dir: real(&git_dir),
            };
            layout.git(&["add", ".zshrc"]);
            layout.git(&["commit", "-q", "-m", "zshrc"]);
            layout
        }

        /// git in HOME as the user's wrapper runs it.
        pub(crate) fn git(&self, args: &[&str]) -> String {
            let mut all = vec![
                "--git-dir".to_owned(),
                self.git_dir.display().to_string(),
                "--work-tree".to_owned(),
                self.home.display().to_string(),
            ];
            all.extend(args.iter().map(|arg| (*arg).to_owned()));
            let all: Vec<&str> = all.iter().map(String::as_str).collect();
            run(&self.home, &all)
        }
    }

    impl Drop for HomeLayout {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(crate) fn run(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn repository(dir: &Path) -> Option<String> {
        crate::workers::repository_of_dir(&dir.display().to_string())
    }

    /// Why `herdr coordinator start` failed in the home directory: herdr
    /// looks for `.git` upward, and a dotfiles repository keeps it
    /// elsewhere, so HOME was "not in a git repository". A registered git
    /// directory makes HOME (and the repository's worktrees) its
    /// repository; a repository nested in HOME stays its own.
    #[test]
    fn the_home_directory_is_its_repository_once_its_git_directory_is_registered() {
        let guard = HomeForTest::set();
        let layout = HomeLayout::new("register");
        guard.at(&layout.home);
        assert_eq!(repository(&layout.home), None);
        assert!(git_dir_args(&layout.home).is_empty());

        assert_eq!(
            register(&layout.home, &layout.git_dir).unwrap(),
            layout.home
        );
        let home = layout.home.display().to_string();
        assert_eq!(repository(&layout.home).as_deref(), Some(home.as_str()));
        assert!(is_home_repository(&home));
        let args: Vec<String> = git_dir_args(&layout.home)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--git-dir".to_owned(),
                layout.git_dir.display().to_string(),
                "--work-tree".to_owned(),
                home.clone()
            ]
        );
        // A worktree of the repository names HOME, not the parent of its
        // git directory.
        let worktree = layout.root.join("wt");
        run(
            &layout.root,
            &[
                "--git-dir",
                &layout.git_dir.display().to_string(),
                "worktree",
                "add",
                "-q",
                "--detach",
                &worktree.display().to_string(),
                "master",
            ],
        );
        assert_eq!(repository(&worktree).as_deref(), Some(home.as_str()));
        // A repository nested in HOME is its own.
        let nested = layout.home.join("proj");
        std::fs::create_dir_all(&nested).unwrap();
        run(&nested, &["init", "-q"]);
        assert_eq!(repository(&nested), Some(nested.display().to_string()));

        // Refused: a directory that is not HOME, a linked worktree's own
        // git directory, a repository that tracks nothing of HOME.
        let refused = register(&layout.root, &layout.git_dir).unwrap_err();
        assert!(
            refused.to_string().contains("not the home directory"),
            "{refused}"
        );
        let refused = register(&layout.home, &layout.git_dir.join("worktrees/wt")).unwrap_err();
        assert!(refused.to_string().contains("linked worktree"), "{refused}");
        run(&layout.root, &["init", "-q", "--bare", "other.git"]);
        let refused = register(&layout.home, &layout.root.join("other.git")).unwrap_err();
        assert!(refused.to_string().contains("tracks no file"), "{refused}");
    }

    /// A home directory with a `.git` of its own needs no registration; one
    /// that names another git directory is refused.
    #[test]
    fn a_home_directory_with_its_own_git_needs_no_registration() {
        let guard = HomeForTest::set();
        let layout = HomeLayout::new("own-git");
        guard.at(&layout.home);
        run(&layout.home, &["init", "-q"]);
        let home = layout.home.display().to_string();
        assert_eq!(repository(&layout.home).as_deref(), Some(home.as_str()));
        let refused = register(&layout.home, &layout.git_dir).unwrap_err();
        assert!(
            refused.to_string().contains("a .git of its own"),
            "{refused}"
        );
        assert!(git_dir_args(&layout.home).is_empty());
    }

    #[test]
    fn a_worker_never_writes_the_live_home_directory() {
        let guard = HomeForTest::set();
        let layout = HomeLayout::new("writes");
        guard.at(&layout.home);
        register(&layout.home, &layout.git_dir).unwrap();
        let home = layout.home.display().to_string();
        let slot = layout.home.join(".local/state/herdr/worktrees/home/worker");
        std::fs::create_dir_all(&slot).unwrap();
        let claude = layout.home.join(".claude/skills");
        std::fs::create_dir_all(&claude).unwrap();
        let refused = |writable: &[&Path], repository: Option<&str>, in_slot: bool| {
            check_worker_writes(writable, repository, in_slot)
                .unwrap_err()
                .to_string()
        };
        // HOME itself, a directory around it, the agents' configuration.
        assert!(refused(&[&layout.home], None, false).contains("read-only to workers"));
        assert!(refused(&[&layout.root], None, false).contains("read-only to workers"));
        assert!(refused(&[&slot, &claude], None, true).contains("agents' live configuration"));
        // The repository rooted at HOME only in its folder slot.
        assert!(refused(&[&slot], Some(&home), false).contains("only in its folder slot"));
        check_worker_writes(&[&slot], Some(&home), true).unwrap();
        // Another repository nested in HOME, or one elsewhere, as before.
        check_worker_writes(&[&layout.home.join("proj")], None, false).unwrap();
        check_worker_writes(&[&layout.root.join("elsewhere")], None, false).unwrap();
    }
}
