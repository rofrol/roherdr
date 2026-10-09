//! Per-model token prices for `herdr usage --workspace`'s cost estimate.
//!
//! The one place herdr keeps model prices. They are list prices, so the
//! estimate is not the provider's bill: subscriptions, discounts, batch and
//! priority tiers, fast mode, long-context surcharges and taxes are ignored.
//!
//! Every row comes from the one dated source below (`PRICE_SOURCE`, checked on
//! `PRICES_AS_OF`); a price that source does not list is not added from
//! memory. A model without a row is reported as unpriced tokens with its
//! model id, so the missing price is visible and can be added from a source.

/// When the prices below were last checked.
pub(crate) const PRICES_AS_OF: &str = "2026-10-06";

/// Where the prices below come from.
pub(crate) const PRICE_SOURCE: &str = "Anthropic API first-party list prices, standard tier \
     (claude.com/pricing as cached in Claude Code's claude-api reference); cache writes at \
     1.25x (5 minutes) and 2x (1 hour) of input. Models without a row (OpenAI, Gemini and \
     others) get no herdr price";

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

const fn anthropic(input: f64, output: f64, cache_read: f64) -> ModelPrice {
    ModelPrice {
        input,
        output,
        cache_write_5m: input * 1.25,
        cache_write_1h: input * 2.0,
        cache_read,
    }
}

/// Model id prefixes and their prices; the longest matching prefix wins.
const PRICES: &[(&str, ModelPrice)] = &[
    ("claude-fable-5-1", anthropic(10.0, 50.0, 0.25)),
    ("claude-mythos-5-1", anthropic(10.0, 50.0, 0.25)),
    ("claude-fable-5", anthropic(10.0, 50.0, 1.0)),
    ("claude-mythos-5", anthropic(10.0, 50.0, 1.0)),
    ("claude-opus-5-5", anthropic(4.0, 20.0, 0.20)),
    ("claude-opus-5", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-8", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-7", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-6", anthropic(5.0, 25.0, 0.50)),
    ("claude-opus-4-5", anthropic(5.0, 25.0, 0.50)),
    ("claude-sonnet-5-5", anthropic(2.0, 10.0, 0.20)),
    ("claude-sonnet-5", anthropic(2.0, 10.0, 0.20)),
    ("claude-sonnet-4", anthropic(3.0, 15.0, 0.30)),
    // Prompts up to 100K tokens; longer ones cost five times as much.
    ("claude-haiku-5-5", anthropic(0.10, 0.50, 0.01)),
    ("claude-haiku-4-5", anthropic(1.0, 5.0, 0.10)),
];

/// The price of `model` as a transcript names it (`claude-opus-5-5`,
/// `claude-sonnet-4-5-20250929`, `us.anthropic.claude-opus-4-8`,
/// `claude-opus-5-5[1m]`), or `None` when the table has no row for it.
pub(crate) fn price_for(model: &str) -> Option<ModelPrice> {
    let model = model.to_ascii_lowercase();
    let start = model.find("claude-")?;
    let model = &model[start..];
    PRICES
        .iter()
        .filter(|(prefix, _)| {
            model
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', '@', '[', ':', '.']))
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, price)| *price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(price_for("claude-opus-5-5").map(|p| p.input), Some(4.0));
        assert_eq!(price_for("claude-opus-5").map(|p| p.input), Some(5.0));
        assert_eq!(price_for("claude-opus-4-8").map(|p| p.input), Some(5.0));
        assert_eq!(
            price_for("claude-fable-5-1").map(|p| p.cache_read),
            Some(0.25)
        );
        assert_eq!(price_for("claude-fable-5").map(|p| p.cache_read), Some(1.0));
    }

    #[test]
    fn provider_prefixes_and_suffixes_are_ignored() {
        assert_eq!(
            price_for("us.anthropic.claude-sonnet-4-5-20250929-v1:0").map(|p| p.output),
            Some(15.0)
        );
        assert_eq!(
            price_for("claude-opus-5-5[1m]").map(|p| p.output),
            Some(20.0)
        );
        assert_eq!(price_for("Claude-Sonnet-5-5").map(|p| p.output), Some(10.0));
    }

    #[test]
    fn cache_writes_follow_input() {
        let price = price_for("claude-sonnet-5-5").expect("priced");
        assert_eq!(price.cache_write_5m, 2.5);
        assert_eq!(price.cache_write_1h, 4.0);
    }

    #[test]
    fn unknown_models_have_no_price() {
        assert_eq!(price_for("gpt-6.1-sol"), None);
        assert_eq!(price_for("<synthetic>"), None);
        assert_eq!(price_for("claude-opus-45"), None);
        // Not in the dated source, so not priced from memory either.
        assert_eq!(price_for("claude-opus-4-1-20250805"), None);
        assert_eq!(price_for("claude-opus-4-20250514"), None);
        assert_eq!(price_for("claude-3-5-haiku-20241022"), None);
    }
}
