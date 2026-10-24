//! A worker run's usage per model as its agent reports it, folded into its
//! status at the event that reports it ([`super::status::Status::apply`]):
//! Claude's `result` carries the session's cumulative `modelUsage` and
//! `total_cost_usd`, so the latest one replaces the run's; a pi worker's
//! translated `assistant` event carries one model reply's usage
//! ([`super::pi`]'s `herdr_usage`), which adds to it.

use serde_json::{json, Value};

use crate::api::schema::WorkerModelUsage;

/// The usage per model of Claude's `result` event (its `modelUsage`, which
/// covers the whole session so far), or `None` when it has none.
pub(super) fn claude_result_usage(event: &Value) -> Option<Vec<WorkerModelUsage>> {
    let models = event["modelUsage"].as_object()?;
    Some(
        models
            .iter()
            .map(|(model, counts)| {
                let count = |key: &str| counts[key].as_u64().unwrap_or(0);
                WorkerModelUsage {
                    model: model.clone(),
                    input_tokens: count("inputTokens"),
                    output_tokens: count("outputTokens"),
                    cache_read_input_tokens: count("cacheReadInputTokens"),
                    cache_creation_input_tokens: count("cacheCreationInputTokens"),
                    cost_usd: counts["costUSD"].as_f64(),
                }
            })
            .collect(),
    )
}

/// The `herdr_usage` a pi `assistant` event carries for one model reply,
/// built from pi's `AssistantMessage` (`provider`, `model` and `usage`:
/// `input`, `output`, `cacheRead`, `cacheWrite` and `cost.total`, as
/// `@earendil-works/pi-ai`'s `types.d.ts` declares them). `None` when the
/// message has no usage or misses one of the token counts.
pub(super) fn pi_message_usage(message: &Value) -> Option<Value> {
    let usage = message["usage"].as_object()?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    let model = match (message["provider"].as_str(), message["model"].as_str()) {
        (Some(provider), Some(model)) => format!("{provider}/{model}"),
        (None, Some(model)) => model.to_owned(),
        _ => return None,
    };
    Some(json!({
        "model": model,
        "input_tokens": count("input")?,
        "output_tokens": count("output")?,
        "cache_read_input_tokens": count("cacheRead")?,
        "cache_creation_input_tokens": count("cacheWrite")?,
        "cost_usd": usage.get("cost").and_then(|cost| cost["total"].as_f64()),
    }))
}

/// Adds one reply's usage to the run's, by model. A model's cost stays
/// unknown once one of its replies reported none.
pub(super) fn add(usage: &mut Vec<WorkerModelUsage>, reply: &WorkerModelUsage) {
    match usage.iter_mut().find(|entry| entry.model == reply.model) {
        Some(entry) => {
            entry.input_tokens += reply.input_tokens;
            entry.output_tokens += reply.output_tokens;
            entry.cache_read_input_tokens += reply.cache_read_input_tokens;
            entry.cache_creation_input_tokens += reply.cache_creation_input_tokens;
            entry.cost_usd = entry.cost_usd.zip(reply.cost_usd).map(|(a, b)| a + b);
        }
        None => usage.push(reply.clone()),
    }
}

/// The run's cost: the sum of its models' costs, unknown when any is.
pub(super) fn total_cost(usage: &[WorkerModelUsage]) -> Option<f64> {
    usage
        .iter()
        .try_fold(0.0, |total, entry| entry.cost_usd.map(|cost| total + cost))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claudes_model_usage_is_read_per_model_and_none_without_it() {
        let event = json!({"type": "result", "modelUsage": {"m": {"inputTokens": 1,
            "outputTokens": 2, "cacheReadInputTokens": 3, "cacheCreationInputTokens": 4,
            "costUSD": 0.5}}});
        let usage = claude_result_usage(&event).unwrap();
        assert_eq!(
            usage,
            [WorkerModelUsage {
                model: "m".into(),
                input_tokens: 1,
                output_tokens: 2,
                cache_read_input_tokens: 3,
                cache_creation_input_tokens: 4,
                cost_usd: Some(0.5),
            }]
        );
        assert_eq!(claude_result_usage(&json!({"type": "result"})), None);
    }

    #[test]
    fn a_pi_reply_needs_every_token_count_and_its_cost_may_be_unknown() {
        let message = json!({"provider": "openai-codex", "model": "gpt", "usage": {
            "input": 10, "output": 5, "cacheRead": 3, "cacheWrite": 2, "totalTokens": 20,
            "cost": {"total": 0.1}}});
        let reply: WorkerModelUsage =
            serde_json::from_value(pi_message_usage(&message).unwrap()).unwrap();
        assert_eq!(reply.model, "openai-codex/gpt");
        assert_eq!(
            (reply.input_tokens, reply.output_tokens),
            (10, 5),
            "{reply:?}"
        );
        assert_eq!(
            (
                reply.cache_read_input_tokens,
                reply.cache_creation_input_tokens
            ),
            (3, 2)
        );
        assert_eq!(reply.cost_usd, Some(0.1));
        let mut missing = message.clone();
        missing["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cacheWrite");
        assert_eq!(pi_message_usage(&missing), None);
        assert_eq!(pi_message_usage(&json!({"model": "gpt"})), None);

        let mut usage = vec![reply.clone()];
        add(&mut usage, &reply);
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].input_tokens, 20);
        assert_eq!(total_cost(&usage), Some(0.2));
        add(
            &mut usage,
            &WorkerModelUsage {
                cost_usd: None,
                ..reply
            },
        );
        assert_eq!(usage[0].cost_usd, None);
        assert_eq!(total_cost(&usage), None);
    }
}
