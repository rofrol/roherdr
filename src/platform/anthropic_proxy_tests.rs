use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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
        format!("POST /v1/messages HTTP/1.1\r\nAuthorization: Bearer {token}\r\nx-api-key: untrusted\r\nHost: evil.invalid\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
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
    assert!(request(bad.as_bytes()).is_err());
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
    let (_, normalized, _, _) = request(raw.as_bytes()).unwrap();
    let text = String::from_utf8(normalized).unwrap();
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
