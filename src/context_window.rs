//! The context window of the model that served an agent's last request, from a named source,
//! and how full the window was as of that request.
//!
//! Sources, first match wins: a fact the agent records (none known: Claude Code records the
//! model and the request's usage in its transcript, but not the window), the user's
//! `[agents] context_window` setting, then [`TABLE`]. The model id is the transcript's
//! (`AgentContext::model`), never prose. An unknown model or variant has no window, and a
//! request whose input exceeds the window a source named proves that window wrong for the
//! session: it is withheld. Unknown never counts as under the threshold.

use std::collections::BTreeMap;

use crate::api::schema::{AgentContext, AgentContextWindow};

/// The share of the window, in percent, from which an agent's context counts as over.
pub(crate) const OVER_PERCENT: f64 = 70.0;

/// One vendor-documented context window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableEntry {
    /// The exact model id the transcript records.
    pub(crate) model: &'static str,
    /// Which variant of the model the window is for (a plan, a long-context beta).
    pub(crate) variant: &'static str,
    pub(crate) tokens: u64,
    /// The vendor document the value was taken from.
    pub(crate) url: &'static str,
    /// The day (YYYY-MM-DD) that document was checked.
    pub(crate) checked: &'static str,
}

/// Context windows taken from a vendor document, cited by URL and the day it was checked.
/// A model listed with more than one variant has no window from the table, since the
/// transcript does not name the variant.
pub(crate) const TABLE: &[TableEntry] = &[
    anthropic("claude-fable-5-1", 1_000_000),
    anthropic("claude-opus-5-5", 1_000_000),
    anthropic("claude-sonnet-5-5", 1_000_000),
    anthropic("claude-haiku-5-5", 1_000_000),
];

/// Anthropic's "Models overview" lists one context window per current model ("Compare
/// models", row "Context window"), with no separate long-context variant.
const fn anthropic(model: &'static str, tokens: u64) -> TableEntry {
    TableEntry {
        model,
        variant: "standard",
        tokens,
        url: "https://platform.claude.com/docs/en/models/overview",
        checked: "2026-10-23",
    }
}

/// The window of the model that served `context`'s request: an agent fact, then the user's
/// `user` setting (exact model id to tokens), then `table`.
pub(crate) fn resolve(
    context: &AgentContext,
    agent_fact: Option<u64>,
    user: &BTreeMap<String, u64>,
    table: &[TableEntry],
) -> AgentContextWindow {
    let Some(model) = context.model.as_deref().filter(|model| !model.is_empty()) else {
        return unknown("the transcript names no model for the request".into());
    };
    let named = if let Some(tokens) = agent_fact {
        AgentContextWindow {
            tokens: Some(tokens),
            provenance: "agent".into(),
            detail: format!("recorded by the agent for {model}"),
            url: None,
            checked: None,
        }
    } else if let Some(tokens) = user.get(model) {
        AgentContextWindow {
            tokens: Some(*tokens),
            provenance: "user".into(),
            detail: format!("user-set: [agents] context_window \"{model}\" in herdr's config"),
            url: None,
            checked: None,
        }
    } else {
        let entries = table
            .iter()
            .filter(|entry| entry.model == model)
            .collect::<Vec<_>>();
        match entries.as_slice() {
            [] => return unknown(format!("no source knows the window of {model}")),
            [entry] => AgentContextWindow {
                tokens: Some(entry.tokens),
                provenance: "table".into(),
                detail: format!("herdr's table: {model} ({})", entry.variant),
                url: Some(entry.url.into()),
                checked: Some(entry.checked.into()),
            },
            _ => {
                return unknown(format!(
                    "the table lists several variants of {model} and the transcript does not \
                     name the variant"
                ))
            }
        }
    };
    match named.tokens {
        Some(window) if context.last_request_input_tokens > window => AgentContextWindow {
            tokens: None,
            provenance: "withheld".into(),
            detail: format!(
                "the request's input of {} tokens exceeds the window of {window} named by {} \
                 ({}), so that window is wrong for this session",
                context.last_request_input_tokens, named.provenance, named.detail
            ),
            url: named.url,
            checked: named.checked,
        },
        _ => named,
    }
}

fn unknown(detail: String) -> AgentContextWindow {
    AgentContextWindow {
        tokens: None,
        provenance: "unknown".into(),
        detail,
        url: None,
        checked: None,
    }
}

/// Where a window came from, or why there is none, in words for a tooltip.
pub(crate) fn source_text(window: &AgentContextWindow) -> String {
    match (window.provenance.as_str(), &window.url, &window.checked) {
        ("table", Some(url), Some(checked)) => {
            format!("{}, from {url}, checked {checked}", window.detail)
        }
        ("agent" | "user" | "table", _, _) => window.detail.clone(),
        _ => format!("no window: {}", window.detail),
    }
}

/// How full the window was, in percent and unrounded; `None` without a window.
pub(crate) fn percent(tokens: u64, window: Option<u64>) -> Option<f64> {
    window
        .filter(|window| *window > 0)
        .map(|window| tokens as f64 * 100.0 / window as f64)
}

/// `over` from [`OVER_PERCENT`] on, `under` below it, `unknown` without a percentage.
pub(crate) fn status(percent: Option<f64>) -> &'static str {
    match percent {
        Some(percent) if percent >= OVER_PERCENT => "over",
        Some(_) => "under",
        None => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &str = "model-a";

    fn context(tokens: u64, model: Option<&str>) -> AgentContext {
        AgentContext {
            last_request_input_tokens: tokens,
            session_id: "s1".into(),
            message_id: None,
            at: None,
            model: model.map(str::to_string),
            window: None,
        }
    }

    fn entry(model: &'static str, variant: &'static str, tokens: u64) -> TableEntry {
        TableEntry {
            model,
            variant,
            tokens,
            url: "https://vendor.example/models",
            checked: "2026-10-23",
        }
    }

    fn user(tokens: u64) -> BTreeMap<String, u64> {
        BTreeMap::from([(MODEL.to_string(), tokens)])
    }

    #[test]
    fn every_shipped_entry_cites_a_vendor_document_and_a_day() {
        assert!(TABLE
            .iter()
            .all(|entry| entry.url.starts_with("https://") && entry.checked.len() == 10));
    }

    #[test]
    fn the_current_claude_models_resolve_to_their_documented_window() {
        for model in [
            "claude-fable-5-1",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
        ] {
            let window = resolve(
                &context(420_522, Some(model)),
                None,
                &BTreeMap::new(),
                TABLE,
            );
            assert_eq!(window.tokens, Some(1_000_000), "{model}");
            assert_eq!(window.provenance, "table", "{model}");
            assert_eq!(
                window.url.as_deref(),
                Some("https://platform.claude.com/docs/en/models/overview"),
                "{model}"
            );
            assert_eq!(window.checked.as_deref(), Some("2026-10-23"), "{model}");
        }
    }

    #[test]
    fn the_shipped_table_leaves_other_models_unknown_and_yields_to_the_user_setting() {
        let unknown = resolve(
            &context(100, Some("claude-opus-5-5-preview")),
            None,
            &BTreeMap::new(),
            TABLE,
        );
        assert_eq!(
            (unknown.tokens, unknown.provenance.as_str()),
            (None, "unknown")
        );
        let user = BTreeMap::from([("claude-opus-5-5".to_string(), 500_000)]);
        let window = resolve(&context(100, Some("claude-opus-5-5")), None, &user, TABLE);
        assert_eq!(window.tokens, Some(500_000));
        assert_eq!(window.provenance, "user");
    }

    #[test]
    fn the_agent_fact_comes_before_the_user_setting_and_the_table() {
        let window = resolve(
            &context(100, Some(MODEL)),
            Some(1_000),
            &user(2_000),
            &[entry(MODEL, "standard", 3_000)],
        );
        assert_eq!(window.tokens, Some(1_000));
        assert_eq!(window.provenance, "agent");
    }

    #[test]
    fn the_user_setting_comes_before_the_table() {
        let window = resolve(
            &context(100, Some(MODEL)),
            None,
            &user(2_000),
            &[entry(MODEL, "standard", 3_000)],
        );
        assert_eq!(window.tokens, Some(2_000));
        assert_eq!(window.provenance, "user");
        assert!(window.detail.contains("[agents] context_window"));
        assert_eq!(window.url, None);
    }

    #[test]
    fn the_table_names_its_vendor_source_and_date() {
        let window = resolve(
            &context(100, Some(MODEL)),
            None,
            &BTreeMap::new(),
            &[
                entry("other", "standard", 9),
                entry(MODEL, "standard", 3_000),
            ],
        );
        assert_eq!(window.tokens, Some(3_000));
        assert_eq!(window.provenance, "table");
        assert_eq!(window.url.as_deref(), Some("https://vendor.example/models"));
        assert_eq!(window.checked.as_deref(), Some("2026-10-23"));
    }

    #[test]
    fn an_unknown_model_or_variant_has_no_window() {
        let none = BTreeMap::new();
        let table = [
            entry(MODEL, "standard", 3_000),
            entry(MODEL, "long-context beta", 9_000),
        ];
        // The user setting is keyed by the exact id: a prefix is another model.
        let other = resolve(&context(100, Some("model-a-2")), None, &user(2_000), &table);
        assert_eq!((other.tokens, other.provenance.as_str()), (None, "unknown"));
        let variant = resolve(&context(100, Some(MODEL)), None, &none, &table);
        assert_eq!(
            (variant.tokens, variant.provenance.as_str()),
            (None, "unknown")
        );
        let no_model = resolve(&context(100, None), Some(1_000), &none, &table);
        assert_eq!(
            (no_model.tokens, no_model.provenance.as_str()),
            (None, "unknown")
        );
    }

    #[test]
    fn an_input_above_the_window_withholds_it_and_names_why() {
        let withheld = resolve(&context(420_522, Some(MODEL)), None, &user(200_000), &[]);
        assert_eq!(withheld.tokens, None);
        assert_eq!(withheld.provenance, "withheld");
        assert!(withheld.detail.contains("420522"));
        assert!(withheld.detail.contains("200000"));
        assert!(withheld.detail.contains("user"));
        let table = resolve(
            &context(420_522, Some(MODEL)),
            None,
            &BTreeMap::new(),
            &[entry(MODEL, "standard", 200_000)],
        );
        assert_eq!(
            (table.tokens, table.provenance.as_str()),
            (None, "withheld")
        );
        // The input exactly at the window fits it.
        let full = resolve(&context(200_000, Some(MODEL)), None, &user(200_000), &[]);
        assert_eq!(full.tokens, Some(200_000));
    }

    #[test]
    fn the_source_text_names_the_setting_the_vendor_document_or_why_there_is_none() {
        let user = resolve(&context(1, Some(MODEL)), None, &user(10), &[]);
        assert_eq!(
            source_text(&user),
            "user-set: [agents] context_window \"model-a\" in herdr's config"
        );
        let table = resolve(
            &context(1, Some(MODEL)),
            None,
            &BTreeMap::new(),
            &[entry(MODEL, "standard", 10)],
        );
        assert_eq!(
            source_text(&table),
            "herdr's table: model-a (standard), from https://vendor.example/models, checked \
             2026-10-23"
        );
        let unknown = resolve(&context(1, Some("x")), None, &BTreeMap::new(), &[]);
        assert_eq!(
            source_text(&unknown),
            "no window: no source knows the window of x"
        );
    }

    #[test]
    fn the_status_is_over_from_70_percent_and_unknown_without_a_window() {
        assert_eq!(percent(420_000, Some(1_000_000)), Some(42.0));
        assert_eq!(status(percent(420_000, Some(1_000_000))), "under");
        assert_eq!(status(percent(699_999, Some(1_000_000))), "under");
        assert_eq!(status(percent(700_000, Some(1_000_000))), "over");
        assert_eq!(status(percent(420_000, None)), "unknown");
        assert_eq!(percent(1, Some(0)), None);
        // Unrounded.
        assert_eq!(percent(1, Some(3)), Some(100.0 / 3.0));
    }
}
