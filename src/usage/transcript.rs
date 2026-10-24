//! One response's token usage, read from one line of the transcript an agent writes
//! (Claude Code, Codex, pi). Shared by `herdr usage --workspace` (`super::estimate`), which
//! reads whole transcripts on demand, and the server's persistent ledger (`super::ledger`),
//! which reads only what was appended since its cursor.

use serde_json::Value;

use super::prices::ModelPrice;

/// The agents whose transcripts herdr reads for usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reader {
    Claude,
    Codex,
    Pi,
}

pub(crate) fn reader_for(agent: &str) -> Option<Reader> {
    match agent {
        "claude" => Some(Reader::Claude),
        "codex" => Some(Reader::Codex),
        "pi" | "omp" => Some(Reader::Pi),
        _ => None,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tokens {
    /// Input tokens neither written to nor read from the prompt cache.
    pub(crate) input: u64,
    pub(crate) output: u64,
    pub(crate) cache_write_5m: u64,
    pub(crate) cache_write_1h: u64,
    pub(crate) cache_read: u64,
}

impl Tokens {
    pub(crate) fn cost(&self, price: &ModelPrice) -> f64 {
        (self.input as f64 * price.input
            + self.output as f64 * price.output
            + self.cache_write_5m as f64 * price.cache_write_5m
            + self.cache_write_1h as f64 * price.cache_write_1h
            + self.cache_read as f64 * price.cache_read)
            / 1_000_000.0
    }

    pub(crate) fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.input == 0 && self.output == 0 && self.cache_write() == 0 && self.cache_read == 0
    }
}

/// The usage one transcript line records.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LineUsage {
    pub(crate) model: String,
    /// Identifies the response across lines and transcripts, where the agent names it:
    /// Claude writes a line per content block of a response, each with the whole usage, and
    /// a resumed session copies earlier responses.
    pub(crate) response_key: Option<String>,
    /// The line's RFC 3339 `timestamp`, if any.
    pub(crate) timestamp: Option<String>,
    pub(crate) tokens: Tokens,
    /// The cost the agent computed itself (pi).
    pub(crate) reported_cost: Option<f64>,
}

fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn timestamp(line: &Value) -> Option<String> {
    line.get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A Claude assistant line with its response's usage. Synthetic messages hold none.
pub(crate) fn claude_line(line: &Value) -> Option<LineUsage> {
    if line.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = line.get("message")?;
    let usage = message.get("usage")?;
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if model == "<synthetic>" {
        return None;
    }
    let message_id = message.get("id").and_then(Value::as_str);
    let request_id = line.get("requestId").and_then(Value::as_str);
    let response_key = match (message_id, request_id) {
        (None, None) => line
            .get("uuid")
            .and_then(Value::as_str)
            .map(|uuid| format!("uuid:{uuid}")),
        (message_id, request_id) => Some(format!(
            "{}:{}",
            message_id.unwrap_or(""),
            request_id.unwrap_or("")
        )),
    };
    let cache_write = number(usage, "cache_creation_input_tokens");
    let (write_5m, write_1h) = match usage.get("cache_creation") {
        Some(split) => {
            let write_1h = number(split, "ephemeral_1h_input_tokens").min(cache_write);
            (cache_write - write_1h, write_1h)
        }
        None => (cache_write, 0),
    };
    Some(LineUsage {
        model: model.to_string(),
        response_key,
        timestamp: timestamp(line),
        tokens: Tokens {
            input: number(usage, "input_tokens"),
            output: number(usage, "output_tokens"),
            cache_write_5m: write_5m,
            cache_write_1h: write_1h,
            cache_read: number(usage, "cache_read_input_tokens"),
        },
        reported_cost: None,
    })
}

/// A pi assistant message with its usage and the cost pi computed. pi names an entry by its
/// `id` within its session only.
pub(crate) fn pi_line(line: &Value) -> Option<LineUsage> {
    if line.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = line.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let usage = message.get("usage")?;
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(LineUsage {
        model: model.to_string(),
        response_key: line
            .get("id")
            .and_then(Value::as_str)
            .map(|id| format!("entry:{id}")),
        timestamp: timestamp(line),
        tokens: Tokens {
            input: number(usage, "input"),
            output: number(usage, "output"),
            cache_write_5m: number(usage, "cacheWrite"),
            cache_write_1h: 0,
            cache_read: number(usage, "cacheRead"),
        },
        reported_cost: usage
            .get("cost")
            .and_then(|cost| cost.get("total"))
            .and_then(Value::as_f64),
    })
}

/// Codex writes the session's running total with each `token_count` event; each event
/// counts the growth since the previous one, under the model of the turn it belongs to. The
/// running state is what a reader that stops mid-file must keep to go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexTotals {
    pub(crate) model: String,
    /// Input, cached input, cache-written input and output, as last reported.
    pub(crate) previous: [u64; 4],
}

impl Default for CodexTotals {
    fn default() -> Self {
        Self {
            model: "codex (model unknown)".into(),
            previous: [0; 4],
        }
    }
}

impl CodexTotals {
    /// The growth one line reports, or `None` for a line that reports none.
    pub(crate) fn line(&mut self, line: &Value) -> Option<LineUsage> {
        let payload = line.get("payload")?;
        if line.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(name) = payload.get("model").and_then(Value::as_str) {
                self.model = name.to_string();
            }
            return None;
        }
        if payload.get("type").and_then(Value::as_str) != Some("token_count") {
            return None;
        }
        let total = payload
            .get("info")
            .and_then(|info| info.get("total_token_usage"))?;
        let current = [
            number(total, "input_tokens"),
            number(total, "cached_input_tokens"),
            number(total, "cache_write_input_tokens"),
            number(total, "output_tokens"),
        ];
        // A smaller total starts a new count (a new process on the file).
        let restarted = current
            .iter()
            .zip(self.previous)
            .any(|(now, before)| *now < before);
        let delta: Vec<u64> = if restarted {
            current.to_vec()
        } else {
            current
                .iter()
                .zip(self.previous)
                .map(|(now, before)| now - before)
                .collect()
        };
        self.previous = current;
        if delta.iter().all(|value| *value == 0) {
            return None;
        }
        let timestamp = timestamp(line);
        Some(LineUsage {
            model: self.model.clone(),
            // The running total at its time names the event.
            response_key: timestamp.as_ref().map(|at| {
                format!(
                    "total:{at}:{}:{}:{}:{}",
                    current[0], current[1], current[2], current[3]
                )
            }),
            timestamp,
            // OpenAI counts cached and cache-written tokens inside the input.
            tokens: Tokens {
                input: delta[0].saturating_sub(delta[1]).saturating_sub(delta[2]),
                output: delta[3],
                cache_write_5m: delta[2],
                cache_write_1h: 0,
                cache_read: delta[1],
            },
            reported_cost: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_splits_the_cache_write_and_keys_the_response() {
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-10-01T10:00:00Z",
            "requestId": "req_1",
            "message": {"id": "msg_1", "model": "claude-opus-5-5", "usage": {
                "input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 3,
                "cache_creation": {"ephemeral_1h_input_tokens": 2}
            }}
        });
        let usage = claude_line(&line).expect("usage");
        assert_eq!(usage.response_key.as_deref(), Some("msg_1:req_1"));
        assert_eq!(usage.tokens.cache_write_5m, 1);
        assert_eq!(usage.tokens.cache_write_1h, 2);
        assert_eq!(usage.tokens.cache_write(), 3);
        let synthetic = serde_json::json!({
            "type": "assistant",
            "message": {"model": "<synthetic>", "usage": {"input_tokens": 1}}
        });
        assert_eq!(claude_line(&synthetic), None);
    }

    #[test]
    fn codex_keeps_its_running_total_across_calls() {
        let count = |input: u64, output: u64| {
            serde_json::json!({
                "timestamp": format!("2026-10-01T10:00:0{input}Z"),
                "type": "event_msg",
                "payload": {"type": "token_count", "info": {"total_token_usage": {
                    "input_tokens": input, "output_tokens": output
                }}}
            })
        };
        let mut totals = CodexTotals::default();
        assert_eq!(totals.line(&count(1, 1)).map(|u| u.tokens.output), Some(1));
        // A reader that stopped here goes on with the kept state.
        let mut resumed = totals.clone();
        assert_eq!(resumed.line(&count(1, 1)), None);
        assert_eq!(resumed.line(&count(3, 4)).map(|u| u.tokens.output), Some(3));
    }
}
