//! Slice-one API transport. The next worker slice owns start/register/revoke;
//! no existing subscription worker is silently switched to paid API access.
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};
use serde_json::Value;

const MODEL: &str = "claude-sonnet-4-5-20250929";
const BODY_MAX: usize = 150_000;
const LINE_MAX: usize = 1_000_000;

fn refused() -> io::Error {
    // Never include credentials, upstream diagnostics, or request text.
    io::Error::other("Anthropic proxy refused the request or accounting is unavailable")
}

/// No Debug implementation: neither the key nor run tokens are printable.
pub(crate) struct AnthropicProxy {
    key: String,
    cap: f64,
    ledger: Connection,
    grants: HashMap<String, (String, String)>,
    killed: bool,
}

impl AnthropicProxy {
    pub(crate) fn open(config: &crate::config::WorkersConfig, ledger: &Path) -> io::Result<Self> {
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

    fn available(&self, reserve: f64) -> io::Result<()> {
        let (cost, pending): (f64, i64) = self
            .ledger
            .query_row(
                "SELECT COALESCE(SUM(cost),0), COALESCE(SUM(pending),0) FROM anthropic_requests",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| refused())?;
        if self.killed || pending != 0 || cost >= self.cap || cost + reserve > self.cap {
            return Err(refused());
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
        upstream: impl FnOnce(&str, &[u8], &mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        let (token, body, streaming, reserve) = request(client)?;
        let (run, item) = self.grants.get(&token).cloned().ok_or_else(refused)?;
        self.ledger
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| refused())?;
        let admission =
            (|| {
                self.available(reserve)?;
                self.ledger.execute(
                "INSERT INTO anthropic_requests(run,item,model,pending) VALUES (?1,?2,?3,1)",
                params![run, item, MODEL]).map_err(|_| refused())?;
                self.ledger.execute_batch("COMMIT").map_err(|_| refused())
            })();
        if admission.is_err() {
            let _ = self.ledger.execute_batch("ROLLBACK");
            return Err(refused());
        }
        let id = self.ledger.last_insert_rowid();
        let mut response = Response {
            output,
            streaming,
            line: Vec::new(),
            json: Vec::new(),
            usage: Usage::default(),
            started: false,
            finished: false,
            delta: false,
            header: false,
        };
        upstream(&self.key, &body, &mut response)?;
        let usage = response.finish()?;
        self.ledger.execute(
            "UPDATE anthropic_requests SET input=?1,output=?2,cache_read=?3,cache_write=?4,cost=?5,pending=0 WHERE id=?6",
            params![usage.input as i64, usage.output as i64, usage.read as i64, usage.write as i64, usage.cost(), id])
            .map_err(|_| refused())?;
        Ok(())
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
                if result.is_err() && !output.wrote {
                    let _ = output.write_all(
                        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                // A failed stream ends by EOF, not an HTTP status appended to
                // its body. Its pending ledger entry keeps admission closed.
            }
            let _ = client.shutdown(std::net::Shutdown::Both);
        }
        Ok(())
    }
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

fn request(input: impl Read) -> io::Result<(String, Vec<u8>, bool, f64)> {
    let mut input = BufReader::new(input);
    let mut head = Vec::new();
    loop {
        let line = bounded_line(&mut input, 8192)?;
        head.extend_from_slice(&line);
        if head.len() > 8192 {
            return Err(refused());
        }
        if line == b"\r\n" {
            break;
        }
    }
    let head = std::str::from_utf8(&head).map_err(|_| refused())?;
    let mut lines = head.split("\r\n");
    if lines.next() != Some("POST /v1/messages HTTP/1.1") {
        return Err(refused());
    }
    let mut token = None;
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or_else(refused)?;
        match name.to_ascii_lowercase().as_str() {
            "authorization" => {
                if token.is_some() {
                    return Err(refused());
                }
                token = Some(
                    value
                        .trim()
                        .strip_prefix("Bearer ")
                        .ok_or_else(refused)?
                        .to_owned(),
                );
            }
            "content-length" => {
                if length.is_some() {
                    return Err(refused());
                }
                length = Some(value.trim().parse::<usize>().map_err(|_| refused())?);
            }
            "transfer-encoding" | "expect" => return Err(refused()),
            _ => {} // No client headers are forwarded, including x-api-key and Host.
        }
    }
    let length = length.filter(|n| *n <= BODY_MAX).ok_or_else(refused)?;
    let mut body = vec![0; length];
    input.read_exact(&mut body)?;
    let mut data: Value = serde_json::from_slice(&body).map_err(|_| refused())?;
    if data["model"].as_str() != Some(MODEL) {
        return Err(refused());
    }
    let object = data.as_object().ok_or_else(refused)?;
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
            return Err(refused());
        }
    }
    if let Some(thinking) = data.get("thinking") {
        if thinking["type"].as_str() != Some("disabled") {
            return Err(refused());
        }
    }
    if let Some(system) = data.get("system") {
        text_content(system, false)?;
    }
    for message in data["messages"].as_array().ok_or_else(refused)? {
        let message = message.as_object().ok_or_else(refused)?;
        if message
            .keys()
            .any(|key| !["role", "content"].contains(&key.as_str()))
        {
            return Err(refused());
        }
        text_content(message.get("content").ok_or_else(refused)?, true)?;
    }
    // Server-side tools and premium service tiers carry non-token charges.
    // Only ordinary client-defined tools are accepted in this first slice.
    if let Some(tools) = data.get("tools") {
        for tool in tools.as_array().ok_or_else(refused)? {
            if tool.get("type").is_some() || !tool["input_schema"].is_object() {
                return Err(refused());
            }
            inline_schema(&tool["input_schema"])?;
        }
    }
    let max = data["max_tokens"]
        .as_u64()
        .filter(|n| *n > 0 && *n <= 64_000)
        .ok_or_else(refused)?;
    let streaming = match data.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(refused()),
    };
    // The API's default `auto` tier may consume priority capacity. Force the
    // standard tier whose prices the ledger and reservation actually know.
    data["service_tier"] = Value::String("standard_only".into());
    data["thinking"] = serde_json::json!({"type": "disabled"});
    // Re-encode the validated value: duplicate JSON keys cannot cause the
    // upstream to interpret a different model or token ceiling than we did.
    let body = serde_json::to_vec(&data).map_err(|_| refused())?;
    if body.len() > BODY_MAX {
        return Err(refused());
    }
    // Plaintext tokens cannot exceed bytes plus protocol framing. Reserve a
    // deliberately generous 1024 tokens per message/tool/system block, not
    // just per request: many short tools must not defeat the upper bound.
    let count = |name: &str| data[name].as_array().map_or(0, Vec::len);
    let input_bound =
        body.len() + 1024 * (1 + count("messages") + count("tools") + count("system"));
    if input_bound >= 200_000 {
        return Err(refused());
    } // Exclude long-context premium pricing.
      // 6 USD/M covers even one-hour cache writes; output is standard tier.
    let reserve = input_bound as f64 * 6.0 / 1_000_000.0 + max as f64 * 15.0 / 1_000_000.0;
    Ok((token.ok_or_else(refused)?, body, streaming, reserve))
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
            return Err(refused());
        }
        let count = |name: &str| {
            value[name]
                .as_u64()
                .filter(|n| *n <= 1_000_000)
                .ok_or_else(refused)
        };
        if initial {
            self.input = count("input_tokens")?;
            // A missing billed component is unavailable accounting, not zero.
            self.read = count("cache_read_input_tokens")?;
            self.write = count("cache_creation_input_tokens")?;
            self.hour = match value.pointer("/cache_creation/ephemeral_1h_input_tokens") {
                Some(hour) => hour.as_u64().ok_or_else(refused)?,
                None if self.write == 0 => 0,
                None => return Err(refused()), // Unknown cache pricing must not undercount.
            };
            if self.hour > self.write {
                return Err(refused());
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
            let value: Value = serde_json::from_slice(&self.json).map_err(|_| refused())?;
            if value["model"].as_str() != Some(MODEL) {
                return Err(refused());
            }
            self.usage.parse(&value["usage"], true)?;
            self.finished = true;
        }
        if !self.finished || !self.line.is_empty() || (self.streaming && !self.delta) {
            return Err(refused());
        }
        Ok(self.usage)
    }
    fn event(&mut self) -> io::Result<()> {
        if let Some(data) = self.line.strip_prefix(b"data: ") {
            let event: Value = serde_json::from_slice(data).map_err(|_| refused())?;
            match event["type"].as_str() {
                Some("message_start") if !self.started => {
                    if event["message"]["model"].as_str() != Some(MODEL) {
                        return Err(refused());
                    }
                    self.usage.parse(&event["message"]["usage"], true)?;
                    self.started = true;
                }
                Some("message_delta") if self.started => {
                    self.usage.parse(&event["usage"], false)?;
                    self.delta = true;
                }
                Some("message_stop") if self.started => self.finished = true,
                Some("error") => return Err(refused()),
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
                    return Err(refused());
                }
                if *byte == b'\n' {
                    self.event()?;
                    self.line.clear();
                }
            }
        } else {
            if self.json.len() + bytes.len() > 16_000_000 {
                return Err(refused());
            }
            self.json.extend_from_slice(bytes);
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

fn curl_config(key: &str, body: &[u8]) -> io::Result<String> {
    let quote = |value: &str| {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    let data = std::str::from_utf8(body).map_err(|_| refused())?;
    Ok(format!("url = \"https://api.anthropic.com/v1/messages\"\nrequest = \"POST\"\nheader = \"x-api-key: {}\"\nheader = \"anthropic-version: 2023-06-01\"\nheader = \"content-type: application/json\"\ndata = \"{}\"\n", quote(key), quote(data)))
}

fn curl(key: &str, body: &[u8], output: &mut dyn Write) -> io::Result<()> {
    // Config goes over stdin, never argv, env, a file, or diagnostics. -q
    // disables user curlrc; proxy env is disabled and redirects are not enabled.
    let config = curl_config(key, body)?;
    let mut command = crate::noninteractive_process::curl_command();
    command
        .args([
            "-q",
            "--silent",
            "--fail",
            "--no-buffer",
            "--proto",
            "=https",
            "--proxy",
            "",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Inherits the server's zero core hard limit, installed before key read.
    let mut child = command.spawn().map_err(|_| refused())?;
    let result: io::Result<()> = (|| {
        child
            .stdin
            .take()
            .ok_or_else(refused)?
            .write_all(config.as_bytes())?;
        let mut stdout = child.stdout.take().ok_or_else(refused)?;
        // No upstream headers or errors are reflected to the caller.
        io::copy(&mut stdout, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|_| refused())?;
    result?;
    if !status.success() {
        return Err(refused());
    }
    Ok(())
}

#[cfg(test)]
#[path = "anthropic_proxy_tests.rs"]
mod tests;
