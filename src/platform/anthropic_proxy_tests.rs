use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Request-policy tests need neither a credential file nor filesystem access.
/// The disk-backed fixture below separately characterizes durable accounting.
struct OfflineFixture {
    proxy: AnthropicProxy,
}
impl OfflineFixture {
    fn new(cap: f64) -> Self {
        let ledger = Connection::open_in_memory().unwrap();
        ledger
            .execute_batch(
                "CREATE TABLE anthropic_requests (
            id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
            model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
            cache_write INTEGER, cost REAL, pending INTEGER NOT NULL);",
            )
            .unwrap();
        Self {
            proxy: AnthropicProxy {
                key: "sk-ant-fake-offline-key".into(),
                cap,
                model: MODEL.into(),
                ledger,
                grants: HashMap::new(),
                killed: false,
            },
        }
    }
}

struct Fixture {
    dir: std::path::PathBuf,
    proxy: AnthropicProxy,
}
impl Fixture {
    fn new(cap: f64) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-api-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let key = dir.join("key");
        std::fs::write(&key, "sk-ant-fake-offline-key\n").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        let config = crate::config::WorkersConfig {
            anthropic_api_key_file: key.display().to_string(),
            anthropic_api_cap_usd: cap,
            ..Default::default()
        };
        let proxy = AnthropicProxy::open(&config, &dir.join("ledger.sqlite")).unwrap();
        Self { dir, proxy }
    }
    fn request(token: &str, stream: bool) -> Vec<u8> {
        let body =
            serde_json::json!({"model": MODEL,"max_tokens":100,"messages":[],"stream":stream})
                .to_string();
        format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nX-Untrusted: untrusted\r\nHost: evil.invalid\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }
    fn response() -> Vec<u8> {
        serde_json::json!({"model":MODEL,"usage":{"input_tokens":10,"output_tokens":20,
            "cache_creation_input_tokens":4,"cache_read_input_tokens":2,
            "cache_creation":{"ephemeral_1h_input_tokens":1}}})
        .to_string()
        .into_bytes()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn loopback_pi_key_auth_accounts_offline_and_refuses_missing_token() {
    let mut f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    // Leave the fixture's cleanup alive while the listener owns this connection.
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        anthropic_api_cap_usd: 200.0,
        ..Default::default()
    };
    let proxy = AnthropicProxy::open(&config, &ledger).unwrap();
    let listener = LoopbackProxy::start_with(
        proxy,
        ledger.clone(),
        "todo-run",
        "todo-item",
        |key, _, out| {
            assert_eq!(key, "sk-ant-fake-offline-key");
            out.write_all(&Fixture::response())
        },
    )
    .unwrap();
    let exchange = |request: Vec<u8>| {
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", listener.port)).unwrap();
        stream.write_all(&request).unwrap();
        let mut output = String::new();
        stream.read_to_string(&mut output).unwrap();
        output
    };
    let request = String::from_utf8(Fixture::request(&listener.token, false))
        .unwrap()
        .replace(
            &format!("Authorization: Bearer {}", listener.token),
            &format!("x-api-key: {}", listener.token),
        );
    assert!(exchange(request.into_bytes()).starts_with("HTTP/1.1 200 OK"));
    assert!(exchange(Fixture::request("not-a-grant", false)).starts_with("HTTP/1.1 401"));
    let (cost, pending) = AnthropicProxy::accounting(&ledger, Some("todo-run")).unwrap();
    assert!((cost - 0.00034785).abs() < 1e-12);
    assert_eq!(pending, 0);
    f.proxy.cap = cost;
    assert!(f.proxy.available(0.0).is_err());
    assert!(f.proxy.register("second", "item").is_err());
    drop(listener);
}

#[test]
fn selected_model_is_the_only_allowlisted_model_and_tariffs_fail_closed() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("model", "item").unwrap();
    f.proxy.model = "claude-sonnet-4-6".into();
    let old = Fixture::request(&token, false);
    assert!(f
        .proxy
        .serve(old.as_slice(), &mut Vec::new(), |_, _, _| panic!())
        .is_err());
    let selected = String::from_utf8(old)
        .unwrap()
        .replace(MODEL, &f.proxy.model);
    f.proxy
        .serve(selected.as_bytes(), &mut Vec::new(), |_, req, out| {
            assert_eq!(
                serde_json::from_slice::<Value>(req).unwrap()["model"],
                "claude-sonnet-4-6"
            );
            out.write_all(
                &String::from_utf8(Fixture::response())
                    .unwrap()
                    .replace(MODEL, "claude-sonnet-4-6")
                    .into_bytes(),
            )
        })
        .unwrap();
    let mut config = crate::config::WorkersConfig::default();
    assert_eq!(config.anthropic_api_model, MODEL);
    config.anthropic_api_model = "claude-opus-5-5".into();
    assert!(config.validate_anthropic_model().is_err());
    let loaded: crate::config::WorkersConfig =
        toml::from_str("anthropic_api_model = 'claude-sonnet-4-6'").unwrap();
    assert_eq!(loaded.anthropic_api_model, "claude-sonnet-4-6");
    assert!(loaded.validate_anthropic_model().is_ok());
}

#[test]
fn refusals_are_distinct_and_do_not_echo_secrets_or_upstream_bodies() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("refusals", "item").unwrap();
    let good = String::from_utf8(Fixture::request(&token, false)).unwrap();
    for (raw, status, reason) in [
        (
            good.replace(&token, "secret-invalid-token"),
            401,
            "invalid or revoked run token",
        ),
        (
            good.replace("/v1/messages", "/secret-path"),
            403,
            "path not allowlisted",
        ),
        (
            good.replace(MODEL, "secret-model-0000"),
            403,
            "model not allowlisted",
        ),
    ] {
        let mut output = Vec::new();
        assert!(f
            .proxy
            .serve(raw.as_bytes(), &mut output, |_, _, _| panic!())
            .is_err());
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with(&format!("HTTP/1.1 {status}")));
        assert!(text.contains(reason));
        for secret in [
            &token,
            "secret-invalid-token",
            "secret-path",
            "secret-model",
            "sk-ant-",
        ] {
            assert!(!text.contains(secret));
        }
    }
    f.proxy.cap = 0.0001;
    let mut output = Vec::new();
    assert!(f
        .proxy
        .serve(good.as_bytes(), &mut output, |_, _, _| panic!())
        .is_err());
    assert!(String::from_utf8(output).unwrap().contains("402 Refused"));
    for (upstream, local) in [
        (401, 502),
        (403, 502),
        (402, 402),
        (429, 429),
        (500, 502),
        (302, 502),
    ] {
        let raw =
            format!("HTTP/1.1 {upstream} Error\r\nX-Secret: never-log\r\n\r\nsecret-error-body");
        let error =
            forward_upstream(&mut BufReader::new(raw.as_bytes()), &mut Vec::new()).unwrap_err();
        let mut output = Vec::new();
        report_refusal(
            &error,
            &RequestContext {
                method: "POST",
                path: "/v1/messages",
                model: MODEL.into(),
            },
            &mut output,
            false,
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with(&format!("HTTP/1.1 {local}")));
        assert!(text.contains(&format!("\"upstream_status\":{upstream}")));
        assert!(!text.contains("secret-error-body") && !text.contains("never-log"));
    }
}

#[test]
fn invalid_upstream_json_returns_502_before_success_and_preserves_pending_accounting() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("invalid-response", "item").unwrap();
    let mut output = Vec::new();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut output,
            |_, _, out| out.write_all(b"secret-invalid-upstream-json")
        )
        .is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("HTTP/1.1 502"));
    assert!(text.contains("invalid upstream message JSON"));
    assert!(text.contains("\"upstream_status\":200"));
    assert!(!text.contains("secret-invalid-upstream-json"));
    assert!(f.proxy.available(0.0).is_err());
}

#[test]
fn upstream_stream_limit_errors_keep_their_status_without_echoing_error_text() {
    for (kind, status) in [
        ("rate_limit_error", 429),
        ("billing_error", 402),
        ("overloaded_error", 502),
    ] {
        let mut f = OfflineFixture::new(200.0);
        let token = f.proxy.register("stream-error", "item").unwrap();
        let mut output = Vec::new();
        let raw = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"{kind}\",\"message\":\"secret-upstream-diagnostics\"}}}}\n\n");
        assert!(f
            .proxy
            .serve(
                Fixture::request(&token, true).as_slice(),
                &mut output,
                |_, _, out| forward_upstream(&mut BufReader::new(raw.as_bytes()), out)
            )
            .is_err());
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with(&format!("HTTP/1.1 {status}")));
        assert!(text.contains("\"upstream_status\":200"));
        assert!(!text.contains("secret-upstream-diagnostics"));
        assert!(f.proxy.available(0.0).is_err());
    }
}

#[test]
fn refusal_log_names_the_check_and_safe_request_metadata() {
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let capture = Capture(Arc::clone(&bytes));
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || capture.clone())
        .finish();
    let error = io::Error::other(Refusal {
        status: 429,
        check: "upstream credit or rate limit",
        upstream: Some(429),
    });
    tracing::subscriber::with_default(subscriber, || {
        report_refusal(
            &error,
            &RequestContext {
                method: "POST",
                path: "/v1/messages?beta=true",
                model: MODEL.into(),
            },
            &mut Vec::new(),
            false,
        );
    });
    let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "POST",
        "/v1/messages?beta=true",
        MODEL,
        "upstream credit or rate limit",
        "429",
    ] {
        assert!(log.contains(field), "{log}");
    }
    assert!(!log.contains("sk-ant-") && !log.contains("x-api-key"));
}

#[test]
fn pi_message_and_count_tokens_routes_preserve_only_allowed_beta_headers() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("pi-shape", "item").unwrap();
    for path in [
        "/v1/messages?beta=true",
        "/v1/messages/count_tokens",
        "/v1/messages/count_tokens?beta=true",
    ] {
        let body = if path.contains("count_tokens") {
            serde_json::json!({"model":MODEL,"messages":[{"role":"user","content":[{"type":"text","text":"offline","cache_control":{"type":"ephemeral"}}]}]}).to_string()
        } else {
            serde_json::json!({"model":MODEL,"max_tokens":100,"messages":[]}).to_string()
        };
        let raw = format!("POST {path} HTTP/1.1\r\nx-api-key: {token}\r\nanthropic-beta: fine-grained-tool-streaming-2025-05-14\r\nHost: evil.invalid\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut output = Vec::new();
        f.proxy
            .serve(raw.as_bytes(), &mut output, |_, req, out| {
                assert_eq!(req.path, path);
                let config = curl_config("sk-ant-fake", req).unwrap();
                assert!(config.contains(&format!("https://api.anthropic.com{path}")));
                assert!(config.contains("anthropic-beta: fine-grained-tool-streaming-2025-05-14"));
                assert!(!config.contains("evil.invalid") && !config.contains(&token));
                if req.count {
                    out.write_all(b"{\"input_tokens\":7}")
                } else {
                    out.write_all(&Fixture::response())
                }
            })
            .unwrap();
        assert!(output.starts_with(b"HTTP/1.1 200 OK"));
    }
    let rows: i64 = f
        .proxy
        .ledger
        .query_row("SELECT COUNT(*) FROM anthropic_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1, "count_tokens creates no billed message rows");
}

#[test]
fn cycle_requires_explicit_operator_identifier_and_cannot_escape_state_directory() {
    let mut config = crate::config::WorkersConfig::default();
    assert!(ledger_path(&config).is_err());
    config.anthropic_api_cycle = "../bad".into();
    assert!(ledger_path(&config).is_err());
    config.anthropic_api_cycle = "cycle-2026-11".into();
    assert!(ledger_path(&config)
        .unwrap()
        .ends_with("anthropic-credits-cycle-2026-11.sqlite"));
}

#[test]
fn anthropic_proxy_rewrites_auth_and_accounts_run_item_and_cache() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run-a", "item-a").unwrap();
    let mut output = Vec::new();
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut output,
            |key, body, out| {
                assert_eq!(key, "sk-ant-fake-offline-key");
                let request: Value = serde_json::from_slice(body).unwrap();
                assert_eq!(request["service_tier"], "standard_only");
                let config = curl_config(key, body).unwrap();
                assert!(config.contains("url = \"https://api.anthropic.com/v1/messages\""));
                assert!(config.contains("header = \"x-api-key: sk-ant-fake-offline-key\""));
                assert!(!config.contains("Authorization:"));
                assert!(!config.contains("evil.invalid"));
                assert!(!config.contains("untrusted"));
                assert!(!String::from_utf8_lossy(body).contains("untrusted"));
                assert!(!String::from_utf8_lossy(body).contains("evil.invalid"));
                out.write_all(&Fixture::response())
            },
        )
        .unwrap();
    let row: (String, String, String, i64, i64, f64, i64) = f
        .proxy
        .ledger
        .query_row(
            "SELECT run,item,model,input,output,cost,pending FROM anthropic_requests",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        (&row.0[..], &row.1[..], &row.2[..], row.3, row.4, row.6),
        ("run-a", "item-a", MODEL, 10, 20, 0)
    );
    assert!((row.5 - 0.00034785).abs() < 1e-12);
    assert!(output.starts_with(b"HTTP/1.1 200 OK"));
    assert!(!String::from_utf8_lossy(&output).contains("sk-ant-"));
}

#[test]
fn anthropic_proxy_streams_fragments_and_uses_final_usage() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("stream", "item").unwrap();
    let sse = format!("event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"model\":\"{MODEL}\",\"usage\":{{\"input_tokens\":12,\"output_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":30}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n");
    let mut output = Vec::new();
    f.proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut output,
            |_, _, out| {
                for chunk in sse.as_bytes().chunks(3) {
                    out.write_all(chunk)?;
                }
                Ok(())
            },
        )
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.split_once("\r\n\r\n").unwrap().1, sse);
    let cost: f64 = f
        .proxy
        .ledger
        .query_row("SELECT cost FROM anthropic_requests", [], |r| r.get(0))
        .unwrap();
    assert!((cost - 0.000486).abs() < 1e-12);
}

#[test]
fn anthropic_proxy_refuses_routes_models_auth_and_paid_server_tools() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("r", "i").unwrap();
    let good = String::from_utf8(Fixture::request(&token, false)).unwrap();
    for bad in [
        good.replace("POST /v1/messages", "CONNECT api.anthropic.com:443"),
        good.replace("/v1/messages", "https://evil.invalid/v1/messages"),
        good.replace("/v1/messages", "/v1/messages?x=1"),
        good.replace("sonnet", "badmod"),
        good.replace(&token, "invalid"),
        good.replace("Host:", "Transfer-Encoding: chunked\r\nHost:"),
        good.replace("Host:", "Authorization: Bearer other\r\nHost:"),
    ] {
        assert!(f
            .proxy
            .serve(bad.as_bytes(), &mut Vec::new(), |_, _, _| panic!(
                "must not reach upstream"
            ))
            .is_err());
    }
    let body = serde_json::json!({"model":MODEL,"max_tokens":100,"messages":[],
        "tools":[{"type":"web_search_20250305","name":"web_search"}]})
    .to_string();
    let bad = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    assert!(request(bad.as_bytes(), MODEL, &mut RequestContext::default()).is_err());
    f.proxy.revoke(&token);
    assert!(f
        .proxy
        .serve(good.as_bytes(), &mut Vec::new(), |_, _, _| panic!())
        .is_err());
}

#[test]
fn anthropic_proxy_cap_kill_and_uncertain_usage_fail_closed_durably() {
    let mut f = Fixture::new(0.0);
    assert!(f.proxy.register("r", "i").is_err());
    f.proxy.cap = 200.0;
    let token = f.proxy.register("r", "i").unwrap();
    f.proxy.cap = 0.0001;
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, _| panic!()
        )
        .is_err());
    f.proxy.cap = 200.0;
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(b"{\"error\":true}")
        )
        .is_err());
    assert!(f.proxy.register("new", "item").is_err());
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        ..Default::default()
    };
    let mut reopened = AnthropicProxy::open(&config, &f.dir.join("ledger.sqlite")).unwrap();
    assert!(reopened.register("new", "item").is_err());
    let mut other = Fixture::new(200.0);
    other.proxy.kill();
    assert!(other.proxy.register("r", "i").is_err());
}

#[test]
fn anthropic_proxy_unavailable_counter_and_incomplete_stream_refuse_more_spend() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run", "item").unwrap();
    let other = f.proxy.register("other-run", "other-item").unwrap();
    assert_ne!(token, other);
    f.proxy
        .ledger
        .execute_batch("DROP TABLE anthropic_requests")
        .unwrap();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, _| panic!("unavailable counter must deny before upstream")
        )
        .is_err());
    let mut streaming = Fixture::new(200.0);
    let token = streaming.proxy.register("stream", "item").unwrap();
    assert!(streaming
        .proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
        )
        .is_err());
    assert!(streaming.proxy.register("next", "item").is_err());
}

#[test]
fn anthropic_proxy_completed_spend_survives_restart_and_shared_keys_are_refused() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("r", "i").unwrap();
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(&Fixture::response()),
        )
        .unwrap();
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        anthropic_api_cap_usd: 0.0003,
        ..Default::default()
    };
    let mut reopened = AnthropicProxy::open(&config, &f.dir.join("ledger.sqlite")).unwrap();
    assert!(reopened.register("r2", "i").is_err());
    assert_eq!(
        crate::config::WorkersConfig::default().anthropic_api_cap_usd,
        200.0
    );
    std::fs::set_permissions(f.dir.join("key"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(AnthropicProxy::open(&config, &f.dir.join("other.sqlite")).is_err());
}

#[test]
fn anthropic_proxy_canonicalizes_duplicate_keys_and_refuses_unknown_cache_cost() {
    let body = format!("{{\"model\":\"expensive-model\",\"model\":\"{MODEL}\",\"max_tokens\":999999,\"max_tokens\":100,\"messages\":[]}}");
    let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer token\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let normalized = request(raw.as_bytes(), MODEL, &mut RequestContext::default()).unwrap();
    let text = String::from_utf8(normalized.body).unwrap();
    assert!(!text.contains("expensive-model"));
    assert!(!text.contains("999999"));
    assert_eq!(text.matches("max_tokens").count(), 1);
    let mut usage = Usage::default();
    assert!(usage
        .parse(
            &serde_json::json!({"input_tokens":1,"output_tokens":2,"service_tier":"priority"}),
            true
        )
        .is_err());
    assert!(usage.parse(&serde_json::json!({"input_tokens":1,"output_tokens":2,"cache_creation_input_tokens":3,"cache_read_input_tokens":0}), true).is_err());
}

#[test]
fn anthropic_proxy_refuses_public_socket_parents_and_unbounded_input_sources() {
    let f = Fixture::new(200.0);
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(bind_socket(&f.dir.join("socket")).is_err());
    for block in [
        serde_json::json!({"type":"image", "source":{"type":"url", "url":"https://example.invalid/large"}}),
        serde_json::json!({"type":"document", "source":{"type":"file", "file_id":"fake"}}),
        serde_json::json!({"type":"redacted_thinking", "data":"opaque"}),
        serde_json::json!({"type":"thinking", "thinking":"summary", "signature":"opaque"}),
        serde_json::json!({"type":"tool_result", "tool_use_id":"id", "content":[{"type":"image"}]}),
    ] {
        assert!(text_content(&serde_json::json!([block]), true).is_err());
    }
    assert!(inline_schema(&serde_json::json!({"type":"object", "$ref":"#/large"})).is_err());
    assert!(text_content(&serde_json::json!([{"type":"text", "text":"hello"}]), true).is_ok());
}

/// Capture the installed pi provider against a fake upstream only. This is
/// separate from hermetic coverage because CI need not have pi installed and
/// worker sandboxes may deny listening sockets. Run explicitly with nextest's
/// --run-ignored all and this test's name before a live credit smoke test.
#[test]
#[ignore = "requires pi 1.1 and loopback sockets; upstream is entirely fake"]
fn installed_pi_anthropic_messages_request_shape_offline() {
    let version = std::process::Command::new("pi")
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    let version = String::from_utf8(version.stdout).unwrap();
    assert!(
        version.trim().starts_with("1.1."),
        "this capture targets pi 1.1"
    );
    eprintln!("pi capture version: {}", version.trim());
    static REQUESTS: AtomicUsize = AtomicUsize::new(0);
    let f = Fixture::new(200.0);
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        ..Default::default()
    };
    let ledger = f.dir.join("capture.sqlite");
    let proxy = AnthropicProxy::open(&config, &ledger).unwrap();
    let listener = LoopbackProxy::start_with(proxy, ledger, "capture", "item", |_, req, out| {
        // Prints only the selected route and protocol feature headers, not
        // credentials, prompts, tool schemas or other request body contents.
        eprintln!("pi anthropic-messages: POST {} anthropic-beta={:?}", req.path, req.beta);
        assert!(matches!(req.path, "/v1/messages" | "/v1/messages?beta=true"));
        assert!(req.streaming);
        REQUESTS.fetch_add(1, Ordering::SeqCst);
        let events = [
            serde_json::json!({"type":"message_start","message":{"id":"msg_offline","type":"message","role":"assistant","model":MODEL,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}),
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
            serde_json::json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}),
            serde_json::json!({"type":"message_stop"}),
        ];
        for event in events { write!(out, "event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())?; }
        Ok(())
    }).unwrap();
    // Capture at the client boundary, before the proxy can reject a new pi
    // feature, so a failed compatibility test still names the exact shape.
    let capture_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let capture_address = format!("http://{}", capture_listener.local_addr().unwrap());
    let proxy_port = listener.port;
    let capture_thread = std::thread::spawn(move || {
        let (mut client, _) = capture_listener.accept().unwrap();
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut raw = Vec::new();
        let mut length = None;
        for _ in 0..128 {
            let line = bounded_line(&mut input, 8192).unwrap();
            let text = std::str::from_utf8(&line).unwrap();
            if raw.is_empty()
                || text.to_ascii_lowercase().starts_with("anthropic-beta:")
                || text.to_ascii_lowercase().starts_with("anthropic-version:")
                || text.to_ascii_lowercase().starts_with("content-type:")
            {
                eprintln!("pi request shape: {}", text.trim_end());
            }
            if let Some((name, value)) = text.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
            raw.extend_from_slice(&line);
            if line == b"\r\n" {
                break;
            }
        }
        let length = length.filter(|length| *length <= BODY_MAX).unwrap();
        let mut body = vec![0; length];
        input.read_exact(&mut body).unwrap();
        let data: Value = serde_json::from_slice(&body).unwrap();
        eprintln!(
            "pi body field names: {:?}",
            data.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        raw.extend_from_slice(&body);
        let mut upstream = std::net::TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        upstream.write_all(&raw).unwrap();
        io::copy(&mut upstream, &mut client).unwrap();
        raw
    });
    let agent_dir = f.dir.join("pi-agent");
    std::fs::create_dir(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("models.json"), serde_json::json!({"providers":{"offline":{
        "baseUrl":capture_address,"api":"anthropic-messages","apiKey":"${HERDR_ANTHROPIC_PROXY_TOKEN}",
        "models":[{"id":MODEL,"contextWindow":200000,"maxTokens":8192}]
    }}}).to_string()).unwrap();
    let mut child = std::process::Command::new("pi")
        .args([
            "--mode",
            "rpc",
            "--model",
            &format!("offline/{MODEL}"),
            "--no-session",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "--no-mcp",
            "--no-approve",
            "--tools",
            "read",
        ])
        .current_dir(&f.dir)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("HERDR_ANTHROPIC_PROXY_TOKEN", &listener.token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("pi 1.1 must be installed for this explicit test");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"type\":\"prompt\",\"message\":\"Reply OK; do not use tools.\"}\n")
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut settled = false;
    for _ in 0..256 {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap() == 0 {
            break;
        }
        let event: Value = serde_json::from_str(&line).unwrap();
        if matches!(event["type"].as_str(), Some("agent_settled" | "agent_end")) {
            settled = true;
            break;
        }
    }
    let _ = child.kill();
    child.wait().unwrap();
    let captured = capture_thread.join().unwrap();
    let shape = request(captured.as_slice(), MODEL, &mut RequestContext::default()).unwrap();
    assert!(shape.streaming);
    assert!(settled, "pi must finish the fake streamed response");
    assert_eq!(REQUESTS.load(Ordering::SeqCst), 1);
    drop(listener);
}

#[test]
fn anthropic_proxy_real_socket_permissions_and_offline_http() {
    // This transport smoke test needs sockets; the other tests need none.
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("socket-run", "item").unwrap();
    let path = f.dir.join("socket");
    let listener = bind_socket(&path).expect("run socket test outside a socket-denying sandbox");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let input = socket.try_clone().unwrap();
        f.proxy
            .serve(input, &mut socket, |_, _, out| {
                out.write_all(&Fixture::response())
            })
            .unwrap();
    });
    let mut client = std::os::unix::net::UnixStream::connect(&path).unwrap();
    client.write_all(&Fixture::request(&token, false)).unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(!response.contains("sk-ant-"));
    thread.join().unwrap();
}
