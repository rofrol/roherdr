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
            stage TEXT, model TEXT NOT NULL, input INTEGER, output INTEGER,
            cache_read INTEGER, cache_write INTEGER, cost REAL,
            pending INTEGER NOT NULL, owner_pid INTEGER, owner_start INTEGER,
            reserved REAL);",
            )
            .unwrap();
        Self {
            proxy: AnthropicProxy {
                key: "sk-ant-fake-offline-key".into(),
                cap,
                model: MODEL.into(),
                ledger,
                ledger_path: std::path::PathBuf::new(),
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

fn thinking_request(token: &str, thinking: Value, max: u64) -> Vec<u8> {
    let body = serde_json::json!({
        "model": MODEL, "max_tokens": max, "messages": [], "thinking": thinking
    })
    .to_string();
    format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
}

#[test]
fn thinking_types_reach_fake_upstream_and_billed_thinking_counts_as_output() {
    for thinking in [
        serde_json::json!({"type":"disabled"}),
        serde_json::json!({"type":"enabled", "budget_tokens":2048}),
        serde_json::json!({"type":"adaptive"}),
        serde_json::json!({"type":"between_tools"}),
        serde_json::json!({"type":"between_tools", "budget_tokens":2048}),
    ] {
        let mut f = OfflineFixture::new(200.0);
        let token = f.proxy.register("thinking", "item", "review").unwrap();
        let request = thinking_request(&token, thinking.clone(), 4096);
        let mut output = Vec::new();
        f.proxy
            .serve(request.as_slice(), &mut output, |_, req, out| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["thinking"], thinking);
                assert_eq!(body["max_tokens"], 4096);
                assert_eq!(body["service_tier"], "standard_only");
                assert!(req.reserve >= 4096.0 * 15.0 / 1_000_000.0);
                // The billed output includes 2048 thinking and 20 visible
                // tokens; do not infer usage from visible content length.
                let mut response: Value = serde_json::from_slice(&Fixture::response()).unwrap();
                response["usage"]["output_tokens"] = Value::from(2068);
                out.write_all(&serde_json::to_vec(&response).unwrap())
            })
            .unwrap();
        let (cost, pending): (f64, i64) = f
            .proxy
            .ledger
            .query_row("SELECT cost,pending FROM anthropic_requests", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!((cost - (0.00034785 + 2048.0 * 15.0 / 1_000_000.0)).abs() < 1e-12);
        assert_eq!(pending, 0);
        assert!(output.starts_with(b"HTTP/1.1 200 OK"));
    }
}

#[test]
fn multi_turn_agent_content_reaches_fake_upstream_unchanged() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("conversation", "item", "review").unwrap();
    let messages = serde_json::json!([
        {"role":"user", "content":[{"type":"text", "text":"Inspect the image and read the file"},
            {"type":"image", "source":{"type":"base64", "media_type":"image/png", "data":"synthetic-image"}}]},
        {"role":"assistant", "content":[
            {"type":"thinking", "thinking":"I should read the file", "signature":"synthetic-signature"},
            {"type":"tool_use", "id":"call_1", "name":"read", "input":{"path":"example.txt"}}]},
        {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"call_1", "content":[
            {"type":"text", "text":"File contents"}]}]},
        {"role":"assistant", "content":[{"type":"text", "text":"The file is readable"}]},
        {"role":"user", "content":"Summarize it"}
    ]);
    // Invalid shapes, including missing messages, also belong to the API.
    for messages in [
        Some(messages),
        Some(serde_json::json!({"invalid":"shape"})),
        Some(serde_json::json!([{"unexpected":true}])),
        None,
    ] {
        let mut body = serde_json::json!({"model":MODEL, "max_tokens":4096,
            "system":[{"type":"text", "text":"Help with the task"}],
            "thinking":{"type":"adaptive"}, "output_config":{"effort":"medium"},
            "tools":[{"name":"read", "input_schema":{"type":"object", "properties":{"path":{"type":"string"}}}}]});
        if let Some(messages) = messages {
            body["messages"] = messages;
        }
        let encoded = body.to_string();
        let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n{encoded}", encoded.len());
        f.proxy
            .serve(raw.as_bytes(), &mut Vec::new(), |_, req, out| {
                let forwarded: Value = serde_json::from_slice(&req.body).unwrap();
                body["service_tier"] = serde_json::json!("standard_only");
                assert_eq!(forwarded, body);
                assert!(req.reserve >= 4096.0 * 15.0 / 1_000_000.0);
                out.write_all(&Fixture::response())
            })
            .unwrap();
    }
}

#[test]
fn full_size_agent_conversation_reaches_upstream_with_both_http_framings() {
    for chunked in [false, true] {
        for streaming in [false, true] {
            let mut f = OfflineFixture::new(200.0);
            let token = f
                .proxy
                .register("long-conversation", "item", "review")
                .unwrap();
            let mut messages =
                vec![serde_json::json!({"role":"user", "content":"Inspect the project"})];
            for turn in 0..32 {
                messages.push(serde_json::json!({"role":"assistant", "content":[
                    {"type":"thinking", "thinking":"Read another file", "signature":"synthetic"},
                    {"type":"tool_use", "id":format!("call_{turn}"), "name":"read", "input":{"path":"example.txt"}}
                ]}));
                messages.push(serde_json::json!({"role":"user", "content":[
                    {"type":"tool_result", "tool_use_id":format!("call_{turn}"), "content":"file contents\n".repeat(5100)}
                ]}));
            }
            let mut body = serde_json::json!({"model":MODEL, "max_tokens":4096,
                "messages":messages, "stream":streaming,
                "system":[{"type":"text", "text":"Help with the task", "cache_control":{"type":"ephemeral"}}],
                "thinking":{"type":"adaptive"}, "output_config":{"effort":"medium"},
                "tools":[{"name":"read", "input_schema":{"type":"object", "properties":{"path":{"type":"string"}}}}]});
            let encoded = serde_json::to_vec(&body).unwrap();
            assert!(encoded.len() > 2 * 1024 * 1024);
            let mut raw = format!("POST /v1/messages?beta=true HTTP/1.1\r\nAuthorization: Bearer {token}\r\nanthropic-beta: interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14\r\n").into_bytes();
            if chunked {
                raw.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
                for chunk in encoded.chunks(65536) {
                    write!(&mut raw, "{:x};test=extension\r\n", chunk.len()).unwrap();
                    raw.extend_from_slice(chunk);
                    raw.extend_from_slice(b"\r\n");
                }
                raw.extend_from_slice(b"0\r\nX-Ignored: trailer\r\n\r\n");
            } else {
                write!(&mut raw, "Content-Length: {}\r\n\r\n", encoded.len()).unwrap();
                raw.extend_from_slice(&encoded);
            }
            let mut output = Vec::new();
            f.proxy.serve(raw.as_slice(), &mut output, |_, req, out| {
                body["service_tier"] = serde_json::json!("standard_only");
                assert_eq!(serde_json::from_slice::<Value>(&req.body).unwrap(), body);
                assert!(req.reserve > 12.0);
                assert!(req.reserve < 200.0);
                assert!(curl_config("synthetic-key", req).unwrap().contains("api.anthropic.com/v1/messages?beta=true"));
                let mut message: Value = serde_json::from_slice(&Fixture::response()).unwrap();
                message["usage"]["input_tokens"] = Value::from(180_000);
                message["usage"]["cache_read_input_tokens"] = Value::from(80_000);
                let mut upstream = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n".to_vec();
                if streaming {
                    writeln!(&mut upstream, "data: {}\n", serde_json::json!({"type":"message_start", "message":message})).unwrap();
                    // A large tool argument event must not trip the old 1 MB
                    // SSE line limit. Transport fragments are not SSE lines.
                    writeln!(&mut upstream, "data: {}\n", serde_json::json!({"type":"content_block_delta", "delta":{"type":"input_json_delta", "partial_json":"x".repeat(2 * 1024 * 1024)}})).unwrap();
                    upstream.extend_from_slice(b"data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":20}}\n\ndata: {\"type\":\"message_stop\"}\n\n");
                } else {
                    upstream.extend_from_slice(&serde_json::to_vec(&message).unwrap());
                }
                forward_upstream(&mut BufReader::new(upstream.as_slice()), out)
            }).unwrap();
            assert!(output.starts_with(b"HTTP/1.1 200 OK"));
            let pending: i64 = f
                .proxy
                .ledger
                .query_row("SELECT pending FROM anthropic_requests", [], |r| r.get(0))
                .unwrap();
            assert_eq!(pending, 0);
        }
    }
}

#[test]
fn request_body_limits_and_chunk_framing_are_enforced_before_upstream() {
    assert_eq!(BODY_MAX, 32 * 1024 * 1024);
    let prefix = "POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer synthetic\r\n";
    for framing in [
        format!("Content-Length: {}\r\n\r\n", BODY_MAX + 1),
        format!("Transfer-Encoding: chunked\r\n\r\n{:x}\r\n", BODY_MAX + 1),
        "Content-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".into(),
        "Transfer-Encoding: gzip, chunked\r\n\r\n".into(),
        "Transfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n".into(),
        "Transfer-Encoding: chunked\r\n\r\n-1\r\n".into(),
        "Transfer-Encoding: chunked\r\n\r\n1\r\nx!!".into(),
        "Transfer-Encoding: chunked\r\n\r\n2\r\nx".into(),
        "Transfer-Encoding: chunked\r\n\r\n0\r\n".into(),
        "Transfer-Encoding: chunked\r\n\r\n0\r\ninvalid\r\n\r\n".into(),
        "\r\n{}".into(),
    ] {
        assert!(request(
            format!("{prefix}{framing}").as_bytes(),
            MODEL,
            &mut RequestContext::default()
        )
        .is_err());
    }
    // Exact decoded limit is accepted; a second chunk cannot exceed it, even
    // though each individual chunk is within the limit. No JSON allocation.
    let mut chunks = format!("{:x}\r\n", BODY_MAX).into_bytes();
    chunks.resize(chunks.len() + BODY_MAX, b'x');
    chunks.extend_from_slice(b"\r\n0\r\n\r\n");
    assert_eq!(
        chunked_body(&mut BufReader::new(chunks.as_slice()))
            .unwrap()
            .len(),
        BODY_MAX
    );
    chunks.truncate(chunks.len() - 5);
    chunks.extend_from_slice(b"1\r\n");
    let error = chunked_body(&mut BufReader::new(chunks.as_slice())).unwrap_err();
    assert_eq!(
        error
            .get_ref()
            .unwrap()
            .downcast_ref::<Refusal>()
            .unwrap()
            .status,
        413
    );
}

#[test]
fn long_context_tariff_uses_billed_tokens_not_bytes_and_reserves_premium() {
    let model = "claude-sonnet-4-5-20250929";
    let raw = thinking_request("synthetic", serde_json::json!({"type":"adaptive"}), 4096);
    let raw = String::from_utf8(raw).unwrap().replace(MODEL, model);
    // Rebuild length after changing the model name.
    let encoded = raw.split("\r\n\r\n").nth(1).unwrap();
    let mut body: Value = serde_json::from_str(encoded).unwrap();
    body["messages"] = serde_json::json!([{"role":"user", "content":"x".repeat(250_000)}]);
    let encoded = body.to_string();
    let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer synthetic\r\nContent-Length: {}\r\n\r\n{encoded}", encoded.len());
    let req = request(raw.as_bytes(), model, &mut RequestContext::default()).unwrap();
    assert!(req.reserve >= 250_000.0 * 12.0 / 1_000_000.0 + 4096.0 * 22.5 / 1_000_000.0);
    let mut usage = Usage {
        input: 100_000,
        read: 100_000,
        write: 0,
        output: 100,
        hour: 0,
    };
    assert_eq!(long_context_premium(model, 200_000), (1.0, 1.0));
    let standard = usage.cost(model);
    usage.read += 1;
    assert!(
        (usage.cost(model)
            - ((100_000.0 * 3.0 + 100_001.0 * 0.3) * 2.0 + 100.0 * 22.5) / 1_000_000.0)
            .abs()
            < 1e-12
    );
    assert!(usage.cost(model) > standard);
    assert_eq!(long_context_premium(MODEL, 500_000), (1.0, 1.0));
    assert_eq!(
        long_context_premium("claude-sonnet-4-6", 500_000),
        (1.0, 1.0)
    );
}

#[test]
fn thinking_output_reservation_denies_budget_beyond_remaining_credit() {
    for thinking in [
        serde_json::json!({"type":"enabled", "budget_tokens":2048}),
        serde_json::json!({"type":"adaptive"}),
        serde_json::json!({"type":"between_tools", "budget_tokens":2048}),
    ] {
        let mut f = OfflineFixture::new(1.0);
        let token = f.proxy.register("thinking", "item", "review").unwrap();
        f.proxy.ledger.execute(
            "INSERT INTO anthropic_requests(run,item,model,cost,pending) VALUES ('prior','item',?1,0.99,0)",
            [MODEL],
        ).unwrap();
        let request = thinking_request(&token, thinking, 4096);
        let mut output = Vec::new();
        assert!(f
            .proxy
            .serve(request.as_slice(), &mut output, |_, _, _| {
                panic!("no upstream bytes may precede the credit reservation")
            })
            .is_err());
        assert!(output.starts_with(b"HTTP/1.1 402"));
        let pending: i64 = f
            .proxy
            .ledger
            .query_row("SELECT SUM(pending) FROM anthropic_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pending, 0);
    }
}

#[test]
fn absent_thinking_is_not_rewritten_to_disabled() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("thinking", "item", "review").unwrap();
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, req, out| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert!(body.get("thinking").is_none());
                out.write_all(&Fixture::response())
            },
        )
        .unwrap();
}

#[test]
fn thinking_object_shape_is_left_to_the_api() {
    for thinking in [
        serde_json::json!({}),
        serde_json::json!({"type":"adaptive", "display":"summarized"}),
        serde_json::json!({"type":"unknown"}),
        serde_json::json!({"type":"enabled"}),
        serde_json::json!({"type":"enabled", "budget_tokens":0}),
        serde_json::json!({"type":"enabled", "budget_tokens":1023}),
        serde_json::json!({"type":"enabled", "budget_tokens":"2048"}),
        serde_json::json!({"type":"enabled", "budget_tokens":4096}),
        serde_json::json!({"type":"enabled", "budget_tokens":64001}),
        serde_json::json!({"type":"between_tools", "budget_tokens":4097}),
        serde_json::json!({"type":"adaptive", "budget_tokens":2048}),
        serde_json::json!({"type":"disabled", "budget_tokens":2048}),
        serde_json::json!({"type":"enabled", "budget_tokens":2048, "extra":true}),
    ] {
        let mut f = OfflineFixture::new(200.0);
        let token = f.proxy.register("thinking", "item", "review").unwrap();
        let request = thinking_request(&token, thinking.clone(), 4096);
        f.proxy
            .serve(request.as_slice(), &mut Vec::new(), |_, req, out| {
                let body: Value = serde_json::from_slice(&req.body).unwrap();
                assert_eq!(body["thinking"], thinking);
                assert!(req.reserve >= 4096.0 * 15.0 / 1_000_000.0);
                out.write_all(&Fixture::response())
            })
            .unwrap();
    }
}

#[test]
fn non_object_thinking_is_refused_with_exact_field_diagnostics() {
    for thinking in [
        serde_json::json!(null),
        serde_json::json!("adaptive"),
        serde_json::json!([]),
        serde_json::json!(42),
    ] {
        let raw = thinking_request("synthetic-token", thinking.clone(), 4096);
        let mut context = RequestContext::default();
        assert!(request(raw.as_slice(), MODEL, &mut context).is_err());
        assert_eq!(context.offending_field.as_deref(), Some("thinking"));
    }
}

#[test]
fn output_config_is_forwarded_but_unknown_top_level_fields_are_refused() {
    for field in ["output_config", "unknown_option"] {
        let value = serde_json::json!({"effort":"medium"});
        let body =
            serde_json::json!({"model": MODEL, "max_tokens":4096, "messages":[], field:value})
                .to_string();
        let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer synthetic-token\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut context = RequestContext::default();
        let result = request(raw.as_bytes(), MODEL, &mut context);
        if field == "output_config" {
            let body: Value = serde_json::from_slice(&result.unwrap().body).unwrap();
            assert_eq!(body[field], value);
        } else {
            assert!(result.is_err());
            assert_eq!(context.offending_field.as_deref(), Some(field));
        }
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
        "worker",
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
    let accounting = AnthropicProxy::accounting(&ledger, Some("todo-run")).unwrap();
    assert!((accounting.spent_usd - 0.00034785).abs() < 1e-12);
    assert_eq!(accounting.pending(), 0);
    f.proxy.cap = accounting.spent_usd;
    assert!(f.proxy.available(0.0).is_err());
    assert!(f.proxy.register("second", "item", "review").is_err());
    drop(listener);
}

#[test]
fn selected_model_is_the_only_allowlisted_model_and_tariffs_fail_closed() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("model", "item", "review").unwrap();
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
    let token = f.proxy.register("refusals", "item", "review").unwrap();
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
        (401, 401),
        (403, 403),
        (402, 402),
        (429, 429),
        (500, 500),
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
                ..Default::default()
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
    let token = f
        .proxy
        .register("invalid-response", "item", "review")
        .unwrap();
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
fn upstream_stream_limit_errors_keep_status_and_documented_error_text() {
    for (kind, status) in [
        ("rate_limit_error", 429),
        ("billing_error", 402),
        ("overloaded_error", 502),
    ] {
        let mut f = OfflineFixture::new(200.0);
        let token = f.proxy.register("stream-error", "item", "review").unwrap();
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
        assert!(text.contains("secret-upstream-diagnostics"));
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
        upstream_error: Some(("rate_limit_error".into(), "Too many requests".into())),
        no_usage: true,
    });
    tracing::subscriber::with_default(subscriber, || {
        report_refusal(
            &error,
            &RequestContext {
                method: "POST",
                path: "/v1/messages?beta=true",
                model: MODEL.into(),
                ..Default::default()
            },
            &mut Vec::new(),
            false,
        );
        let mut context = RequestContext::default();
        let error = field_denial(&mut context, "thinking", "thinking must be an object");
        report_refusal(&error, &context, &mut Vec::new(), false);
        for (field, value) in [
            (
                "unknown_option",
                serde_json::json!({"input":"private-content"}),
            ),
            ("model", serde_json::json!("private-content")),
            (
                "max_tokens",
                serde_json::json!({"prompt":"private-content"}),
            ),
            (
                "tools",
                serde_json::json!([{"input_schema":{"$ref":"private-content"}}]),
            ),
            ("thinking", serde_json::json!("private-content")),
            ("stream", serde_json::json!("private-content")),
        ] {
            let body = serde_json::json!({
                "model": MODEL, "max_tokens": 100,
                "messages": [{"role":"user", "content":"private-content"}],
                "system": "private-content", field: value
            })
            .to_string();
            let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer synthetic-token\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            let mut context = RequestContext::default();
            let error = request(raw.as_bytes(), MODEL, &mut context).err().unwrap();
            assert_eq!(context.offending_field.as_deref(), Some(field));
            report_refusal(&error, &context, &mut Vec::new(), false);
        }
    });
    let log = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    for field in [
        "POST",
        "/v1/messages?beta=true",
        MODEL,
        "upstream credit or rate limit",
        "rate_limit_error",
        "Too many requests",
        "429",
        "thinking must be an object",
        "offending_field=Some(\"thinking\")",
    ] {
        assert!(log.contains(field), "{log}");
    }
    assert!(!log.contains("sk-ant-") && !log.contains("x-api-key"));
    assert!(!log.contains("offending_json"));
    assert!(!log.contains("private-content"));
}

#[test]
fn field_diagnostics_never_copy_prompts_or_credentials() {
    for field in [
        "messages",
        "system",
        "tools",
        "metadata",
        "apiKey",
        "authorization",
        "proxy_token",
        "secret",
    ] {
        let mut context = RequestContext::default();
        field_denial(&mut context, field, "request field not allowlisted");
        assert_eq!(context.offending_field.as_deref(), Some(field));
    }
    let mut context = RequestContext::default();
    field_denial(&mut context, "max_tokens", "max_tokens not allowlisted");
    assert_eq!(context.offending_field.as_deref(), Some("max_tokens"));
}

#[test]
fn pi_message_and_count_tokens_routes_preserve_only_allowed_beta_headers() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("pi-shape", "item", "review").unwrap();
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
    let token = f.proxy.register("run-a", "item-a", "review").unwrap();
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
    let token = f.proxy.register("stream", "item", "review").unwrap();
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
    let token = f.proxy.register("r", "i", "review").unwrap();
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
    assert!(f.proxy.register("r", "i", "review").is_err());
    f.proxy.cap = 200.0;
    let token = f.proxy.register("r", "i", "review").unwrap();
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
    assert!(f.proxy.register("new", "item", "review").is_err());
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        ..Default::default()
    };
    let mut reopened = AnthropicProxy::open(&config, &f.dir.join("ledger.sqlite")).unwrap();
    assert!(reopened.register("new", "item", "review").is_err());
    let mut other = Fixture::new(200.0);
    other.proxy.kill();
    assert!(other.proxy.register("r", "i", "review").is_err());
}

#[test]
fn anthropic_proxy_unavailable_counter_and_incomplete_stream_refuse_more_spend() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run", "item", "review").unwrap();
    let other = f
        .proxy
        .register("other-run", "other-item", "review")
        .unwrap();
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
    let token = streaming
        .proxy
        .register("stream", "item", "review")
        .unwrap();
    assert!(streaming
        .proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
        )
        .is_err());
    assert!(streaming.proxy.register("next", "item", "review").is_err());
    // A stream that ends without settling is marked abandoned, not left looking
    // like a request that is still in flight.
    let pending: i64 = streaming
        .proxy
        .ledger
        .query_row("SELECT pending FROM anthropic_requests", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending, ABANDONED);
}

#[test]
fn anthropic_proxy_completed_spend_survives_restart_and_shared_keys_are_refused() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("r", "i", "review").unwrap();
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
    assert!(reopened.register("r2", "i", "review").is_err());
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
fn anthropic_proxy_refuses_public_socket_parents_and_schema_references() {
    let f = Fixture::new(200.0);
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(bind_socket(&f.dir.join("socket")).is_err());
    assert!(inline_schema(&serde_json::json!({"type":"object", "$ref":"#/large"})).is_err());
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
    let listener = LoopbackProxy::start_with(
        proxy,
        ledger,
        "capture",
        "item",
        "review",
        |_, req, out| {
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
        // Record configuration exactly, but never prompts, tools, metadata or
        // authentication. The names above cover every top-level field.
        for field in [
            "thinking",
            "output_config",
            "max_tokens",
            "stream",
            "temperature",
            "top_p",
            "top_k",
            "model",
        ] {
            if let Some(value) = data.get(field) {
                eprintln!("pi body configuration: {field}={value}");
            }
        }
        assert!(
            data["thinking"].is_object(),
            "medium thinking must be captured"
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
        "models":[{"id":MODEL,"reasoning":true,"contextWindow":200000,"maxTokens":8192}]
    }}}).to_string()).unwrap();
    let mut child = std::process::Command::new("pi")
        .args([
            "--mode",
            "rpc",
            "--model",
            &format!("offline/{MODEL}"),
            "--thinking",
            "medium",
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
    let token = f.proxy.register("socket-run", "item", "review").unwrap();
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

#[test]
fn documented_http_rejections_release_reservations_and_expose_only_error_fields() {
    for status in [400, 401, 402, 403, 404, 429, 500, 502, 529] {
        for streaming in [false, true] {
            let mut f = OfflineFixture::new(200.0);
            let token = f.proxy.register("fake-run", "item", "review").unwrap();
            let raw_request = String::from_utf8(Fixture::request(&token, streaming))
                .unwrap()
                .replace("/v1/messages HTTP", "/v1/messages?beta=true HTTP")
                .replace(
                    "X-Untrusted:",
                    "anthropic-beta: interleaved-thinking-2025-05-14\r\nX-Untrusted:",
                );
            let mut output = Vec::new();
            let error_body = serde_json::json!({"type":"error", "error":{
                "type":"invalid_request_error", "message":"Unsupported beta feature"},
                "request_id":"not-exposed", "extra":"not-exposed"})
            .to_string();
            let raw = format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\n\r\n{error_body}"
            );
            assert!(f
                .proxy
                .serve(raw_request.as_bytes(), &mut output, |key, request, out| {
                    let config = curl_config(key, request).unwrap();
                    assert!(config.contains("/v1/messages?beta=true"));
                    assert!(config.contains("anthropic-beta: interleaved-thinking-2025-05-14"));
                    forward_upstream(&mut BufReader::new(raw.as_bytes()), out)
                })
                .is_err());
            let text = String::from_utf8(output).unwrap();
            assert!(text.starts_with(&format!("HTTP/1.1 {status}")));
            assert!(
                text.contains("invalid_request_error") && text.contains("Unsupported beta feature")
            );
            assert!(
                !text.contains("not-exposed")
                    && !text.contains(&token)
                    && !text.contains("sk-ant-")
            );
            f.proxy.available(0.0).unwrap();
            let (cost, pending): (f64, i64) = f
                .proxy
                .ledger
                .query_row("SELECT cost,pending FROM anthropic_requests", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .unwrap();
            assert_eq!((cost, pending), (0.0, 0));
            f.proxy
                .serve(
                    Fixture::request(&token, false).as_slice(),
                    &mut Vec::new(),
                    |_, _, out| out.write_all(&Fixture::response()),
                )
                .unwrap();
        }
    }
}

#[test]
fn uncertain_transport_keeps_latch_until_owner_releases_the_entry() {
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run", "item", "review").unwrap();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut Vec::new(),
            |_, _, out| {
                out.write_all(b"data: ")?;
                Err(io::Error::other("transport lost"))
            }
        )
        .is_err());
    assert!(f.proxy.available(0.0).is_err());
    let path = f.dir.join("ledger.sqlite");
    let status = credit_status(&path).unwrap();
    assert_eq!(status["pending"], 1);
    assert_eq!(status["abandoned"], 1);
    assert_eq!(status["in_flight"], 0);
    assert_eq!(status["entries"][0]["state"], "abandoned");
    assert!(credit_release(&path, 999).is_err());
    credit_release(&path, 1).unwrap();
    assert!(credit_release(&path, 1).is_err());
    let status = credit_status(&path).unwrap();
    assert_eq!(status["pending"], 0);
    assert_eq!(status["abandoned"], 0);
    assert_eq!(status["entries"][0]["state"], "released");
    f.proxy.available(0.0).unwrap();
    // Accounted costs must never be erased by owner release.
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(&Fixture::response()),
        )
        .unwrap();
    assert!(credit_release(&path, 2).is_err());
    assert!(credit_status(&path).unwrap()["spent_usd"].as_f64().unwrap() > 0.0);
}

#[test]
fn offline_transport_loss_stays_closed_and_owner_release_is_selective() {
    let mut f = OfflineFixture::new(200.0);
    let token = f.proxy.register("run", "item", "review").unwrap();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, _| Err(io::Error::other("connection lost"))
        )
        .is_err());
    assert!(f.proxy.available(0.0).is_err());
    assert!(release_entry(&f.proxy.ledger, 99).is_err());
    release_entry(&f.proxy.ledger, 1).unwrap();
    assert!(release_entry(&f.proxy.ledger, 1).is_err());
    f.proxy.available(0.0).unwrap();
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, _, out| out.write_all(&Fixture::response()),
        )
        .unwrap();
    assert!(release_entry(&f.proxy.ledger, 2).is_err());
}

#[test]
fn http_rejection_without_valid_error_json_still_releases_reservation() {
    for body in [String::new(), "not JSON".into(), "x".repeat(65537)] {
        let mut f = OfflineFixture::new(200.0);
        let token = f.proxy.register("run", "item", "review").unwrap();
        let raw = format!("HTTP/1.1 503 Error\r\n\r\n{body}");
        let mut output = Vec::new();
        assert!(f
            .proxy
            .serve(
                Fixture::request(&token, true).as_slice(),
                &mut output,
                |_, _, out| forward_upstream(&mut BufReader::new(raw.as_bytes()), out)
            )
            .is_err());
        assert!(String::from_utf8(output).unwrap().contains("proxy_refusal"));
        f.proxy.available(0.0).unwrap();
    }
}

#[test]
fn tool_use_round_trip_with_a_validated_tool_choice_reaches_upstream() {
    let tools = serde_json::json!([{"name":"read_file", "input_schema":{"type":"object"}}]);
    let messages = serde_json::json!([
        {"role":"user", "content":"review"},
        {"role":"assistant", "content":[{"type":"tool_use", "id":"t1", "name":"read_file", "input":{"path":"a"}}]},
        {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"t1", "content":"x"}]}]);
    let admitted = [
        serde_json::json!({"type":"auto"}),
        serde_json::json!({"type":"none"}),
        serde_json::json!({"type":"any", "disable_parallel_tool_use":true}),
        serde_json::json!({"type":"tool", "name":"read_file"}),
    ];
    let refused = [
        serde_json::json!("auto"),
        serde_json::json!({"type":"bogus"}),
        serde_json::json!({"type":"none", "name":"read_file"}),
        serde_json::json!({"type":"tool", "name":"undeclared"}),
        serde_json::json!({"type":"tool"}),
        serde_json::json!({"type":"auto", "disable_parallel_tool_use":"yes"}),
        serde_json::json!({"type":"auto", "extra":1}),
    ];
    for (choice, ok) in admitted
        .into_iter()
        .map(|c| (c, true))
        .chain(refused.into_iter().map(|c| (c, false)))
    {
        let body = serde_json::json!({"model": MODEL, "max_tokens":4096, "messages":messages,
            "tools":tools, "tool_choice":choice})
        .to_string();
        let raw = format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer synthetic-token\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut context = RequestContext::default();
        let result = request(raw.as_bytes(), MODEL, &mut context);
        assert_eq!(result.is_ok(), ok, "{choice}");
        if !ok {
            assert_eq!(context.offending_field.as_deref(), Some("tool_choice"));
        }
    }
}

/// A complete streamed message: a started message, its final usage and the stop.
fn settled_stream() -> String {
    format!("event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"model\":\"{MODEL}\",\"usage\":{{\"input_tokens\":12,\"output_tokens\":1,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n\nevent: message_delta\ndata: {{\"type\":\"message_delta\",\"usage\":{{\"output_tokens\":30}}}}\n\nevent: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n")
}

/// The ledger row of one request: its pending state and its recorded owner.
fn ledger_row(path: &Path) -> (i64, Option<i64>, Option<i64>) {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row(
        "SELECT pending,owner_pid,owner_start FROM anthropic_requests",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

/// The reservation one row holds, or `None` once it is cleared.
fn ledger_reserved(path: &Path, entry: i64) -> Option<f64> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row(
        "SELECT reserved FROM anthropic_requests WHERE id=?1",
        params![entry],
        |row| row.get(0),
    )
    .unwrap()
}

fn insert_pending(f: &Fixture, owner: (Option<i64>, Option<i64>), reserved: f64) {
    f.proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,stage,model,pending,owner_pid,owner_start,reserved) \
             VALUES ('other','item','review',?1,?2,?3,?4,?5)",
            params![MODEL, IN_FLIGHT, owner.0, owner.1, reserved],
        )
        .unwrap();
}

/// A pending row that records no reservation at all, as one written before the
/// reservation column reads: NULL is unknown, never zero.
fn insert_pending_unreserved(f: &Fixture, owner: (Option<i64>, Option<i64>)) {
    f.proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,stage,model,pending,owner_pid,owner_start) \
             VALUES ('other','item','review',?1,?2,?3,?4)",
            params![MODEL, IN_FLIGHT, owner.0, owner.1],
        )
        .unwrap();
}

/// This process as a row's owner, so the row is in flight for its readers.
fn this_owner() -> (Option<i64>, Option<i64>) {
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    (Some(i64::from(pid)), Some(start as i64))
}

/// The refusal a failed step answers with: its HTTP status and its check.
fn refusal(error: &io::Error) -> Option<(u16, &'static str)> {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<Refusal>())
        .map(|refusal| (refusal.status, refusal.check))
}

#[test]
fn a_request_in_flight_records_its_owner_and_reads_as_in_flight() {
    let mut f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let token = f.proxy.register("run", "item", "review").unwrap();
    let sse = settled_stream();
    let mut streaming = None;
    f.proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut Vec::new(),
            |_, request, out| {
                // Admission wrote the row before any upstream byte: it names
                // this process, holds its worst-case reservation, and the
                // readers see a request in flight.
                streaming = Some((
                    request.reserve,
                    ledger_row(&ledger),
                    AnthropicProxy::accounting(&ledger, None).unwrap(),
                    credit_status(&ledger).unwrap(),
                ));
                out.write_all(sse.as_bytes())
            },
        )
        .unwrap();
    let (reserve, row, accounting, status) =
        streaming.expect("the row exists while the request streams");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    assert!(reserve > 0.0);
    assert_eq!(row, (IN_FLIGHT, Some(i64::from(pid)), Some(start as i64)));
    assert_eq!(
        accounting,
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: reserve,
            in_flight: 1,
            abandoned: 0,
        }
    );
    // The local inspection agrees, and a pending row is not spend.
    assert_eq!(status["entries"][0]["state"], "in flight");
    assert_eq!(status["in_flight"], 1);
    assert_eq!(status["abandoned"], 0);
    assert_eq!(status["spent_usd"], 0.0);
    assert_eq!(status["reserved_usd"].as_f64(), Some(reserve));
    // The settled row keeps the owner but no longer counts as pending.
    assert_eq!(
        ledger_row(&ledger),
        (SETTLED, Some(i64::from(pid)), Some(start as i64))
    );
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert!(accounting.spent_usd > 0.0 && accounting.pending() == 0);
    assert_eq!(accounting.reserved_usd, 0.0);
}

#[test]
fn admission_counts_settled_spend_and_holds_in_flight_reservations() {
    let mut f = Fixture::new(10.0);
    // Settled spend only: the reservation comparison uses settled spend plus
    // this request's worst case.
    f.proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,model,cost,pending) \
             VALUES ('prior','item',?1,4.0,?2)",
            params![MODEL, SETTLED],
        )
        .unwrap();
    // The cap may be reached exactly: 4.0 settled + a 6.0 reservation. A
    // request whose worst case would exceed it is refused.
    f.proxy.available(6.0).unwrap();
    let error = f.proxy.available(6.01).expect_err("over the cap");
    assert_eq!(
        refusal(&error),
        Some((402, "credit cap or reservation limit"))
    );
    // One request in flight holds its reservation, and a request that still
    // fits beside it is admitted instead of refused.
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 3.0);
    f.proxy.available(3.0).unwrap();
    let error = f.proxy.available(3.01).expect_err("over the cap");
    assert_eq!(
        refusal(&error),
        Some((402, "credit cap or reservation limit"))
    );
    // A settled spend at the cap refuses even a request that reserves nothing:
    // this comparison is the settled spend alone, at `>= cap`, because nothing
    // admitted afterwards could stay within the cap.
    f.proxy
        .ledger
        .execute(
            "UPDATE anthropic_requests SET cost=10.0 WHERE pending=0",
            [],
        )
        .unwrap();
    let error = f.proxy.available(0.0).expect_err("the cap is reached");
    assert_eq!(
        refusal(&error),
        Some((402, "credit cap or reservation limit"))
    );
    // The kill switch closes admission whatever the ledger holds.
    f.proxy.kill();
    let error = f.proxy.available(0.0).expect_err("the kill switch is on");
    assert_eq!(refusal(&error), Some((403, "kill switch")));
}

#[test]
fn an_abandoned_row_admits_nothing_until_its_owner_releases_it() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    // An owner that is gone: the row's spend is unknown, so its reservation
    // cannot make a new request safe.
    insert_pending(&f, (Some(0), Some(0)), 1.0);
    let error = f
        .proxy
        .available(0.0)
        .expect_err("an abandoned row admits nothing");
    assert_eq!(refusal(&error), Some((402, "abandoned pending request")));
    // It keeps its reservation recorded but reserves nothing: the abandoned
    // state itself is what closes admission.
    assert_eq!(ledger_reserved(&ledger, 1), Some(1.0));
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    assert_eq!(credit_status(&ledger).unwrap()["reserved_usd"], 0.0);
    // Only the owner's release clears it.
    credit_release(&ledger, 1).unwrap();
    assert_eq!(ledger_reserved(&ledger, 1), None);
    f.proxy.available(0.0).unwrap();
}

#[test]
fn a_pending_row_without_a_recorded_reservation_admits_nothing() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    // A live request whose row records no reservation: its worst case is
    // unknown, so the ledger cannot hold it back and nothing may be admitted
    // beside it, whatever a new request reserves.
    insert_pending_unreserved(&f, this_owner());
    for reserve in [0.0, 1.0] {
        let error = f
            .proxy
            .available(reserve)
            .expect_err("an unknown reservation admits nothing");
        assert_eq!(
            refusal(&error),
            Some((402, "pending request without a reservation"))
        );
    }
    // It is still a request in flight, not an abandoned one, and the totals
    // cannot know what it holds back.
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 1,
            abandoned: 0,
        }
    );
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["state"], "in flight");
    assert_eq!(status["entries"][0]["reserved_usd"], Value::Null);
    assert_eq!(status["unreserved"], 1);
    // Its settle clears the unknown reservation beside the cost.
    f.proxy
        .ledger
        .execute(
            "UPDATE anthropic_requests SET cost=0.25,pending=?1,reserved=NULL WHERE id=1",
            params![SETTLED],
        )
        .unwrap();
    f.proxy.available(0.0).unwrap();
    // So does the owner's release of such a row.
    insert_pending_unreserved(&f, this_owner());
    let error = f
        .proxy
        .available(0.0)
        .expect_err("the unknown reservation is back");
    assert_eq!(
        refusal(&error),
        Some((402, "pending request without a reservation"))
    );
    credit_release(&ledger, 2).unwrap();
    f.proxy.available(0.0).unwrap();
    assert_eq!(credit_status(&ledger).unwrap()["unreserved"], 0);
}

#[test]
fn a_crashed_row_keeps_reserving_until_its_owner_is_gone_then_closes_admission() {
    // While the owner of an in-flight row runs, the row holds its reservation:
    // a request that would cross the cap with it is refused.
    let f = Fixture::new(10.0);
    let ledger = f.dir.join("ledger.sqlite");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 6.0);
    let error = f.proxy.available(4.5).expect_err("the reservation is held");
    assert_eq!(
        refusal(&error),
        Some((402, "credit cap or reservation limit"))
    );
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 6.0,
            in_flight: 1,
            abandoned: 0,
        }
    );
    // Once its owner is provably gone the row is abandoned: it holds nothing
    // any more, and the abandoned state closes admission instead.
    let gone = AnthropicProxy::accounting_with(&ledger, None, |_, _| false).unwrap();
    assert_eq!(
        gone,
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    drop(f);
    let f = Fixture::new(10.0);
    insert_pending(&f, (Some(0), Some(0)), 6.0);
    let error = f
        .proxy
        .available(0.0)
        .expect_err("an abandoned row admits nothing");
    assert_eq!(refusal(&error), Some((402, "abandoned pending request")));
}

#[test]
fn settling_or_releasing_a_row_frees_its_reservation() {
    let mut f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let token = f.proxy.register("run", "item", "review").unwrap();
    let mut during = None;
    f.proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, request, out| {
                during = Some((request.reserve, ledger_reserved(&ledger, 1)));
                out.write_all(&Fixture::response())
            },
        )
        .unwrap();
    let (reserve, held) = during.expect("the row exists while the request runs");
    assert!(reserve > 0.0);
    // Admission wrote the request's own worst case with its row...
    assert_eq!(held, Some(reserve));
    // ...and the settle cleared it beside the cost, so it reserves nothing.
    assert_eq!(ledger_reserved(&ledger, 1), None);
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert!(accounting.spent_usd > 0.0);
    assert_eq!((accounting.reserved_usd, accounting.in_flight), (0.0, 0));
    // A released row frees its reservation too.
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 1.5);
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None)
            .unwrap()
            .reserved_usd,
        1.5
    );
    credit_release(&ledger, 2).unwrap();
    assert_eq!(ledger_reserved(&ledger, 2), None);
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert_eq!((accounting.reserved_usd, accounting.in_flight), (0.0, 0));
    f.proxy.available(100.0).unwrap();
}

#[test]
fn two_admissions_racing_for_the_last_reservation_gap_admit_exactly_one() {
    // 4.0 settled leaves a 6.0 gap under a 10.0 cap: exactly one of two 6.0
    // reservations fits. Both threads wait on the barrier and then reserve, so
    // the two attempts start together instead of one finishing before the
    // other begins, and the round repeats so the serialization is exercised
    // however the threads are scheduled. A check-then-insert that read one
    // snapshot twice would admit both in some round; the ledger's own
    // `BEGIN IMMEDIATE` admits only one.
    for round in 0..25 {
        let f = Fixture::new(10.0);
        let ledger = f.dir.join("ledger.sqlite");
        f.proxy
            .ledger
            .execute(
                "INSERT INTO anthropic_requests(run,item,model,cost,pending) \
                 VALUES ('prior','item',?1,4.0,?2)",
                params![MODEL, SETTLED],
            )
            .unwrap();
        let config = crate::config::WorkersConfig {
            anthropic_api_key_file: f.dir.join("key").display().to_string(),
            anthropic_api_cap_usd: 10.0,
            ..Default::default()
        };
        let proxies = [
            AnthropicProxy::open(&config, &ledger).unwrap(),
            AnthropicProxy::open(&config, &ledger).unwrap(),
        ];
        let barrier = std::sync::Barrier::new(proxies.len());
        let results = std::thread::scope(|scope| {
            let handles = proxies.map(|proxy| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    proxy.reserve("run", "item", CreditStage::Review, 6.0)
                })
            });
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let mut admitted = 0;
        let mut refused = 0;
        for result in results {
            match result {
                Ok(_) => admitted += 1,
                Err(error) => {
                    assert_eq!(
                        refusal(&error),
                        Some((402, "credit cap or reservation limit")),
                        "round {round}"
                    );
                    refused += 1;
                }
            }
        }
        assert_eq!((admitted, refused), (1, 1), "round {round}");
        // Exactly one reservation is recorded beside the settled spend, so the
        // cap cannot be overshot.
        assert_eq!(
            AnthropicProxy::accounting(&ledger, None).unwrap(),
            CreditAccounting {
                spent_usd: 4.0,
                reserved_usd: 6.0,
                in_flight: 1,
                abandoned: 0,
            },
            "round {round}"
        );
        let rows: i64 = Connection::open(&ledger)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM anthropic_requests", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 2, "round {round}");
    }
}

#[test]
fn an_admission_refusal_body_names_the_reason() {
    // The cap: a request whose worst-case reservation crosses it.
    let mut f = Fixture::new(0.001);
    let token = f.proxy.register("run", "item", "review").unwrap();
    let mut output = Vec::new();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut output,
            |_, _, _| panic!("a cap refusal must deny before upstream")
        )
        .is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("HTTP/1.1 402"), "{text}");
    assert!(
        text.contains("\"message\":\"credit cap or reservation limit\""),
        "{text}"
    );

    // An abandoned row.
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run", "item", "review").unwrap();
    insert_pending(&f, (Some(0), Some(0)), 1.0);
    let mut output = Vec::new();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut output,
            |_, _, _| panic!("an abandoned row must deny before upstream")
        )
        .is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("HTTP/1.1 402"), "{text}");
    assert!(
        text.contains("\"message\":\"abandoned pending request\""),
        "{text}"
    );

    // A request in flight whose worst case the ledger does not record.
    let mut f = Fixture::new(200.0);
    let token = f.proxy.register("run", "item", "review").unwrap();
    insert_pending_unreserved(&f, this_owner());
    let mut output = Vec::new();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut output,
            |_, _, _| panic!("an unknown reservation must deny before upstream")
        )
        .is_err());
    let text = String::from_utf8(output).unwrap();
    assert!(text.starts_with("HTTP/1.1 402"), "{text}");
    assert!(
        text.contains("\"message\":\"pending request without a reservation\""),
        "{text}"
    );
}

#[test]
fn credit_status_and_accounting_report_the_reserved_amount() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    f.proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,model,cost,pending) \
             VALUES ('prior','item',?1,2.0,?2)",
            params![MODEL, SETTLED],
        )
        .unwrap();
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 1.5);
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 2.0,
            reserved_usd: 1.5,
            in_flight: 1,
            abandoned: 0,
        }
    );
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["spent_usd"], 2.0);
    assert_eq!(status["reserved_usd"], 1.5);
    assert_eq!(status["unreserved"], 0);
    assert_eq!(status["in_flight"], 1);
    assert_eq!(status["entries"][0]["reserved_usd"], Value::Null);
    assert_eq!(status["entries"][1]["reserved_usd"], 1.5);
}

#[test]
fn an_owner_that_is_gone_or_was_reused_is_never_proven_in_flight() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    assert!(!Owner { pid: 0, start: 0 }.is_live(&ledger, 1));
    // The pid of a live process, reused by a later one: another start token.
    assert!(!Owner {
        pid,
        start: start + 1,
    }
    .is_live(&ledger, 1));
    assert!(Owner { pid, start }.is_live(&ledger, 1));

    // A reader that cannot prove the owner live counts the row as abandoned,
    // and one that can counts it as in flight: the split is the reader's.
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 0.0);
    let gone = AnthropicProxy::accounting_with(&ledger, None, |_, _| false).unwrap();
    assert_eq!(
        gone,
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    let running = AnthropicProxy::accounting_with(&ledger, None, |_, _| true).unwrap();
    assert_eq!((running.in_flight, running.abandoned), (1, 0));
}

#[test]
fn a_row_without_a_recorded_owner_is_abandoned() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    insert_pending(&f, (None, None), 2.0);
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert_eq!(
        accounting,
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    // A reader that would call any owner live still cannot prove this one.
    let proven = AnthropicProxy::accounting_with(&ledger, None, |_, _| true).unwrap();
    assert_eq!((proven.in_flight, proven.abandoned), (0, 1));
    // The operator sees the split, and the owner can still release the row.
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["state"], "abandoned");
    assert_eq!(status["abandoned"], 1);
    credit_release(&ledger, 1).unwrap();
    assert_eq!(credit_status(&ledger).unwrap()["pending"], 0);
}

#[test]
fn a_ledger_from_before_the_owner_columns_reads_as_abandoned_and_migrates_once() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("old-ledger.sqlite");
    let db = Connection::open(&ledger).unwrap();
    db.execute_batch(
        "CREATE TABLE anthropic_requests (
          id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
          model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
          cache_write INTEGER, cost REAL, pending INTEGER NOT NULL);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO anthropic_requests(run,item,model,cost,pending) VALUES ('prior','item',?1,0.5,1)",
        params![MODEL],
    )
    .unwrap();
    drop(db);
    // Before the migration the reader cannot prove the row in flight either.
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert_eq!(
        accounting,
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["state"], "abandoned");
    // The row still carries the cost an older herdr wrote onto it, which the
    // inspection must not count as spend: its total is the settled spend the
    // accounting reader reports, a pending row has none.
    assert_eq!(status["entries"][0]["cost_usd"], Value::Null);
    assert_eq!(status["spent_usd"].as_f64(), Some(accounting.spent_usd));
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        anthropic_api_cap_usd: 200.0,
        ..Default::default()
    };
    // Opening the cycle's ledger migrates it: the old pending row gets the
    // abandoned state, so nothing has to guess its owner.
    let proxy = AnthropicProxy::open(&config, &ledger).unwrap();
    assert_eq!(ledger_row(&ledger), (ABANDONED, None, None));
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap().abandoned,
        1
    );
    // A later open of the migrated ledger never relabels a live row.
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,model,pending,owner_pid,owner_start) \
             VALUES ('now','item',?1,?2,?3,?4)",
            params![MODEL, IN_FLIGHT, pid, start as i64],
        )
        .unwrap();
    drop(proxy);
    let reopened = AnthropicProxy::open(&config, &ledger).unwrap();
    let rows: Vec<(i64, Option<i64>)> = reopened
        .ledger
        .prepare("SELECT pending,owner_pid FROM anthropic_requests ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            (ABANDONED, None),
            (IN_FLIGHT, Some(i64::from(std::process::id())))
        ]
    );
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert_eq!((accounting.in_flight, accounting.abandoned), (1, 1));
}

#[test]
fn a_row_whose_reservation_the_ledger_does_not_record_admits_nothing_until_it_settles() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("pre-reservation.sqlite");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    let db = Connection::open(&ledger).unwrap();
    // The previous schema: the owner columns, but no reservation column.
    db.execute_batch(
        "CREATE TABLE anthropic_requests (
          id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
          model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
          cache_write INTEGER, cost REAL, pending INTEGER NOT NULL,
          owner_pid INTEGER, owner_start INTEGER);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    db.execute(
        "INSERT INTO anthropic_requests(run,item,model,cost,pending) \
         VALUES ('prior','item',?1,1.0,?2)",
        params![MODEL, SETTLED],
    )
    .unwrap();
    db.execute(
        "INSERT INTO anthropic_requests(run,item,model,pending,owner_pid,owner_start) \
         VALUES ('now','item',?1,?2,?3,?4)",
        params![MODEL, IN_FLIGHT, pid, start as i64],
    )
    .unwrap();
    drop(db);
    // A row written before the column holds no reservation, and it still reads
    // as a request in flight whose reservation the reader cannot know.
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 1.0,
            reserved_usd: 0.0,
            in_flight: 1,
            abandoned: 0,
        }
    );
    let stale = credit_status(&ledger).unwrap();
    assert_eq!(stale["reserved_usd"], 0.0);
    assert_eq!(stale["unreserved"], 1);
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        anthropic_api_cap_usd: 200.0,
        ..Default::default()
    };
    let mut proxy = AnthropicProxy::open(&config, &ledger).unwrap();
    let version: i64 = proxy
        .ledger
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    // The migration adds the column and never invents a reservation for the
    // older in-flight row: unknown stays unknown, so it admits nothing.
    let older: (i64, Option<f64>) = proxy
        .ledger
        .query_row(
            "SELECT pending,reserved FROM anthropic_requests WHERE id=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(older, (IN_FLIGHT, None));
    assert_eq!(ledger_reserved(&ledger, 2), None);
    let error = proxy
        .available(0.0)
        .expect_err("an unknown reservation admits nothing");
    assert_eq!(
        refusal(&error),
        Some((402, "pending request without a reservation"))
    );
    // Its settle clears the unknown reservation beside its cost, and admission
    // opens again.
    proxy
        .ledger
        .execute(
            "UPDATE anthropic_requests SET cost=0.5,pending=?1,reserved=NULL WHERE id=2",
            params![SETTLED],
        )
        .unwrap();
    proxy.available(0.0).unwrap();
    assert_eq!(credit_status(&ledger).unwrap()["unreserved"], 0);
    // A request written after the migration still records its own reservation
    // and clears it on settlement.
    let token = proxy.register("run", "item", "review").unwrap();
    let mut during = None;
    proxy
        .serve(
            Fixture::request(&token, false).as_slice(),
            &mut Vec::new(),
            |_, request, out| {
                during = Some((request.reserve, ledger_reserved(&ledger, 3)));
                out.write_all(&Fixture::response())
            },
        )
        .unwrap();
    let (reserve, held) = during.expect("the new request ran");
    assert!(reserve > 0.0);
    assert_eq!(held, Some(reserve));
    assert_eq!(ledger_reserved(&ledger, 3), None);
    let accounting = AnthropicProxy::accounting(&ledger, None).unwrap();
    assert!(accounting.spent_usd > 1.5);
    assert_eq!((accounting.reserved_usd, accounting.in_flight), (0.0, 0));
}

#[test]
fn a_release_on_a_ledger_from_before_the_reservation_column_migrates_and_clears_it() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("pre-reservation-release.sqlite");
    let db = Connection::open(&ledger).unwrap();
    db.execute_batch(
        "CREATE TABLE anthropic_requests (
          id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
          model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
          cache_write INTEGER, cost REAL, pending INTEGER NOT NULL,
          owner_pid INTEGER, owner_start INTEGER);
         PRAGMA user_version = 1;",
    )
    .unwrap();
    db.execute(
        "INSERT INTO anthropic_requests(run,item,model,pending) VALUES ('prior','item',?1,?2)",
        params![MODEL, IN_FLIGHT],
    )
    .unwrap();
    drop(db);
    // The owner's release opens and migrates the ledger, so a row written
    // before the reservation column can still be cleared.
    credit_release(&ledger, 1).unwrap();
    assert_eq!(credit_status(&ledger).unwrap()["pending"], 0);
    assert_eq!(ledger_reserved(&ledger, 1), None);
    let version: i64 = Connection::open(&ledger)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn an_abandonment_the_ledger_cannot_record_is_kept_closed_in_memory() {
    let mut f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    // A ledger that refuses the abandonment write, as a locked or read-only
    // ledger does: the request must still stop reading as in flight.
    Connection::open(&ledger)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_abandonment BEFORE UPDATE OF pending \
             ON anthropic_requests WHEN NEW.pending=2 \
             BEGIN SELECT RAISE(ABORT,'ledger refuses the write'); END;",
        )
        .unwrap();
    let token = f.proxy.register("run", "item", "review").unwrap();
    assert!(f
        .proxy
        .serve(
            Fixture::request(&token, true).as_slice(),
            &mut Vec::new(),
            |_, _, out| {
                out.write_all(b"data: ")?;
                Err(io::Error::other("transport lost"))
            }
        )
        .is_err());
    // The write failed, so the ledger still holds the row in flight...
    assert_eq!(ledger_row(&ledger).0, IN_FLIGHT);
    // ...and neither reader in this process proves it in flight again.
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 0.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 1,
        }
    );
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["state"], "abandoned");
    assert_eq!(status["in_flight"], 0);
    assert_eq!(status["abandoned"], 1);
    // Fail-closed admission is unchanged: the row still admits nothing.
    let error = f
        .proxy
        .available(0.0)
        .expect_err("an abandoned row admits nothing");
    assert_eq!(refusal(&error), Some((402, "abandoned pending request")));
}

#[test]
fn one_snapshot_counts_a_request_that_settles_between_the_reads() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let pid = std::process::id();
    let start = crate::platform::process_start_token(pid).unwrap();
    // One settled charge, and a request that streams when the reads start.
    f.proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,model,cost,pending) \
             VALUES ('prior','item',?1,11.0,?2)",
            params![MODEL, SETTLED],
        )
        .unwrap();
    insert_pending(&f, (Some(i64::from(pid)), Some(start as i64)), 5.0);
    // A write-ahead log lets the second connection commit inside the reader's
    // snapshot; in rollback-journal mode a writer waits for that read instead,
    // so the interleaving cannot happen there in the first place.
    let mode: String = f
        .proxy
        .ledger
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let writer = Connection::open(&ledger).unwrap();
    let accounting = AnthropicProxy::accounting_between(
        &ledger,
        None,
        |entry, owner| owner.is_live(&ledger, entry),
        move || {
            // Between the two reads, the streaming request settles: its cost
            // lands and its reservation is cleared in one UPDATE.
            writer
                .execute(
                    "UPDATE anthropic_requests SET cost=5.0,pending=?1,reserved=NULL WHERE id=2",
                    params![SETTLED],
                )
                .unwrap();
        },
    )
    .unwrap();
    // Neither its charge nor its reservation is in what the snapshot's first
    // read saw, so the request is still one of its pending rows with the
    // reservation it held: no figure lost it, and the cycle's spend is never
    // understated while the accounting reads complete.
    assert_eq!(
        accounting,
        CreditAccounting {
            spent_usd: 11.0,
            reserved_usd: 5.0,
            in_flight: 1,
            abandoned: 0,
        }
    );
    assert_eq!(accounting.pending(), 1);
    assert_eq!(accounting.spent_usd + accounting.reserved_usd, 16.0);
    // The settle landed: a later read is a coherent later snapshot that moves
    // the reservation into the settled spend.
    assert_eq!(
        AnthropicProxy::accounting(&ledger, None).unwrap(),
        CreditAccounting {
            spent_usd: 16.0,
            reserved_usd: 0.0,
            in_flight: 0,
            abandoned: 0,
        }
    );
}

/// The stage column's newest value in one ledger, as its row recorded it.
fn newest_stage(path: &Path) -> Option<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    db.query_row(
        "SELECT stage FROM anthropic_requests ORDER BY id DESC LIMIT 1",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[test]
fn registration_refuses_a_stage_this_build_cannot_name() {
    let mut f = OfflineFixture::new(200.0);
    // A caller that cannot name its stage is a coding error, refused before
    // any grant or row exists: `unknown` would look like an older build's row.
    for stage in ["", "escalation", "Draft", " review"] {
        let error = f.proxy.register("run", "item", stage).unwrap_err();
        assert_eq!(
            refusal(&error),
            Some((403, "credit stage not named")),
            "{stage:?}"
        );
    }
    assert!(f.proxy.grants.is_empty());
    assert!(f.proxy.register("run", "item", "review").is_ok());
}

#[test]
fn every_stage_a_caller_states_is_recorded_with_its_row() {
    let mut f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    for stage in CreditStage::ALL {
        let token = f.proxy.register("run", "item", stage.as_str()).unwrap();
        f.proxy
            .serve(
                Fixture::request(&token, false).as_slice(),
                &mut Vec::new(),
                |_, _, out| out.write_all(&Fixture::response()),
            )
            .unwrap();
        assert_eq!(newest_stage(&ledger).as_deref(), Some(stage.as_str()));
    }
    let stages: Vec<String> = credit_status(&ledger).unwrap()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["stage"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        stages,
        ["draft", "worker", "review", "answer"].map(str::to_owned)
    );
}

#[test]
fn a_ledger_from_before_the_stage_column_reads_unknown_and_migrates_once() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("pre-stage.sqlite");
    let db = Connection::open(&ledger).unwrap();
    // The schema before the stage column: owners and reservations, no stage.
    db.execute_batch(
        "CREATE TABLE anthropic_requests (
          id INTEGER PRIMARY KEY, run TEXT NOT NULL, item TEXT NOT NULL,
          model TEXT NOT NULL, input INTEGER, output INTEGER, cache_read INTEGER,
          cache_write INTEGER, cost REAL, pending INTEGER NOT NULL,
          owner_pid INTEGER, owner_start INTEGER, reserved REAL);
         PRAGMA user_version = 2;",
    )
    .unwrap();
    db.execute(
        "INSERT INTO anthropic_requests(run,item,model,input,cost,pending) \
         VALUES ('prior','item',?1,1,0.25,?2)",
        params![MODEL, SETTLED],
    )
    .unwrap();
    drop(db);
    // Before the migration the row is read as it is: its settled spend counts,
    // and the stage it never recorded reads as unknown, never as a guess.
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["stage"], "unknown");
    assert_eq!(status["by_stage"][4]["stage"], "unknown");
    assert_eq!(status["by_stage"][4]["spent_usd"].as_f64(), Some(0.25));
    let config = crate::config::WorkersConfig {
        anthropic_api_key_file: f.dir.join("key").display().to_string(),
        anthropic_api_cap_usd: 200.0,
        ..Default::default()
    };
    let proxy = AnthropicProxy::open(&config, &ledger).unwrap();
    let version: i64 = proxy
        .ledger
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    // A row written now carries the stage its caller stated.
    proxy
        .ledger
        .execute(
            "INSERT INTO anthropic_requests(run,item,stage,model,input,cost,pending) \
             VALUES ('now','item','review',?1,1,0.5,?2)",
            params![MODEL, SETTLED],
        )
        .unwrap();
    drop(proxy);
    // A second open does not migrate again: the old row keeps reading as
    // unknown and the new one keeps its stage.
    let reopened = AnthropicProxy::open(&config, &ledger).unwrap();
    let version: i64 = reopened
        .ledger
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let rows: Vec<Option<String>> = reopened
        .ledger
        .prepare("SELECT stage FROM anthropic_requests ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows, [None, Some("review".to_owned())]);
    drop(reopened);
    let status = credit_status(&ledger).unwrap();
    assert_eq!(status["entries"][0]["stage"], "unknown");
    assert_eq!(status["entries"][1]["stage"], "review");
    assert_eq!(status["spent_usd"].as_f64(), Some(0.75));
}

#[test]
fn credit_status_sums_spend_per_item_and_stage() {
    let f = Fixture::new(200.0);
    let ledger = f.dir.join("ledger.sqlite");
    let insert = |run: &str,
                  item: &str,
                  stage: Option<&str>,
                  cost: Option<f64>,
                  pending: i64,
                  input: Option<i64>,
                  owner: (Option<i64>, Option<i64>),
                  reserved: Option<f64>| {
        f.proxy
            .ledger
            .execute(
                "INSERT INTO anthropic_requests(run,item,stage,model,input,cost,pending,\
                 owner_pid,owner_start,reserved) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![run, item, stage, MODEL, input, cost, pending, owner.0, owner.1, reserved],
            )
            .unwrap();
    };
    // Settled draft and review calls of one item, a settled row an older build
    // wrote (no stage) for another, an answer call still streaming from this
    // process and a review row whose stream ended without settling.
    insert(
        "run-1",
        "item-a",
        Some("draft"),
        Some(1.5),
        SETTLED,
        Some(10),
        (None, None),
        None,
    );
    insert(
        "run-1",
        "item-a",
        Some("review"),
        Some(0.5),
        SETTLED,
        Some(10),
        (None, None),
        None,
    );
    insert(
        "run-1",
        "item-b",
        None,
        Some(0.25),
        SETTLED,
        Some(10),
        (None, None),
        None,
    );
    insert(
        "run-2",
        "item-b",
        Some("answer"),
        None,
        IN_FLIGHT,
        None,
        this_owner(),
        Some(0.4),
    );
    insert(
        "run-2",
        "item-b",
        Some("review"),
        None,
        ABANDONED,
        None,
        this_owner(),
        Some(0.4),
    );

    let status = credit_status(&ledger).unwrap();
    let sums = |key: &str, name: &str| -> Vec<(String, f64, usize, usize)> {
        status[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|sum| {
                (
                    sum[name].as_str().unwrap().to_owned(),
                    sum["spent_usd"].as_f64().unwrap(),
                    sum["in_flight"].as_u64().unwrap() as usize,
                    sum["abandoned"].as_u64().unwrap() as usize,
                )
            })
            .collect()
    };
    assert_eq!(
        sums("by_item", "item"),
        [
            ("item-a".to_owned(), 2.0, 0, 0),
            ("item-b".to_owned(), 0.25, 1, 1)
        ]
    );
    assert_eq!(
        sums("by_stage", "stage"),
        [
            ("draft".to_owned(), 1.5, 0, 0),
            ("worker".to_owned(), 0.0, 0, 0),
            ("review".to_owned(), 0.5, 0, 1),
            ("answer".to_owned(), 0.0, 1, 0),
            ("unknown".to_owned(), 0.25, 0, 0),
        ]
    );
    // The totals the earlier change prints stay the same figures.
    assert_eq!(status["spent_usd"].as_f64(), Some(2.25));
    assert_eq!(status["pending"], 2);
    assert_eq!(status["in_flight"], 1);
    assert_eq!(status["abandoned"], 1);
    assert_eq!(status["entries"][2]["stage"], "unknown");
}
