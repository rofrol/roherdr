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

/// Embed the built commit's short hash and subject for the sidebar, with a `+`
/// after the hash when the sources had uncommitted changes. Builds outside a
/// git checkout (source tarballs, Nix) simply go without it.
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
    let dirty = rev == "HEAD"
        && git(
            dir,
            &[
                "status",
                "--porcelain",
                "--",
                "src",
                "build.rs",
                "Cargo.toml",
                "Cargo.lock",
            ],
        )
        .is_some();
    let mark = if dirty { "+" } else { "" };
    println!("cargo:rustc-env=HERDR_GIT_COMMIT_LINE={hash}{mark} {subject}");
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
