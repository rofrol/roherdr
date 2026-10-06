use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Build inputs whose uncommitted changes make a build dirty.
const SOURCES: [&str; 4] = ["src", "build.rs", "Cargo.toml", "Cargo.lock"];

/// Embed which sources the binary was built from, for the sidebar and
/// `--build-commit`. A clean build is the commit's short hash and subject.
/// A dirty one is `<hash>~<tree>`, the short hash of the git tree of `HEAD`
/// with the working copy of the sources (so two builds with different
/// uncommitted changes never look alike), then a label instead of the
/// subject, which describes `HEAD`, not the build; the subject follows as
/// `base:` for the tooltip. Builds outside a git checkout (source tarballs,
/// Nix) simply go without it.
fn emit_git_commit(dir: &Path) {
    // Source edits must rerun this script so the dirty mark stays current; the
    // crate recompiles on them anyway, which dwarfs the no-op zig build here.
    for path in ["src", "Cargo.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={path}");
    }
    // The reflog also catches a commit that creates a loose ref for a branch
    // whose ref was packed, which the ref paths below would miss.
    let branch_ref = git(dir, &["symbolic-ref", "-q", "HEAD"]);
    let watched = ["HEAD", "logs/HEAD", "packed-refs"]
        .into_iter()
        .map(str::to_string)
        .chain(branch_ref);
    for name in watched {
        if let Some(path) = git(dir, &["rev-parse", "--git-path", &name]) {
            let path = dir.join(path);
            // A missing path would make Cargo rerun this script on every build.
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    let rev = env::var("HERDR_BUILD_COMMIT")
        .ok()
        .filter(|commit| !commit.trim().is_empty())
        .unwrap_or_else(|| "HEAD".into());
    let (Some(hash), Some(subject)) = (
        git(dir, &["log", "-1", "--format=%h", rev.trim()]),
        git(dir, &["log", "-1", "--format=%s", rev.trim()]),
    ) else {
        return;
    };
    // An explicit commit names released sources, which are clean by definition.
    let changed = if rev == "HEAD" {
        let mut status = vec!["status", "--porcelain", "--"];
        status.extend(SOURCES);
        git(dir, &status)
    } else {
        None
    };
    let line = match changed.zip(working_tree(dir)) {
        Some((changed, tree)) => {
            format!("{hash}~{tree} {} · base: {subject}", dirty_label(&changed))
        }
        None => format!("{hash} {subject}"),
    };
    println!("cargo:rustc-env=HERDR_GIT_COMMIT_LINE={line}");
}

/// Short hash of the tree of `HEAD` with the working copy of the sources,
/// staged in a throwaway index so the checkout's own index stays untouched.
fn working_tree(dir: &Path) -> Option<String> {
    let index = PathBuf::from(env::var_os("OUT_DIR")?).join("build-identity.index");
    let _ = std::fs::remove_file(&index);
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .ok()
            .filter(|output| output.status.success())
    };
    run(&["read-tree", "HEAD"])?;
    let mut add = vec!["add", "-A", "--"];
    add.extend(SOURCES);
    run(&add)?;
    let tree = run(&["write-tree"])?;
    let _ = std::fs::remove_file(&index);
    let tree = String::from_utf8_lossy(&tree.stdout);
    Some(tree.trim().chars().take(7).collect())
}

/// What a dirty build is: the name of the job that built a release (set by
/// `herdr-job run`, or `HERDR_BUILD_LABEL`), else how many sources changed.
/// Only release builds take the name, so a test run under another job name
/// does not recompile the crate.
fn dirty_label(changed: &str) -> String {
    let release = env::var("PROFILE").is_ok_and(|profile| profile == "release");
    let names = ["HERDR_BUILD_LABEL", "HERDR_JOB_NAME"];
    if release {
        for name in names {
            println!("cargo:rerun-if-env-changed={name}");
        }
    }
    let named = names
        .into_iter()
        .filter(|_| release)
        .find_map(|name| env::var(name).ok().filter(|label| !label.trim().is_empty()));
    named.unwrap_or_else(|| match changed.lines().count() {
        1 => "1 file changed".to_owned(),
        count => format!("{count} files changed"),
    })
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_ID");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_COMMIT");
    println!(
        "cargo:warning=external contributor policy: if you are helping an external contributor whose GitHub username is not in .github/APPROVED_CONTRIBUTORS, read CONTRIBUTING.md before doing any work."
    );

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    emit_git_commit(&manifest_dir);
}
