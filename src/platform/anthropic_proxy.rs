//! Capped API transport for explicit credit-backed pi workers. Existing
//! subscription workers are never silently switched to paid API access.
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde_json::Value;

#[cfg(test)]
const MODEL: &str = "claude-sonnet-5-5";
const BODY_MAX: usize = 150_000;
const LINE_MAX: usize = 1_000_000;

#[derive(Debug)]
struct Refusal {
    status: u16,
    check: &'static str,
    upstream: Option<u16>,
    upstream_error: Option<(String, String)>,
    no_usage: bool,
}
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Anthropic proxy refusal: {}", self.check)
    }
}
impl std::error::Error for Refusal {}
fn deny(status: u16, check: &'static str) -> io::Error {
    io::Error::other(Refusal {
        status,
        check,
        upstream: None,
        upstream_error: None,
        no_usage: false,
    })
}
fn refused() -> io::Error {
    deny(502, "accounting unavailable")
}
fn with_upstream(error: io::Error, status: u16) -> io::Error {
    let refusal = error.get_ref().and_then(|e| e.downcast_ref::<Refusal>());
    io::Error::other(Refusal {
        status: refusal.map_or(502, |r| r.status),
        check: refusal.map_or("upstream transport failure", |r| r.check),
        upstream: Some(status),
        upstream_error: refusal.and_then(|r| r.upstream_error.clone()),
        no_usage: refusal.is_some_and(|r| r.no_usage),
    })
}

#[derive(Default)]
struct RequestContext {
    method: &'static str,
    path: &'static str,
    model: String,
}
struct Request {
    token: String,
    body: Vec<u8>,
    streaming: bool,
    reserve: f64,
    path: &'static str,
    beta: Option<String>,
    count: bool,
}
impl std::ops::Deref for Request {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.body
    }
}

fn report_refusal(
    error: &io::Error,
    context: &RequestContext,
    output: &mut impl Write,
    wrote: bool,
) {
    let refusal = error.get_ref().and_then(|e| e.downcast_ref::<Refusal>());
    let status = refusal.map_or(502, |r| r.status);
    let check = refusal.map_or("transport failure", |r| r.check);
    let upstream = refusal.and_then(|r| r.upstream);
    // Only selected error fields, never headers, credentials or request bodies.
    let upstream_error = refusal.and_then(|r| r.upstream_error.as_ref());
    let error_type = upstream_error.map_or("proxy_refusal", |e| e.0.as_str());
    let error_message = upstream_error.map_or(check, |e| e.1.as_str());
    tracing::warn!(method = context.method, path = context.path, model = %context.model,
        check, status, upstream_status = ?upstream, error_type, error_message,
        "Anthropic proxy refused request");
    if !wrote {
        let body =
            serde_json::json!({"type":"error", "error": {"type":error_type, "message":error_message},
            "method":context.method, "path":context.path, "model":context.model,
            "upstream_status":upstream})
            .to_string();
        let _ = write!(output, "HTTP/1.1 {status} Refused\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    }
}

/// No Debug implementation: neither the key nor run tokens are printable.
pub(crate) struct AnthropicProxy {
    key: String,
    cap: f64,
    model: String,
    ledger: Connection,
    grants: HashMap<String, (String, String)>,
    killed: bool,
}

impl AnthropicProxy {
    pub(crate) fn open(config: &crate::config::WorkersConfig, ledger: &Path) -> io::Result<Self> {
        config
            .validate_anthropic_model()
            .map_err(|_| deny(403, "configured model tariff not allowlisted"))?;
        if !config.anthropic_api_cap_usd.is_finite() || config.anthropic_api_cap_usd < 0.0 {
            return Err(refused());
        }
        let path = crate::workers::anthropic_key_path(config)?.ok_or_else(refused)?;
        if std::fs::metadata(&path)?.permissions().mode() & 0o777 != 0o600 {
            return Err(refused());
        }
        // The owning server must not dump memory after reading the key.
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: setrlimit takes a valid initialized pointer.
        if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) } != 0 {
            return Err(refused());
        }
        let mut key = crate::platform::read_secret_file(&path).map_err(|_| refused())?;
        key = key.trim().to_owned();
        if !key.starts_with("sk-ant-")
            || key.len() > 1024
            || key.bytes().any(|b| !b.is_ascii_graphic())
        {
            return Err(refused());
        }
        let db = Connection::open(ledger).map_err(|_| refused())?;
        db.execute_batch(
            "PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS anthropic_requests (
              id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
              model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
              cache_write INTEGER, cost REAL, pending INTEGER NOT NULL);",
        )
        .map_err(|_| refused())?;
        Ok(Self {
            key,
            cap: config.anthropic_api_cap_usd,
            model: config.anthropic_api_model.clone(),
            ledger: db,
            grants: HashMap::new(),
            killed: false,
        })
    }

    /// Tokens live only in the owning server and the run's authenticated client.
    pub(crate) fn register(&mut self, run: &str, item: &str) -> io::Result<String> {
        if run.is_empty() || item.is_empty() {
            return Err(refused());
        }
        self.available(0.0)?;
        let mut bytes = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        self.grants.insert(token.clone(), (run.into(), item.into()));
        Ok(token)
    }

    pub(crate) fn revoke(&mut self, token: &str) {
        self.grants.remove(token);
    }

    pub(crate) fn kill(&mut self) {
        self.killed = true;
        self.grants.clear();
    }

    pub(crate) fn available(&self, reserve: f64) -> io::Result<()> {
        let (cost, pending): (f64, i64) = self
            .ledger
            .query_row(
                "SELECT COALESCE(SUM(cost),0), COALESCE(SUM(pending),0) FROM anthropic_requests",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| refused())?;
        if self.killed {
            return Err(deny(403, "kill switch"));
        }
        if pending != 0 {
            return Err(deny(402, "pending unaccounted request"));
        }
        if cost >= self.cap || cost + reserve > self.cap {
            return Err(deny(402, "credit cap or reservation limit"));
        }
        Ok(())
    }

    /// One serialized request, with an injected transport for offline tests.
    /// A pending durable row precedes any upstream bytes. An interrupted or
    /// unaccountable response leaves it pending and denies all further spend.
    fn serve<R: Read, W: Write>(
        &mut self,
        client: R,
        output: &mut W,
        upstream: impl FnOnce(&str, &Request, &mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut context = RequestContext {
            model: self.model.clone(),
            ..Default::default()
        };
        let mut output = TrackedWriter {
            inner: output,
            wrote: false,
        };
        let result = self.serve_inner(client, &mut output, upstream, &mut context);
        if let Err(error) = &result {
            let wrote = output.wrote;
            report_refusal(error, &context, &mut output, wrote);
        }
        result
    }

    fn serve_inner<R: Read, W: Write>(
        &mut self,
        client: R,
        output: &mut W,
        upstream: impl FnOnce(&str, &Request, &mut dyn Write) -> io::Result<()>,
        context: &mut RequestContext,
    ) -> io::Result<()> {
        let request = request(client, &self.model, context).map_err(|error| {
            if error
                .get_ref()
                .and_then(|e| e.downcast_ref::<Refusal>())
                .is_some_and(|r| r.status != 502)
            {
                error
            } else {
                deny(403, "request shape or content not allowlisted")
            }
        })?;
        let (run, item) = self
            .grants
            .get(&request.token)
            .cloned()
            .ok_or_else(|| deny(401, "invalid or revoked run token"))?;
        if request.count {
            // count_tokens does not generate tokens or incur a message charge.
            // It still requires an active grant and an open spending gate.
            self.available(0.0)?;
            let mut bytes = Vec::new();
            upstream(&self.key, &request, &mut bytes)?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|_| with_upstream(deny(502, "invalid count_tokens response"), 200))?;
            if value["input_tokens"].as_u64().is_none() {
                return Err(with_upstream(
                    deny(502, "invalid count_tokens response"),
                    200,
                ));
            }
            write!(output, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len())?;
            return output.write_all(&bytes);
        }
        self.ledger
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| refused())?;
        let admission =
            (|| {
                self.available(request.reserve)?;
                self.ledger.execute(
                "INSERT INTO anthropic_requests(run,item,model,pending) VALUES (?1,?2,?3,1)",
                params![run, item, self.model]).map_err(|_| refused())?;
                self.ledger.execute_batch("COMMIT").map_err(|_| refused())
            })();
        if let Err(error) = admission {
            let _ = self.ledger.execute_batch("ROLLBACK");
            return Err(error);
        }
        let id = self.ledger.last_insert_rowid();
        let mut response = Response {
            output,
            streaming: request.streaming,
            model: &self.model,
            line: Vec::new(),
            json: Vec::new(),
            usage: Usage::default(),
            started: false,
            finished: false,
            delta: false,
            header: false,
        };
        if let Err(error) = upstream(&self.key, &request, &mut response) {
            // Only a definite HTTP rejection before any message bytes proves
            // no usage. Transport loss, malformed success and partial streams
            // remain fail-closed, even when no usage has yet been observed.
            let no_usage = error
                .get_ref()
                .and_then(|e| e.downcast_ref::<Refusal>())
                .is_some_and(|r| r.no_usage);
            if no_usage && !response.header && response.json.is_empty() && response.line.is_empty()
            {
                release_entry(&self.ledger, id)?;
            }
            return Err(error);
        }
        let usage = response.finish().map_err(|e| with_upstream(e, 200))?;
        self.ledger.execute(
            "UPDATE anthropic_requests SET input=?1,output=?2,cache_read=?3,cache_write=?4,cost=?5,pending=0 WHERE id=?6",
            params![usage.input as i64, usage.output as i64, usage.read as i64, usage.write as i64, usage.cost(), id])
            .map_err(|_| refused())?;
        Ok(())
    }

    /// Read durable accounting without loading the upstream key.
    pub(crate) fn accounting(path: &Path, run: Option<&str>) -> io::Result<(f64, i64)> {
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| refused())?;
        db.query_row(
            "SELECT COALESCE(SUM(cost),0),COALESCE(SUM(pending),0) FROM anthropic_requests WHERE (?1 IS NULL OR run=?1)",
            params![run], |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|_| refused())
    }

    /// Socket parent must already be user-only; never replace an existing path.
    /// Callers own the accept loop's lifetime. Revocation and kill are synchronous.
    pub(crate) fn listen(proxy: Arc<Mutex<Self>>, path: &Path) -> io::Result<()> {
        let listener = bind_socket(path)?;
        for client in listener.incoming() {
            let mut client = client?;
            let input = client.try_clone()?;
            {
                let mut output = TrackedWriter {
                    inner: &mut client,
                    wrote: false,
                };
                let result = proxy
                    .lock()
                    .map_err(|_| refused())?
                    .serve(input, &mut output, curl);
                let _ = result;
                // A failed stream ends by EOF, not an HTTP status appended to
                // its body. Its pending ledger entry keeps admission closed.
            }
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
        Ok(())
    }
}

/// A run-scoped HTTP listener. Dropping it revokes the grant and wakes accept;
/// there is no polling thread or timeout-driven lifetime.
pub(crate) struct LoopbackProxy {
    pub(crate) address: String,
    pub(crate) token: String,
    pub(crate) ledger: std::path::PathBuf,
    pub(crate) run: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    port: u16,
}

impl LoopbackProxy {
    pub(crate) fn start(
        proxy: AnthropicProxy,
        ledger: std::path::PathBuf,
        run: &str,
        item: &str,
    ) -> io::Result<Self> {
        Self::start_with(proxy, ledger, run, item, curl)
    }

    fn start_with(
        mut proxy: AnthropicProxy,
        ledger: std::path::PathBuf,
        run: &str,
        item: &str,
        upstream: fn(&str, &Request, &mut dyn Write) -> io::Result<()>,
    ) -> io::Result<Self> {
        let token = proxy.register(run, item)?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = crate::thread_spawn::spawn_named("herdr-anthropic-proxy", move || {
            for client in listener.incoming() {
                if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                let Ok(mut client) = client else {
                    break;
                };
                let Ok(input) = client.try_clone() else {
                    continue;
                };
                let mut output = TrackedWriter {
                    inner: &mut client,
                    wrote: false,
                };
                let _ = proxy.serve(input, &mut output, upstream);
                let _ = client.shutdown(std::net::Shutdown::Both);
            }
            proxy.kill();
        })?;
        Ok(Self {
            address: format!("http://127.0.0.1:{port}"),
            token,
            ledger,
            run: run.to_owned(),
            stop,
            thread: Some(thread),
            port,
        })
    }
}

impl Drop for LoopbackProxy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn ledger_path(config: &crate::config::WorkersConfig) -> io::Result<std::path::PathBuf> {
    let cycle = &config.anthropic_api_cycle;
    if cycle.is_empty()
        || cycle.len() > 64
        || !cycle
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(io::Error::other(
            "configure anthropic_api_cycle for the confirmed credit cycle",
        ));
    }
    Ok(crate::config::state_dir().join(format!("anthropic-credits-{cycle}.sqlite")))
}

/// Owner-only local ledger inspection; never loads the API key.
pub(crate) fn credit_status(path: &Path) -> io::Result<Value> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({"spent_usd":0.0,"pending":0,"entries":[]}));
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| refused())?;
    let mut query = db
        .prepare("SELECT id,run,item,model,cost,pending,input FROM anthropic_requests ORDER BY id")
        .map_err(|_| refused())?;
    let entries = query.query_map([], |row| {
        let pending: i64 = row.get(5)?;
        let input: Option<i64> = row.get(6)?;
        Ok(serde_json::json!({"id":row.get::<_,i64>(0)?,"run":row.get::<_,String>(1)?,
            "item":row.get::<_,String>(2)?,"model":row.get::<_,String>(3)?,
            "cost_usd":row.get::<_,Option<f64>>(4)?,
            "state":if pending != 0 {"pending"} else if input.is_none() {"released"} else {"accounted"}}))
    }).map_err(|_| refused())?.collect::<Result<Vec<_>,_>>().map_err(|_| refused())?;
    // Totals come from the same SQLite statement snapshot as the entries.
    let cost: f64 = entries
        .iter()
        .filter_map(|entry| entry["cost_usd"].as_f64())
        .sum();
    let pending = entries
        .iter()
        .filter(|entry| entry["state"] == "pending")
        .count();
    Ok(serde_json::json!({"spent_usd":cost,"pending":pending,"entries":entries}))
}

fn release_entry(db: &Connection, entry: i64) -> io::Result<()> {
    let changed = db
        .execute(
            "UPDATE anthropic_requests SET pending=0,cost=0 WHERE id=?1 AND pending=1",
            params![entry],
        )
        .map_err(|_| refused())?;
    if changed != 1 {
        return Err(io::Error::other("entry is missing or is not pending"));
    }
    Ok(())
}

/// The owner attests the stopped request did not consume tokens. This must
/// never run automatically for an ambiguous or interrupted success response.
pub(crate) fn credit_release(path: &Path, entry: i64) -> io::Result<()> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|_| refused())?;
    release_entry(&db, entry)
}

struct TrackedWriter<W> {
    inner: W,
    wrote: bool,
}
impl<W: Write> Write for TrackedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.wrote |= count != 0;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    let parent = path.parent().ok_or_else(refused)?;
    let meta = std::fs::metadata(parent)?;
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions.
    if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(refused());
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn request(input: impl Read, model: &str, context: &mut RequestContext) -> io::Result<Request> {
    let mut input = BufReader::new(input);
    let mut head = Vec::new();
    loop {
        let line = bounded_line(&mut input, 8192)
            .map_err(|_| deny(403, "invalid request header framing"))?;
        head.extend_from_slice(&line);
        if head.len() > 8192 {
            return Err(deny(403, "request headers too large"));
        }
        if line == b"\r\n" {
            break;
        }
    }
    let head =
        std::str::from_utf8(&head).map_err(|_| deny(403, "invalid request header encoding"))?;
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or_default();
    context.method = match method {
        "POST" => "POST",
        "GET" => "GET",
        "CONNECT" => "CONNECT",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "OPTIONS" => "OPTIONS",
        _ => "not allowlisted",
    };
    let path = match parts.next() {
        Some("/v1/messages") => "/v1/messages",
        Some("/v1/messages?beta=true") => "/v1/messages?beta=true",
        Some("/v1/messages/count_tokens") => "/v1/messages/count_tokens",
        Some("/v1/messages/count_tokens?beta=true") => "/v1/messages/count_tokens?beta=true",
        _ => {
            context.path = "not allowlisted";
            return Err(deny(403, "path not allowlisted"));
        }
    };
    context.path = path;
    if method != "POST" || parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(deny(403, "method or HTTP version not allowlisted"));
    }
    let count_request = path.starts_with("/v1/messages/count_tokens");
    let mut beta = None;
    let mut token = None;
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| deny(403, "malformed request header"))?;
        match name.to_ascii_lowercase().as_str() {
            "x-api-key" | "authorization" => {
                if token.is_some() {
                    return Err(deny(401, "duplicate authentication headers"));
                }
                token = Some(if name.eq_ignore_ascii_case("x-api-key") {
                    value.trim().to_owned()
                } else {
                    value
                        .trim()
                        .strip_prefix("Bearer ")
                        .ok_or_else(|| deny(401, "invalid bearer authentication"))?
                        .to_owned()
                });
            }
            "content-length" => {
                if length.is_some() {
                    return Err(deny(403, "duplicate content-length"));
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| deny(403, "invalid content-length"))?,
                );
            }
            "anthropic-beta" => {
                let value = value.trim();
                if beta.is_some()
                    || value.len() > 1024
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b',' || b == b' ')
                {
                    return Err(deny(403, "invalid anthropic-beta header"));
                }
                // pi's tools/cache beta headers, never server-side paid features.
                if !value.split(',').all(|v| {
                    matches!(
                        v.trim(),
                        "interleaved-thinking-2025-05-14"
                            | "fine-grained-tool-streaming-2025-05-14"
                            | "prompt-caching-2024-07-31"
                            | "token-counting-2024-11-01"
                    )
                }) {
                    return Err(deny(403, "anthropic-beta feature not allowlisted"));
                }
                beta = Some(value.to_owned());
            }
            "transfer-encoding" | "expect" => return Err(deny(403, "unsupported HTTP framing")),
            _ => {} // No client headers are forwarded, including x-api-key and Host.
        }
    }
    let token = token.ok_or_else(|| deny(401, "missing run token"))?;
    let length = length
        .filter(|n| *n <= BODY_MAX)
        .ok_or_else(|| deny(403, "content-length missing or too large"))?;
    let mut body = vec![0; length];
    input
        .read_exact(&mut body)
        .map_err(|_| deny(403, "incomplete request body"))?;
    let mut data: Value =
        serde_json::from_slice(&body).map_err(|_| deny(403, "invalid request JSON"))?;
    context.model = if data["model"].as_str() == Some(model) {
        model.to_owned()
    } else {
        "not allowlisted".into()
    };
    if data["model"].as_str() != Some(model) {
        return Err(deny(403, "model not allowlisted"));
    }
    let object = data
        .as_object()
        .ok_or_else(|| deny(403, "request must be an object"))?;
    for field in object.keys() {
        if ![
            "model",
            "max_tokens",
            "messages",
            "system",
            "stream",
            "tools",
            "tool_choice",
            "metadata",
            "stop_sequences",
            "temperature",
            "top_p",
            "top_k",
            "thinking",
        ]
        .contains(&field.as_str())
        {
            return Err(deny(403, "request field not allowlisted"));
        }
    }
    if let Some(thinking) = data.get("thinking") {
        if thinking["type"].as_str() != Some("disabled") {
            return Err(deny(403, "thinking must be disabled"));
        }
    }
    if let Some(system) = data.get("system") {
        text_content(system, false).map_err(|_| deny(403, "system content not allowlisted"))?;
    }
    for message in data["messages"]
        .as_array()
        .ok_or_else(|| deny(403, "messages must be an array"))?
    {
        let message = message
            .as_object()
            .ok_or_else(|| deny(403, "message must be an object"))?;
        if message
            .keys()
            .any(|key| !["role", "content"].contains(&key.as_str()))
        {
            return Err(deny(403, "message field not allowlisted"));
        }
        text_content(
            message
                .get("content")
                .ok_or_else(|| deny(403, "missing message content"))?,
            true,
        )
        .map_err(|_| deny(403, "message content not allowlisted"))?;
    }
    // Server-side tools and premium service tiers carry non-token charges.
    // Only ordinary client-defined tools are accepted in this first slice.
    if let Some(tools) = data.get("tools") {
        for tool in tools
            .as_array()
            .ok_or_else(|| deny(403, "tools must be an array"))?
        {
            if tool.get("type").is_some() || !tool["input_schema"].is_object() {
                return Err(deny(403, "server tool or schema not allowlisted"));
            }
            inline_schema(&tool["input_schema"])
                .map_err(|_| deny(403, "schema references not allowlisted"))?;
        }
    }
    let max = if count_request {
        0
    } else {
        data["max_tokens"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 64_000)
            .ok_or_else(|| deny(403, "max_tokens not allowlisted"))?
    };
    let streaming = match data.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(deny(403, "stream must be boolean")),
    };
    // The API's default `auto` tier may consume priority capacity. Force the
    // standard tier whose prices the ledger and reservation actually know.
    if !count_request {
        data["service_tier"] = Value::String("standard_only".into());
        data["thinking"] = serde_json::json!({"type": "disabled"});
    }
    // Re-encode the validated value: duplicate JSON keys cannot cause the
    // upstream to interpret a different model or token ceiling than we did.
    let body = serde_json::to_vec(&data).map_err(|_| refused())?;
    if body.len() > BODY_MAX {
        return Err(deny(403, "normalized body too large"));
    }
    // Plaintext tokens cannot exceed bytes plus protocol framing. Reserve a
    // deliberately generous 1024 tokens per message/tool/system block, not
    // just per request: many short tools must not defeat the upper bound.
    let count = |name: &str| data[name].as_array().map_or(0, Vec::len);
    let input_bound =
        body.len() + 1024 * (1 + count("messages") + count("tools") + count("system"));
    if input_bound >= 200_000 {
        return Err(deny(403, "long-context pricing not allowlisted"));
    } // Exclude long-context premium pricing.
      // 6 USD/M covers even one-hour cache writes; output is standard tier.
    let reserve = input_bound as f64 * 6.0 / 1_000_000.0 + max as f64 * 15.0 / 1_000_000.0;
    Ok(Request {
        token,
        body,
        streaming,
        reserve,
        path,
        beta,
        count: count_request,
    })
}

fn inline_schema(value: &Value) -> io::Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                // Neither remote nor local references may expand a compact
                // schema into unbounded prompt text in upstream preprocessing.
                if ["$ref", "$dynamicRef", "$recursiveRef"].contains(&key.as_str()) {
                    return Err(refused());
                }
                inline_schema(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                inline_schema(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Remote documents, images and opaque thinking replay can expand into input
/// tokens not bounded by the request's byte length. They are deliberately not
/// supported until there is an independent, safe reservation mechanism.
fn text_content(content: &Value, tools: bool) -> io::Result<()> {
    if content.is_string() {
        return Ok(());
    }
    for block in content.as_array().ok_or_else(refused)? {
        let fields: &[&str] = match block["type"].as_str() {
            Some("text") if block["text"].is_string() => &["type", "text", "cache_control"],
            Some("tool_use") if tools => &["type", "id", "name", "input", "cache_control"],
            Some("tool_result") if tools => {
                if let Some(content) = block.get("content") {
                    text_content(content, false)?;
                }
                &[
                    "type",
                    "tool_use_id",
                    "content",
                    "is_error",
                    "cache_control",
                ]
            }
            _ => return Err(refused()),
        };
        if block
            .as_object()
            .ok_or_else(refused)?
            .keys()
            .any(|key| !fields.contains(&key.as_str()))
        {
            return Err(refused());
        }
    }
    Ok(())
}

fn bounded_line(input: &mut impl BufRead, max: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let n = input.take(max as u64 + 1).read_until(b'\n', &mut line)?;
    if n == 0 || n > max || !line.ends_with(b"\n") {
        return Err(refused());
    }
    Ok(line)
}

#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
    read: u64,
    write: u64,
    hour: u64,
}
impl Usage {
    fn parse(&mut self, value: &Value, initial: bool) -> io::Result<()> {
        if value
            .get("service_tier")
            .is_some_and(|tier| tier.as_str() != Some("standard"))
        {
            return Err(deny(502, "upstream service tier not standard"));
        }
        let count = |name: &str| {
            value[name]
                .as_u64()
                .filter(|n| *n <= 1_000_000)
                .ok_or_else(|| deny(502, "upstream billed token counts missing or invalid"))
        };
        if initial {
            self.input = count("input_tokens")?;
            // A missing billed component is unavailable accounting, not zero.
            self.read = count("cache_read_input_tokens")?;
            self.write = count("cache_creation_input_tokens")?;
            self.hour = match value.pointer("/cache_creation/ephemeral_1h_input_tokens") {
                Some(hour) => hour
                    .as_u64()
                    .ok_or_else(|| deny(502, "invalid upstream cache usage"))?,
                None if self.write == 0 => 0,
                None => return Err(deny(502, "upstream cache tariff unavailable")),
            };
            if self.hour > self.write {
                return Err(deny(502, "upstream cache counts inconsistent"));
            }
        }
        self.output = count("output_tokens")?;
        Ok(())
    }
    fn cost(&self) -> f64 {
        (self.input as f64 * 3.0
            + self.output as f64 * 15.0
            + self.read as f64 * 0.3
            + (self.write - self.hour) as f64 * 3.75
            + self.hour as f64 * 6.0)
            / 1_000_000.0
    }
}

struct Response<'a, W> {
    output: &'a mut W,
    streaming: bool,
    model: &'a str,
    line: Vec<u8>,
    json: Vec<u8>,
    usage: Usage,
    started: bool,
    finished: bool,
    delta: bool,
    header: bool,
}
impl<W: Write> Response<'_, W> {
    fn finish(mut self) -> io::Result<Usage> {
        if !self.streaming {
            let value: Value = serde_json::from_slice(&self.json)
                .map_err(|_| deny(502, "invalid upstream message JSON"))?;
            if value["model"].as_str() != Some(self.model) {
                return Err(deny(502, "upstream model mismatch"));
            }
            self.usage.parse(&value["usage"], true)?;
            self.finished = true;
        }
        if !self.finished || !self.line.is_empty() || (self.streaming && !self.delta) {
            return Err(deny(502, "incomplete upstream message or usage"));
        }
        if !self.streaming {
            write!(self.output, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", self.json.len())?;
            self.output.write_all(&self.json)?;
            self.output.flush()?;
        }
        Ok(self.usage)
    }
    fn event(&mut self) -> io::Result<()> {
        if let Some(data) = self.line.strip_prefix(b"data: ") {
            let event: Value = serde_json::from_slice(data)
                .map_err(|_| deny(502, "invalid upstream stream JSON"))?;
            match event["type"].as_str() {
                Some("message_start") if !self.started => {
                    if event["message"]["model"].as_str() != Some(self.model) {
                        return Err(deny(502, "upstream stream model mismatch"));
                    }
                    self.usage.parse(&event["message"]["usage"], true)?;
                    self.started = true;
                }
                Some("message_delta") if self.started => {
                    self.usage.parse(&event["usage"], false)?;
                    self.delta = true;
                }
                Some("message_stop") if self.started => self.finished = true,
                Some("error") => {
                    let status = match event["error"]["type"].as_str() {
                        Some("rate_limit_error") => 429,
                        Some("billing_error") => 402,
                        _ => 502,
                    };
                    return Err(io::Error::other(Refusal {
                        status,
                        check: "upstream stream error",
                        upstream: Some(200),
                        upstream_error: event["error"]["type"]
                            .as_str()
                            .zip(event["error"]["message"].as_str())
                            .map(|(kind, message)| (kind.to_owned(), message.to_owned())),
                        no_usage: false,
                    }));
                }
                _ => {}
            }
        }
        Ok(())
    }
}
impl<W: Write> Write for Response<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.streaming {
            for byte in bytes {
                self.line.push(*byte);
                if self.line.len() > LINE_MAX {
                    return Err(deny(502, "upstream stream line too large"));
                }
                if *byte == b'\n' {
                    self.event()?;
                    self.line.clear();
                }
            }
        } else {
            if self.json.len() + bytes.len() > 16_000_000 {
                return Err(deny(502, "upstream message too large"));
            }
            self.json.extend_from_slice(bytes);
            // Validate complete JSON/model/usage before publishing a 200.
            return Ok(bytes.len());
        }
        if !self.header && !bytes.is_empty() {
            let content_type = if self.streaming {
                "text/event-stream"
            } else {
                "application/json"
            };
            write!(
                self.output,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n"
            )?;
            self.header = true;
        }
        self.output.write_all(bytes)?;
        self.output.flush()?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn curl_config(key: &str, request: &Request) -> io::Result<String> {
    let quote = |value: &str| {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    let data = std::str::from_utf8(&request.body).map_err(|_| refused())?;
    let beta = request
        .beta
        .as_deref()
        .map(|value| format!("header = \"anthropic-beta: {}\"\n", quote(value)))
        .unwrap_or_default();
    Ok(format!("url = \"https://api.anthropic.com{}\"\nrequest = \"POST\"\nheader = \"x-api-key: {}\"\nheader = \"anthropic-version: 2023-06-01\"\nheader = \"content-type: application/json\"\n{beta}data = \"{}\"\n", request.path, quote(key), quote(data)))
}

fn forward_upstream(input: &mut impl BufRead, output: &mut dyn Write) -> io::Result<()> {
    // Bound informational responses, headers and JSON error bodies. Expose
    // only Anthropic's documented error.type and error.message fields.
    for _ in 0..4 {
        let status_line =
            bounded_line(input, 8192).map_err(|_| deny(502, "upstream HTTP status unavailable"))?;
        let status = std::str::from_utf8(&status_line)
            .ok()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| deny(502, "invalid upstream HTTP status"))?;
        let mut size = status_line.len();
        loop {
            let line = bounded_line(input, 8192).map_err(|_| {
                with_upstream(deny(502, "upstream headers incomplete or invalid"), status)
            })?;
            size += line.len();
            if size > 32768 {
                return Err(with_upstream(
                    deny(502, "upstream headers too large"),
                    status,
                ));
            }
            if line == b"\r\n" {
                break;
            }
        }
        if (100..200).contains(&status) {
            continue;
        }
        if status != 200 {
            let local = if (400..600).contains(&status) {
                status
            } else {
                502
            };
            let mut body = Vec::new();
            // curl closes stdout after the response; a bounded read prevents
            // an untrusted error body from exhausting memory.
            let parsed = input.take(65537).read_to_end(&mut body).is_ok() && body.len() <= 65536;
            let upstream_error = if parsed {
                serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|value| {
                        Some((
                            value["error"]["type"].as_str()?.to_owned(),
                            value["error"]["message"].as_str()?.to_owned(),
                        ))
                    })
            } else {
                None
            };
            return Err(io::Error::other(Refusal {
                status: local,
                check: if matches!(status, 402 | 429) {
                    "upstream credit or rate limit"
                } else {
                    "upstream HTTP failure"
                },
                upstream: Some(status),
                upstream_error,
                no_usage: (400..600).contains(&status),
            }));
        }
        io::copy(input, output).map_err(|e| with_upstream(e, status))?;
        return Ok(());
    }
    Err(deny(502, "too many upstream informational responses"))
}

fn curl(key: &str, request: &Request, output: &mut dyn Write) -> io::Result<()> {
    // Config goes over stdin, never argv, env, a file, or diagnostics. -q
    // disables user curlrc; proxy env is disabled and redirects are not enabled.
    let config = curl_config(key, request)?;
    let mut command = crate::noninteractive_process::curl_command();
    command
        .args([
            "-q",
            "--silent",
            "--include",
            "--no-buffer",
            "--proto",
            "=https",
            "--proxy",
            "",
            "--suppress-connect-headers",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Inherits the server's zero core hard limit, installed before key read.
    let mut child = command
        .spawn()
        .map_err(|_| deny(502, "cannot start upstream transport"))?;
    let result: io::Result<()> = (|| {
        child
            .stdin
            .take()
            .ok_or_else(|| deny(502, "upstream transport stdin unavailable"))?
            .write_all(config.as_bytes())
            .map_err(|_| deny(502, "upstream transport config write failed"))?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| deny(502, "upstream transport stdout unavailable"))?,
        );
        forward_upstream(&mut stdout, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|_| deny(502, "cannot reap upstream transport"))?;
    result?;
    if !status.success() {
        return Err(with_upstream(
            deny(502, "upstream transport process failed"),
            200,
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "anthropic_proxy_tests.rs"]
mod tests;
