//! List prices of models, for agents that report tokens but not what they
//! cost, as Claude Code does message by message. The prices are LiteLLM's
//! public list, as ccusage uses it, fetched and kept a week on disk.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use ironquill_core::Usd;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// LiteLLM's list of models with their prices, under the MIT licence.
pub const PRICES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// How long a copy kept on disk is used before fetching it again.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// What a model charges per token.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// Input read fresh.
    pub input: f64,
    /// Output, thinking included.
    pub output: f64,
    /// Input read from the cache.
    pub cache_read: f64,
    /// Input written to the cache.
    pub cache_write: f64,
}

/// The tokens of one call, split the way they are priced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Input read fresh.
    pub input: u64,
    /// Input read from the cache.
    pub cache_read: u64,
    /// Input written to the cache.
    pub cache_write: u64,
    /// Output.
    pub output: u64,
}

/// List prices by model name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PriceTable(HashMap<String, Rates>);

impl PriceTable {
    /// Reads LiteLLM's list, keeping the models priced per token.
    pub fn parse(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        let table = value
            .as_object()?
            .iter()
            .filter_map(|(name, entry)| {
                let price = |key: &str| entry[key].as_f64();
                let input = price("input_cost_per_token")?;
                let output = price("output_cost_per_token")?;
                Some((
                    name.clone(),
                    Rates {
                        input,
                        output,
                        cache_read: price("cache_read_input_token_cost").unwrap_or(input),
                        cache_write: price("cache_creation_input_token_cost").unwrap_or(input),
                    },
                ))
            })
            .collect();
        Some(Self(table))
    }

    /// The rates of `model`, by its name, or with the provider in front as
    /// LiteLLM sometimes writes it.
    pub fn rates(&self, model: &str) -> Option<Rates> {
        self.0
            .get(model)
            .or_else(|| self.0.get(&format!("anthropic/{model}")))
            .copied()
    }

    /// What `tokens` cost on `model` at its list prices.
    pub fn cost(&self, model: &str, tokens: Tokens) -> Option<Usd> {
        let rates = self.rates(model)?;
        // Token counts stay far below 2^53, so the conversions are exact.
        Some(Usd(tokens.input as f64 * rates.input
            + tokens.cache_read as f64 * rates.cache_read
            + tokens.cache_write as f64 * rates.cache_write
            + tokens.output as f64 * rates.output))
    }
}

/// The price list, from the copy at `kept` when it is under a week old,
/// else fetched and kept there. `None` when neither works: costs are then
/// left unknown, never guessed.
pub async fn load_prices(kept: &Path) -> Option<PriceTable> {
    let fresh = std::fs::metadata(kept)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < KEEP);
    let read_kept = || {
        std::fs::read(kept)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<PriceTable>(&bytes).ok())
    };
    if fresh && let Some(table) = read_kept() {
        return Some(table);
    }
    let fetched = async {
        let body = reqwest::Client::new()
            .get(PRICES_URL)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .ok()?
            .text()
            .await
            .ok()?;
        PriceTable::parse(&body)
    }
    .await;
    match fetched {
        Some(table) => {
            if let Ok(json) = serde_json::to_vec(&table) {
                let _ = std::fs::create_dir_all(kept.parent().unwrap_or(Path::new(".")));
                let _ = std::fs::write(kept, json);
            }
            Some(table)
        }
        // An old copy beats none.
        None => read_kept(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_is_priced_with_its_cache_rates() {
        let table = PriceTable::parse(
            r#"{"sample_spec": {"input_cost_per_token": "x"},
                "claude-opus-5-5": {"input_cost_per_token": 4e-6, "output_cost_per_token": 2e-5,
                                    "cache_read_input_token_cost": 2e-7,
                                    "cache_creation_input_token_cost": 5e-6}}"#,
        )
        .unwrap();
        let tokens = Tokens {
            input: 1_000,
            cache_read: 100_000,
            cache_write: 2_000,
            output: 500,
        };
        let cost = table.cost("claude-opus-5-5", tokens).unwrap();
        // 0.004 + 0.02 + 0.01 + 0.01
        assert!((cost.0 - 0.044).abs() < 1e-12, "{cost:?}");
        assert_eq!(table.cost("unknown", tokens), None);
    }
}
