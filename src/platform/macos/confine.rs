//! A confined job on macOS: `sandbox-exec` with a generated Seatbelt
//! profile that denies every write outside the granted paths and every
//! network peer but the local egress proxy.

use std::fmt::Write as _;
use std::process::Command;

use super::super::Confinement;

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
    profile.push_str("(deny file-write*)\n(allow file-write*\n");
    for path in confinement.write_paths {
        let _ = writeln!(profile, "  (subpath {})", quoted(&path.to_string_lossy()));
    }
    profile.push_str(
        "  (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/tty\")\n  \
         (literal \"/dev/dtracehelper\") (regex #\"^/dev/fd/\"))\n",
    );
    profile
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
}
