//! `scripts/herdr_live.sh install` against a fake `herdr` that records its
//! calls: the drain before the handoff. The fake's `worker wait-drained`
//! blocks on a FIFO until the test writes the turn's end into it, standing
//! in for the turn-end event the real wait is woken by.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// Only so that a broken script fails the test instead of hanging it.
const HANG_GUARD: Duration = Duration::from_secs(60);

const FAKE_HERDR: &str = r#"#!/usr/bin/env bash
dir="$FAKE_DIR"
echo "$*" >>"$dir/calls"
case "$*" in
  --version) echo "herdr fake" ;;
  --build-commit) echo "abc1234 fake: build" ;;
  "worker list")
    printf '{"result":{"workers":[{"worker_id":"w1","state":"%s"%s}]}}\n' \
      "$(cat "$dir/w1-state")" "$(cat "$dir/w1-extra" 2>/dev/null)" ;;
  "worker drain start"*) cat "$dir/drain.json" ;;
  "worker drain cancel") echo '{}' ;;
  "worker wait-drained")
    [[ -p "$dir/turn-end" ]] || { echo "unexpected wait" >&2; exit 9; }
    echo "waiting for the turn of w1 (working)"
    read -r state <"$dir/turn-end"
    [[ "$state" != cancelled ]] || { echo "the drain was cancelled while turns still run" >&2; exit 1; }
    echo "$state" >"$dir/w1-state"
    echo "turn ended: w1 ($state)" ;;
  "worker stop w1" | "worker wait w1 --exit") echo '{}' ;;
  "server live-handoff"*) echo "handed off" ;;
  *) echo "fake herdr: unexpected $*" >&2; exit 2 ;;
esac
"#;

struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// A checkout with the script and a candidate build, an installed
    /// build, and worker w1 in `state`; `in_turn` says whether the drain
    /// finds it in a turn.
    fn new(name: &str, state: &str, in_turn: bool) -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-install-{name}-{}-{}",
            std::process::id(),
            super::now_ms()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["repo/scripts", "repo/target/release", "bin", "home", "fake"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/herdr_live.sh");
        let copy = root.join("repo/scripts/herdr_live.sh");
        std::fs::copy(script, &copy).unwrap();
        let sandbox = Self { root };
        sandbox.write_exe("bin/herdr", FAKE_HERDR);
        sandbox.write_exe(
            "repo/target/release/herdr",
            &format!("{FAKE_HERDR}# the candidate\n"),
        );
        std::fs::write(sandbox.path("fake/w1-state"), state).unwrap();
        let workers = if in_turn {
            r#"[{"worker_id":"w1","state":"working"}]"#
        } else {
            "[]"
        };
        std::fs::write(
            sandbox.path("fake/drain.json"),
            format!(r#"{{"result":{{"drain":{{"draining":true,"in_turn":{workers}}}}}}}"#),
        )
        .unwrap();
        sandbox
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn write_exe(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn make_turn_end_fifo(&self) {
        let status = Command::new("mkfifo")
            .arg(self.path("fake/turn-end"))
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// The turn-end event: unblocks the fake's `worker wait-drained`.
    fn end_turn(&self, state: &str) {
        let mut fifo = std::fs::OpenOptions::new()
            .write(true)
            .open(self.path("fake/turn-end"))
            .unwrap();
        writeln!(fifo, "{state}").unwrap();
    }

    /// Starts `herdr_live.sh install` in its own process group, as a
    /// terminal's foreground job.
    fn install(&self) -> Install {
        let mut child = Command::new("bash")
            .arg(self.path("repo/scripts/herdr_live.sh"))
            .arg("install")
            .env("HOME", self.path("home"))
            .env("HERDR_INSTALLED", self.path("bin/herdr"))
            .env("HERDR_PANE_ID", "test")
            .env("FAKE_DIR", self.path("fake"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let lines = |stream: Box<dyn Read + Send>| {
            let (sender, receiver) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
            receiver
        };
        let stdout = lines(Box::new(child.stdout.take().unwrap()));
        let stderr = lines(Box::new(child.stderr.take().unwrap()));
        Install {
            child,
            stdout,
            stderr,
            seen: Vec::new(),
        }
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("fake/calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn called(&self, prefix: &str) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }

    fn installed_is_candidate(&self) -> bool {
        std::fs::read(self.path("bin/herdr")).unwrap()
            == std::fs::read(self.path("repo/target/release/herdr")).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Install {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    /// Every line read so far, both streams.
    seen: Vec<String>,
}

impl Install {
    /// Reads `stream`'s lines until one contains `text`.
    fn expect_line(&mut self, stdout: bool, text: &str) {
        loop {
            let receiver = if stdout { &self.stdout } else { &self.stderr };
            let line = receiver
                .recv_timeout(HANG_GUARD)
                .unwrap_or_else(|_| panic!("no line with {text:?}; seen {:?}", self.seen));
            self.seen.push(line.clone());
            if line.contains(text) {
                return;
            }
        }
    }

    fn signal_group(&self, signal: libc::c_int) {
        // SAFETY: kill(2) with the child's own process group, which it leads.
        let sent = unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) };
        assert_eq!(sent, 0);
    }

    /// Waits for the script's exit, then collects what it printed.
    fn finish(mut self) -> (ExitStatus, String) {
        let status = self.child.wait().unwrap();
        for receiver in [&self.stdout, &self.stderr] {
            while let Ok(line) = receiver.recv_timeout(HANG_GUARD) {
                self.seen.push(line);
            }
        }
        (status, self.seen.join("\n"))
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.signal_group(libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

#[test]
fn an_install_waits_for_a_running_turn_and_hands_off_at_its_end() {
    let sandbox = Sandbox::new("turn", "working", true);
    sandbox.make_turn_end_fifo();
    let mut install = sandbox.install();
    install.expect_line(true, "waiting for the turn of w1");
    let printed = install.seen.join("\n");
    assert!(printed.contains("in a turn: w1 (working)"), "{printed}");
    assert!(printed.contains("herdr worker drain cancel"), "{printed}");
    assert_eq!(sandbox.called("server live-handoff"), 0);
    assert!(!sandbox.installed_is_candidate());

    sandbox.end_turn("finished");
    let (status, printed) = install.finish();
    assert!(status.success(), "{printed}");
    assert!(printed.contains("turn ended: w1 (finished)"), "{printed}");
    let calls = sandbox.calls();
    let order: Vec<&str> = [
        "worker drain start",
        "worker wait-drained",
        "worker stop w1",
        "worker wait w1 --exit",
        "server live-handoff",
    ]
    .into_iter()
    .filter(|step| calls.iter().any(|call| call.starts_with(step)))
    .collect();
    assert_eq!(order.len(), 5, "{calls:?}");
    let position = |step: &str| calls.iter().position(|call| call.starts_with(step));
    assert!(
        order
            .windows(2)
            .all(|pair| position(pair[0]) < position(pair[1])),
        "{calls:?}"
    );
    let start = calls
        .iter()
        .find(|call| call.starts_with("worker drain start"))
        .unwrap();
    assert!(
        start.contains("--reason install by scripts/herdr_live.sh"),
        "{start}"
    );
    // The handoff ends the drain: nothing cancels it.
    assert_eq!(sandbox.called("worker drain cancel"), 0, "{calls:?}");
    assert!(sandbox.installed_is_candidate());
}

#[test]
fn an_install_without_a_turn_hands_off_at_once() {
    let sandbox = Sandbox::new("idle", "finished", false);
    let (status, printed) = sandbox.install().finish();
    assert!(status.success(), "{printed}");
    assert_eq!(sandbox.called("worker wait-drained"), 0);
    assert_eq!(sandbox.called("worker stop w1"), 1);
    assert_eq!(sandbox.called("server live-handoff"), 1);
    assert!(sandbox.installed_is_candidate());
}

#[test]
fn ctrl_c_during_the_wait_cancels_the_drain_and_installs_nothing() {
    let sandbox = Sandbox::new("interrupt", "working", true);
    sandbox.make_turn_end_fifo();
    let mut install = sandbox.install();
    install.expect_line(true, "waiting for the turn of w1");
    install.signal_group(libc::SIGINT);
    let (status, printed) = install.finish();
    assert!(!status.success(), "{printed}");
    assert!(printed.contains("nothing installed"), "{printed}");
    assert_eq!(sandbox.called("worker drain cancel"), 1);
    assert_eq!(sandbox.called("server live-handoff"), 0);
    assert!(!sandbox.installed_is_candidate());
    assert!(!sandbox.path("home/.cache/herdr/install.lock").exists());
}

#[test]
fn a_drain_cancelled_elsewhere_installs_nothing() {
    let sandbox = Sandbox::new("cancelled", "working", true);
    sandbox.make_turn_end_fifo();
    let mut install = sandbox.install();
    install.expect_line(true, "waiting for the turn of w1");
    sandbox.end_turn("cancelled");
    let (status, printed) = install.finish();
    assert_eq!(status.code(), Some(1), "{printed}");
    assert!(printed.contains("nothing installed"), "{printed}");
    assert_eq!(sandbox.called("server live-handoff"), 0);
    assert!(!sandbox.installed_is_candidate());
}

#[test]
fn a_second_install_waits_for_the_first_installs_lock() {
    let sandbox = Sandbox::new("concurrent", "working", true);
    sandbox.make_turn_end_fifo();
    let mut first = sandbox.install();
    first.expect_line(true, "waiting for the turn of w1");
    let mut second = sandbox.install();
    second.expect_line(false, "waiting for another install or rollback");
    assert_eq!(sandbox.called("worker drain start"), 1);

    sandbox.end_turn("finished");
    let (status, printed) = first.finish();
    assert!(status.success(), "{printed}");
    // The second finds the first's build installed.
    let (status, printed) = second.finish();
    assert!(status.success(), "{printed}");
    assert!(printed.contains("is already this build"), "{printed}");
    assert_eq!(sandbox.called("server live-handoff"), 1);
}

#[test]
fn an_install_keeps_a_brokered_worker_running_mid_turn() {
    // The server's drain does not count a worker the handoff keeps.
    let sandbox = Sandbox::new("brokered", "working", false);
    std::fs::write(sandbox.path("fake/w1-extra"), r#","survives_handoff":true"#).unwrap();
    let (status, printed) = sandbox.install().finish();
    assert!(status.success(), "{printed}");
    assert!(printed.contains("keeping worker w1 (working)"), "{printed}");
    assert_eq!(sandbox.called("worker wait-drained"), 0);
    assert_eq!(sandbox.called("worker stop w1"), 0);
    assert_eq!(sandbox.called("server live-handoff"), 1);
    assert!(sandbox.installed_is_candidate());
}
