//! A run's `prepare`: the repository-declared operation the driver runs
//! before the run's first worker starts, under the user's grant
//! ([`super::super::capabilities`]). It runs in a checkout of the run's base
//! commit exported from git's objects (never the worker's tree), as a job
//! confined by the platform ([`crate::platform::confined_command`]): writes
//! only under its granted paths, the export and its own scratch directory
//! (both removed after it), the granted
//! environment variables only, and network only through a local egress
//! proxy that tunnels to the granted hosts (HTTPS `CONNECT` to port 443)
//! and refuses every other peer. The job's own process group is killed once
//! it exits, so no descendant outlives it. Its result (done or failed, exit
//! status, refused hosts, a bounded output tail) is a run event, and its
//! whole output stays in a job-scoped log next to the worker store. A failed
//! prepare blocks the run; a run cut off mid-prepare runs it again (it is
//! idempotent, like fetching into a cache).

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::json;
use tracing::warn;

use super::super::capabilities::{check_enforceable, confined_job, CapabilityKind};
use super::super::verify::tail;
use super::{lock, Run, RUN_ENV};
use crate::platform::Confinement;
use crate::workers::WorkerSupervisor;

/// The bytes a proxy reads of a request's head before it refuses it.
const HEAD_MAX: usize = 8 * 1024;
/// The only port the proxy tunnels to: HTTPS.
const TUNNEL_PORT: u16 = 443;

/// A local proxy that tunnels `CONNECT` requests to the allowed hosts only.
pub(super) struct EgressProxy {
    port: u16,
    stop: Arc<AtomicBool>,
    denied: Arc<Mutex<BTreeSet<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl EgressProxy {
    pub(super) fn start(hosts: &[String]) -> io::Result<Self> {
        Self::start_with(hosts.to_vec(), TUNNEL_PORT)
    }

    fn start_with(hosts: Vec<String>, tunnel_port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let denied = Arc::new(Mutex::new(BTreeSet::new()));
        let allowed: Arc<BTreeSet<String>> = Arc::new(hosts.into_iter().collect());
        let thread = {
            let stop = Arc::clone(&stop);
            let denied = Arc::clone(&denied);
            crate::thread_spawn::spawn_named("herdr-egress-proxy", move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let allowed = Arc::clone(&allowed);
                    let denied = Arc::clone(&denied);
                    if let Err(error) =
                        crate::thread_spawn::spawn_named("herdr-egress-tunnel", move || {
                            tunnel(stream, &allowed, &denied, tunnel_port)
                        })
                    {
                        warn!(%error, "cannot start an egress proxy connection");
                    }
                }
            })?
        };
        Ok(Self {
            port,
            stop,
            denied,
            thread: Some(thread),
        })
    }

    pub(super) fn port(&self) -> u16 {
        self.port
    }

    /// The hosts the proxy refused so far.
    pub(super) fn denied(&self) -> Vec<String> {
        lock(&self.denied).iter().cloned().collect()
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop, which sees the stop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Answers one client: a tunnel to an allowed host, else 403.
fn tunnel(
    mut client: TcpStream,
    allowed: &BTreeSet<String>,
    denied: &Mutex<BTreeSet<String>>,
    tunnel_port: u16,
) {
    let Some((target, rest)) = read_head(&mut client) else {
        return;
    };
    let refuse = |client: &mut TcpStream| {
        let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
    };
    let Some((host, port)) = target else {
        refuse(&mut client);
        return;
    };
    if port != tunnel_port || !allowed.contains(&host) {
        lock(denied).insert(format!("{host}:{port}"));
        refuse(&mut client);
        return;
    }
    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)) else {
        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    if client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .is_err()
        || upstream.write_all(&rest).is_err()
    {
        return;
    }
    let (Ok(mut client_read), Ok(mut upstream_write)) = (client.try_clone(), upstream.try_clone())
    else {
        return;
    };
    let up = crate::thread_spawn::spawn_named("herdr-egress-up", move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    if let Ok(up) = up {
        let _ = up.join();
    }
}

/// The request's `CONNECT` target (lowercase host, port), none for any
/// other request, and the bytes read past its head.
#[allow(clippy::type_complexity)] // one private helper's pair of results
fn read_head(client: &mut TcpStream) -> Option<(Option<(String, u16)>, Vec<u8>)> {
    let mut head = Vec::new();
    let mut buffer = [0_u8; 1024];
    let end = loop {
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if head.len() > HEAD_MAX {
            return Some((None, Vec::new()));
        }
        let read = client.read(&mut buffer).ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&buffer[..read]);
    };
    let rest = head[end..].to_vec();
    let text = String::from_utf8_lossy(&head[..end]);
    Some((
        connect_target(text.lines().next().unwrap_or_default()),
        rest,
    ))
}

fn connect_target(line: &str) -> Option<(String, u16)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "CONNECT" {
        return None;
    }
    let (host, port) = parts.next()?.rsplit_once(':')?;
    Some((host.to_ascii_lowercase(), port.parse().ok()?))
}

/// A granted path as the platform resolves it: `~/` from the home
/// directory, the longest existing ancestor canonical.
fn resolved(path: &str, home: Option<&Path>) -> Result<PathBuf, String> {
    let path = match path.strip_prefix("~/") {
        Some(rest) => home
            .ok_or_else(|| format!("`{path}` needs a home directory, and HOME is not set"))?
            .join(rest),
        None => PathBuf::from(path),
    };
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return Ok(missing.iter().rev().fold(real, |acc, part| acc.join(part)));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return Ok(path),
        }
    }
}

impl WorkerSupervisor {
    /// Runs the run's granted prepare once, before its first worker; a run
    /// without one, or prepared already, goes on.
    pub(super) fn step_prepare(&self, run: &mut Run) -> Result<(), String> {
        let Some(plan) = run.finish.prepare.clone() else {
            return Ok(());
        };
        if run.finish.prepared {
            return Ok(());
        }
        let capabilities = plan.capabilities()?;
        check_enforceable(confined_job(), "[prepare]", &capabilities)
            .map_err(|error| format!("prepare unsupported: {error}"))?;
        let base = run.info.base.clone().ok_or("the run has no base")?;
        let jobs = self.shared.dir.join("prepare");
        let dir = jobs.join(&run.info.run_id);
        let log_path = jobs.join(format!("{}.log", run.info.run_id));
        let _ = std::fs::remove_dir_all(&dir);
        let tree = dir.join("tree");
        let scratch = dir.join("tmp");
        for path in [&tree, &scratch] {
            std::fs::create_dir_all(path)
                .map_err(|error| format!("prepare: cannot create {}: {error}", path.display()))?;
        }
        self.run_step(
            run,
            json!({
                "type": "run_prepare_started",
                "grant": plan,
                "log": log_path,
            }),
        )?;
        export_tree(Path::new(&run.info.repo), &base, &tree)?;

        let caller_env = lock(&RUN_ENV).get(&run.info.run_id).cloned();
        let source: HashMap<String, String> =
            caller_env.unwrap_or_else(|| std::env::vars().collect());
        let home = source.get("HOME").map(PathBuf::from);
        // Its scratch directory first (its TMPDIR), then the export it runs
        // in, both removed after it; then the granted paths.
        let mut write_paths = Vec::new();
        for path in [&scratch, &tree] {
            write_paths.push(
                path.canonicalize()
                    .map_err(|error| format!("prepare: {error}"))?,
            );
        }
        for path in capabilities
            .scope(CapabilityKind::FsWrite)
            .unwrap_or_default()
        {
            write_paths.push(resolved(path, home.as_deref())?);
        }
        let mut env: Vec<(String, String)> = capabilities
            .scope(CapabilityKind::Env)
            .unwrap_or_default()
            .iter()
            .filter_map(|name| source.get(name).map(|value| (name.clone(), value.clone())))
            .collect();
        env.push((
            "TMPDIR".into(),
            write_paths[0].to_string_lossy().into_owned(),
        ));
        let proxy = match capabilities.scope(CapabilityKind::NetEgress) {
            Some(hosts) => Some(
                EgressProxy::start(hosts)
                    .map_err(|error| format!("prepare: cannot start the egress proxy: {error}"))?,
            ),
            None => None,
        };
        if let Some(proxy) = &proxy {
            let url = format!("http://127.0.0.1:{}", proxy.port());
            for name in [
                "HTTPS_PROXY",
                "https_proxy",
                "HTTP_PROXY",
                "http_proxy",
                "ALL_PROXY",
            ] {
                env.push((name.into(), url.clone()));
            }
        }
        let mut command = crate::platform::confined_command(&Confinement {
            argv: &plan.argv,
            write_paths: &write_paths,
            proxy_port: proxy.as_ref().map(EgressProxy::port),
        })
        .ok_or("prepare unsupported: this platform cannot confine a job")?;
        let log = File::create(&log_path)
            .map_err(|error| format!("prepare: cannot create {}: {error}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .map_err(|error| format!("prepare: {error}"))?;
        command
            .current_dir(&tree)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        crate::platform::configure_worker_process(&mut command);
        let outcome = run_job(command);
        let denied = proxy.as_ref().map(EgressProxy::denied).unwrap_or_default();
        drop(proxy);
        let output = std::fs::read_to_string(&log_path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let (status, exit) = match &outcome {
            Ok(Some(0)) => ("done", Some(0)),
            Ok(code) => ("failed", *code),
            Err(_) => ("failed", None),
        };
        self.run_step(
            run,
            json!({
                "type": "run_prepared",
                "status": status,
                "exit": exit,
                "error": outcome.as_ref().err(),
                "denied_hosts": denied,
                "output": tail(&output),
                "log": log_path,
            }),
        )?;
        if status != "done" {
            return Err(format!(
                "prepare failed ({}){}; its log is {}:\n{}",
                match (&outcome, exit) {
                    (Err(error), _) => error.clone(),
                    (_, Some(code)) => format!("exit {code}"),
                    (_, None) => "killed by a signal".into(),
                },
                if denied.is_empty() {
                    String::new()
                } else {
                    format!("; the egress proxy refused {}", denied.join(", "))
                },
                log_path.display(),
                tail(&output)
            ));
        }
        run.finish.prepared = true;
        self.run_step(run, json!({"type": "run_prepare_recorded"}))?;
        Ok(())
    }
}

/// Exports `commit`'s tree into `into` from git's objects.
fn export_tree(repo: &Path, commit: &str, into: &Path) -> Result<(), String> {
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", commit])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("prepare: cannot run git archive: {error}"))?;
    let stdout = archive
        .stdout
        .take()
        .ok_or("prepare: git archive has no output")?;
    let untar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(into)
        .stdin(stdout)
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("prepare: cannot run tar: {error}"))?;
    let archived = archive
        .wait_with_output()
        .map_err(|error| format!("prepare: git archive: {error}"))?;
    if !archived.status.success() {
        return Err(format!(
            "prepare: git archive {commit} failed: {}",
            String::from_utf8_lossy(&archived.stderr).trim()
        ));
    }
    if !untar.status.success() {
        return Err(format!(
            "prepare: extracting {commit} failed: {}",
            String::from_utf8_lossy(&untar.stderr).trim()
        ));
    }
    Ok(())
}

/// Runs the job to its end, then kills what is left of its process group.
fn run_job(mut command: Command) -> Result<Option<i32>, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start the job: {error}"))?;
    let pid = child.id();
    let status = child
        .wait()
        .map_err(|error| format!("waiting for the job: {error}"));
    let _ = crate::platform::signal_process_group(pid, crate::platform::Signal::Kill);
    status.map(|status| status.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_connect_names_a_target() {
        assert_eq!(
            connect_target("CONNECT Registry.Example:443 HTTP/1.1"),
            Some(("registry.example".into(), 443))
        );
        assert_eq!(
            connect_target("GET http://registry.example/ HTTP/1.1"),
            None
        );
        assert_eq!(connect_target("CONNECT registry.example HTTP/1.1"), None);
    }

    #[test]
    fn a_granted_home_path_resolves_under_home() {
        let home = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(
            resolved("~/no-such-dir/cache", Some(&home)).unwrap(),
            home.join("no-such-dir/cache")
        );
        assert!(resolved("~/x", None).is_err());
    }

    /// Needs a local listener; inside a sandbox that forbids binding one it
    /// says so instead of failing.
    #[test]
    fn the_proxy_tunnels_only_to_allowed_hosts() {
        let upstream = match TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("skipped: cannot bind a local port here: {error}");
                return;
            }
        };
        let upstream_port = upstream.local_addr().unwrap().port();
        let echo = std::thread::spawn(move || {
            let (mut stream, _) = upstream.accept().unwrap();
            let mut buffer = [0_u8; 5];
            stream.read_exact(&mut buffer).unwrap();
            stream.write_all(&buffer).unwrap();
        });
        let proxy = EgressProxy::start_with(vec!["localhost".into()], upstream_port).unwrap();
        let ask = |target: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", proxy.port())).unwrap();
            write!(
                stream,
                "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n"
            )
            .unwrap();
            let mut reply = [0_u8; 12];
            stream.read_exact(&mut reply).unwrap();
            (String::from_utf8_lossy(&reply).into_owned(), stream)
        };
        let (reply, _) = ask(&format!("evil.example:{upstream_port}"));
        assert_eq!(reply, "HTTP/1.1 403");
        let (reply, _) = ask("localhost:22");
        assert_eq!(reply, "HTTP/1.1 403");
        let (reply, mut stream) = ask(&format!("localhost:{upstream_port}"));
        assert_eq!(reply, "HTTP/1.1 200");
        let mut rest = Vec::new();
        // The rest of the established line, up to the blank line.
        while !rest.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).unwrap();
            rest.push(byte[0]);
        }
        stream.write_all(b"hello").unwrap();
        let mut echoed = [0_u8; 5];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"hello");
        echo.join().unwrap();
        assert_eq!(
            proxy.denied(),
            [
                format!("evil.example:{upstream_port}"),
                "localhost:22".to_owned()
            ]
        );
    }
}
