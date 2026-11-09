//! Source-scoped reference API costs, never Antigravity subscription charges.
use crate::pricing::{self, Estimate, Metadata, Rates, Tokens};
use std::collections::BTreeMap;

pub const GOOGLE_SOURCE: &str = "https://ai.google.dev/gemini-api/docs/pricing";
pub const VERIFIED: &str = "2026-11-09";
pub struct Context<'a> {
    pub source: &'a str,
    pub model: &'a str,
    pub timestamp: &'a str,
    pub complete: bool,
    pub cache_known: bool,
    pub creation_known: bool,
}
pub fn key(family: &str, model: &str) -> String {
    if family == "antigravity" {
        format!("antigravity::{model}")
    } else {
        model.into()
    }
}

pub fn builtin(model: &str, t: Tokens, date: &str) -> Option<Rates> {
    let (input, read, output) = match model {
        "gemini-4-argon" if date.len() >= 10 => {
            if date >= "2026-12-01" {
                (4.0, 0.20, 20.0)
            } else {
                (2.0, 0.10, 10.0)
            }
        }
        "gemini-3.8-flash" | "gemini-3.7-flash" | "gemini-3.6-flash" if date.len() >= 10 => {
            if date >= "2027-01-01" {
                (1.5, 0.15, 7.5)
            } else {
                (0.75, 0.075, 3.75)
            }
        }
        "gemini-2.5-pro" => {
            if t.input.saturating_add(t.read) > 200_000 {
                (2.5, 0.25, 15.0)
            } else {
                (1.25, 0.125, 10.0)
            }
        }
        _ => {
            return pricing::estimate(model, t, &Metadata::default(), &BTreeMap::new())
                .map(|e| e.rates_per_million)
        }
    };
    Some(Rates {
        input,
        read,
        output,
        cache_5m: 0.0,
        cache_1h: 0.0,
    })
}

pub fn estimate(
    c: Context<'_>,
    t: Tokens,
    meta: &Metadata,
    overrides: &BTreeMap<String, Rates>,
) -> Option<Estimate> {
    if c.source == "claude" {
        return pricing::estimate(c.model, t, meta, overrides);
    }
    if !c.complete || !c.cache_known {
        return None;
    }
    let custom = overrides.get(&key("antigravity", c.model));
    let rates = custom
        .cloned()
        .or_else(|| builtin(c.model, t, c.timestamp))?;
    // Missing write duration is never silently assumed to be Anthropic's 5m TTL.
    if t.creation > 0 && (custom.is_none() || !c.creation_known) {
        return None;
    }
    let input_usd = t.input as f64 * rates.input / 1e6;
    let cache_read_usd = t.read as f64 * rates.read / 1e6;
    let output_usd = t.output as f64 * rates.output / 1e6;
    let cache_write_usd = t.creation as f64 * rates.cache_5m / 1e6;
    let source = if c.model.starts_with("gemini-") {
        GOOGLE_SOURCE
    } else {
        pricing::SOURCE
    };
    let mut notes=vec!["Reference standard API token cost, not Antigravity subscription billing or quota consumption.".into(),
        "Output already includes thinking; thinking is not charged a second time. Unreported storage, grounding, tools, region and service-tier charges are excluded.".into()];
    if !c.creation_known {
        notes.push("Cache-write usage/TTL unavailable: reference cost covers reported input, cache reads and output only.".into());
    }
    if c.model.starts_with("gemini-") {
        notes.push("Scheduled introductory price changes use the call timestamp. No price is inferred for internal aliases.".into());
    }
    if custom.is_some() && t.creation > 0 {
        notes.push(
            "Custom write rate is an effective per-token rate; actual TTL was not recorded.".into(),
        );
    }
    Some(Estimate {
        total_usd: input_usd + cache_read_usd + output_usd + cache_write_usd,
        input_usd,
        cache_write_usd: c.creation_known.then_some(cache_write_usd),
        cache_read_usd,
        output_usd,
        web_search_usd: None,
        rates_per_million: rates,
        basis: if custom.is_some() {
            "Antigravity custom reference rates (IDE and CLI)".into()
        } else {
            format!("Reference API prices verified {VERIFIED}; {source}")
        },
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dated_tiers_exact_models_and_scoped_overrides() {
        let t = Tokens {
            input: 1000,
            creation: 0,
            read: 10000,
            output: 500,
        };
        let cost = |date: &str| {
            estimate(
                Context {
                    source: "antigravity_ide",
                    model: "gemini-4-argon",
                    timestamp: date,
                    complete: true,
                    cache_known: true,
                    creation_known: true,
                },
                t,
                &Metadata::default(),
                &BTreeMap::new(),
            )
            .unwrap()
            .total_usd
        };
        assert!((cost("2026-11-09") - 0.008).abs() < 1e-10);
        assert!((cost("2026-12-01") - 0.016).abs() < 1e-10);
        assert!(builtin("gemini-pro-default", t, "2026-11-09").is_none());
        assert_ne!(key("claude", "same"), key("antigravity", "same"));
        assert_eq!(
            builtin(
                "gemini-2.5-pro",
                Tokens { input: 200001, ..t },
                "2026-11-09"
            )
            .unwrap()
            .input,
            2.5
        );
    }
}
