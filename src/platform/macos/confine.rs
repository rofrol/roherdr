//! A confined job on macOS: `sandbox-exec` with a generated Seatbelt
//! profile that denies every write outside the granted paths and every
//! network peer but the local egress proxy. A headless pi worker's shell
//! commands run under such a profile too ([`sandboxed_shell_prefix`]).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use super::super::{Confinement, ShellSandbox};

pub(crate) const CONFINED_JOB_SUPPORTED: bool = true;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

pub(crate) fn confined_command(confinement: &Confinement<'_>) -> Option<Command> {
    let (program, args) = confinement.argv.split_first()?;
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(profile(confinement))
        .arg("--")
        .arg(program)
        .args(args);
    Some(command)
}

/// A string in the profile's syntax.
fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn profile(confinement: &Confinement<'_>) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny network*)\n");
    if let Some(port) = confinement.proxy_port {
        let _ = writeln!(
            profile,
            "(allow network-outbound (remote ip {}))",
            quoted(&format!("localhost:{port}"))
        );
    }
    push_writes(&mut profile, confinement.write_paths);
    profile
}

/// Denies every write but under `write_paths` and to the devices a shell
/// needs.
fn push_writes(profile: &mut String, write_paths: &[PathBuf]) {
    profile.push_str("(deny file-write*)\n(allow file-write*\n");
    for path in write_paths {
        let _ = writeln!(profile, "  (subpath {})", quoted(&path.to_string_lossy()));
    }
    profile.push_str(
        "  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\")\n  \
         (literal \"/dev/dtracehelper\") (regex #\"^/dev/fd/\"))\n",
    );
}

/// The profile of a pi worker's shell: no network, writes only under the
/// granted paths, and no read of the credentials or of environment files.
fn shell_profile(sandbox: &ShellSandbox<'_>) -> String {
    let mut profile = String::from("(version 1)\n(allow default)\n(deny network*)\n");
    push_writes(&mut profile, sandbox.write_paths);
    profile.push_str("(deny file-read*\n");
    for path in sandbox.deny_read {
        let _ = writeln!(profile, "  (subpath {})", quoted(&path.to_string_lossy()));
    }
    profile.push_str(
        "  (regex #\"/\\.env$\") (regex #\"/\\.env\\.[^/]*$\") (regex #\"/\\.envrc$\"))\n",
    );
    profile
}

/// A string as one single-quoted shell word.
fn shell_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// `sandbox-exec` with the shell profile, then `/bin/bash -c`: the command
/// follows as one shell-quoted word.
pub(crate) fn sandboxed_shell_prefix(sandbox: &ShellSandbox<'_>) -> Option<String> {
    Some(format!(
        "{SANDBOX_EXEC} -p {} /bin/bash -c ",
        shell_quoted(&shell_profile(sandbox))
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn the_profile_allows_only_the_granted_writes_and_the_proxy() {
        let argv = vec!["true".to_owned()];
        let paths = vec![PathBuf::from("/tmp/a \"b\"")];
        let text = profile(&Confinement {
            argv: &argv,
            write_paths: &paths,
            proxy_port: Some(4242),
        });
        assert!(text.contains("(deny network*)"), "{text}");
        assert!(text.contains("(remote ip \"localhost:4242\")"), "{text}");
        assert!(text.contains("(subpath \"/tmp/a \\\"b\\\"\")"), "{text}");
        let offline = profile(&Confinement {
            argv: &argv,
            write_paths: &[],
            proxy_port: None,
        });
        assert!(!offline.contains("network-outbound"), "{offline}");
    }

    /// Runs the job for real; inside another Seatbelt sandbox (an agent's)
    /// `sandbox_apply` is refused, and the test says so instead of failing.
    #[test]
    fn a_confined_job_writes_only_under_its_granted_paths() {
        let root = std::env::temp_dir().join(format!("herdr-confine-{}", std::process::id()));
        let granted = root.join("granted");
        let other = root.join("other");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let root_real = root.canonicalize().unwrap();
        let paths = vec![root_real.join("granted")];
        let run = |target: &std::path::Path| {
            let argv = vec![
                "/usr/bin/touch".to_owned(),
                target.to_string_lossy().into_owned(),
            ];
            confined_command(&Confinement {
                argv: &argv,
                write_paths: &paths,
                proxy_port: None,
            })
            .unwrap()
            .output()
            .unwrap()
        };
        let allowed = run(&root_real.join("granted/file"));
        if String::from_utf8_lossy(&allowed.stderr).contains("sandbox_apply") {
            eprintln!("skipped: sandbox-exec cannot apply a profile inside this sandbox");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(allowed.status.success(), "{allowed:?}");
        let denied = run(&root_real.join("other/file"));
        assert!(!denied.status.success(), "{denied:?}");
        assert!(!other.join("file").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_shell_profile_denies_network_other_writes_and_secret_reads() {
        let writes = vec![PathBuf::from("/w/slot")];
        let secrets = vec![PathBuf::from("/h/.ssh")];
        let text = shell_profile(&ShellSandbox {
            write_paths: &writes,
            deny_read: &secrets,
        });
        assert!(text.contains("(deny network*)"), "{text}");
        assert!(!text.contains("network-outbound"), "{text}");
        assert!(
            text.contains("(allow file-write*\n  (subpath \"/w/slot\")"),
            "{text}"
        );
        assert!(
            text.contains("(deny file-read*\n  (subpath \"/h/.ssh\")"),
            "{text}"
        );
        assert!(text.contains(r#"(regex #"/\.envrc$")"#), "{text}");
        assert_eq!(shell_quoted("it's"), r"'it'\''s'");
    }

    /// Runs commands through the prefix for real, as a pi worker's shell
    /// does; skipped inside another Seatbelt sandbox, as above.
    #[test]
    fn a_sandboxed_shell_writes_only_where_granted_and_reads_no_secret() {
        let root = std::env::temp_dir().join(format!("herdr-shell-{}", std::process::id()));
        std::fs::create_dir_all(root.join("granted")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        std::fs::write(root.join("secrets/key"), "secret").unwrap();
        std::fs::write(root.join("granted/.env"), "TOKEN=1").unwrap();
        let root = root.canonicalize().unwrap();
        let writes = vec![root.join("granted")];
        let secrets = vec![root.join("secrets")];
        let prefix = sandboxed_shell_prefix(&ShellSandbox {
            write_paths: &writes,
            deny_read: &secrets,
        })
        .unwrap();
        let run = |command: &str| {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("{prefix}{}", shell_quoted(command)))
                .output()
                .unwrap()
        };
        let allowed = run(&format!("touch '{}/granted/file'", root.display()));
        if String::from_utf8_lossy(&allowed.stderr).contains("sandbox_apply") {
            eprintln!("skipped: sandbox-exec cannot apply a profile inside this sandbox");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert!(allowed.status.success(), "{allowed:?}");
        let denied = run(&format!("touch '{}/other/file'", root.display()));
        assert!(!denied.status.success(), "{denied:?}");
        assert!(!root.join("other/file").exists());
        let secret = run(&format!("cat '{}/secrets/key'", root.display()));
        assert!(!secret.status.success(), "{secret:?}");
        let env = run(&format!("cat '{}/granted/.env'", root.display()));
        assert!(!env.status.success(), "{env:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
