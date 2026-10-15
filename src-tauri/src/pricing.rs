use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SOURCE: &str = "https://platform.claude.com/docs/en/about-claude/pricing";
pub const VERIFIED: &str = "2026-10-16";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub cache_5m: f64,
    pub cache_1h: f64,
    pub read: f64,
    pub output: f64,
}

impl Rates {
    fn new(input: f64, read: f64, output: f64) -> Self {
        Self {
            input,
            cache_5m: input * 1.25,
            cache_1h: input * 2.0,
            read,
            output,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if [
            self.input,
            self.cache_5m,
            self.cache_1h,
            self.read,
            self.output,
        ]
        .into_iter()
        .all(|n| n.is_finite() && (0.0..=1_000_000.0).contains(&n))
        {
            Ok(())
        } else {
            Err("Rates must be finite USD values between 0 and 1,000,000 per million tokens".into())
        }
    }
    fn multiply(&mut self, factor: f64) {
        self.input *= factor;
        self.cache_5m *= factor;
        self.cache_1h *= factor;
        self.read *= factor;
        self.output *= factor;
    }
}

pub fn builtin(model: &str) -> Option<Rates> {
    let model = canonical(model)?;
    Some(match model {
        "claude-opus-5-5" => Rates::new(4.0, 0.2, 20.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
        | "claude-opus-4-5" => Rates::new(5.0, 0.5, 25.0),
        "claude-opus-4-1" | "claude-opus-4" => Rates::new(15.0, 1.5, 75.0),
        "claude-sonnet-5-5" => Rates::new(2.0, 0.1, 10.0),
        "claude-sonnet-5" => Rates::new(2.0, 0.2, 10.0),
        "claude-sonnet-4-6" | "claude-sonnet-4-5" | "claude-sonnet-4" => Rates::new(3.0, 0.3, 15.0),
        "claude-haiku-5-5" => Rates::new(0.1, 0.01, 0.5),
        "claude-haiku-4-5" => Rates::new(1.0, 0.1, 5.0),
        "claude-haiku-3-5" => Rates::new(0.8, 0.08, 4.0),
        "claude-fable-5-1" | "claude-mythos-5-1" => Rates::new(10.0, 0.25, 50.0),
        "claude-fable-5" | "claude-mythos-5" => Rates::new(10.0, 1.0, 50.0),
        _ => return None,
    })
}

fn canonical(model: &str) -> Option<&'static str> {
    // Exact aliases and explicit YYYYMMDD snapshots only; never infer pricing for a future model.
    const MODELS: &[&str] = &[
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-opus-4-5",
        "claude-opus-4-1",
        "claude-opus-4",
        "claude-sonnet-5-5",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-sonnet-4-5",
        "claude-sonnet-4",
        "claude-haiku-5-5",
        "claude-haiku-4-5",
        "claude-haiku-3-5",
        "claude-fable-5-1",
        "claude-mythos-5-1",
        "claude-fable-5",
        "claude-mythos-5",
    ];
    MODELS.iter().copied().find(|name| {
        model == *name
            || model
                .strip_prefix(name)
                .and_then(|s| s.strip_prefix('-'))
                .is_some_and(|suffix| {
                    suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit())
                })
    })
}

#[derive(Debug, Default)]
pub struct Metadata {
    pub cache_1h: Option<i64>,
    pub speed: String,
    pub geo: String,
    pub searches: i64,
}

#[derive(Clone, Copy)]
pub struct Tokens {
    pub input: i64,
    pub creation: i64,
    pub read: i64,
    pub output: i64,
}

#[derive(Serialize)]
pub struct Estimate {
    pub total_usd: f64,
    pub input_usd: f64,
    pub cache_write_usd: f64,
    pub cache_read_usd: f64,
    pub output_usd: f64,
    pub web_search_usd: f64,
    pub rates_per_million: Rates,
    pub basis: String,
    pub notes: Vec<String>,
}

pub fn estimate(
    model: &str,
    t: Tokens,
    meta: &Metadata,
    overrides: &BTreeMap<String, Rates>,
) -> Option<Estimate> {
    let custom = overrides.get(model);
    let mut rates = custom.cloned().or_else(|| builtin(model))?;
    let mut notes=vec!["Token usage and recorded web searches only; excludes tax, other tools, and subscription charges.".into()];
    if custom.is_none() {
        if !matches!(meta.speed.as_str(), "" | "standard" | "fast")
            || !matches!(meta.geo.as_str(), "" | "not_available" | "global" | "us")
        {
            return None;
        }
        if canonical(model) == Some("claude-haiku-5-5")
            && t.input.saturating_add(t.creation).saturating_add(t.read) > 100_000
        {
            rates.multiply(5.0);
            notes.push(
                "Haiku 5.5 prompt exceeds 100,000 tokens: higher rates apply to the whole call."
                    .into(),
            );
        }
        if meta.speed == "fast" {
            match canonical(model) {
                Some("claude-opus-5-5" | "claude-opus-5" | "claude-opus-4-8") => {
                    rates.multiply(2.0);
                    notes.push("Fast mode: 2x token rates.".into());
                }
                Some("claude-opus-4-6") => {}
                _ => return None,
            }
        }
        if meta.geo == "us" {
            rates.multiply(1.1);
            notes.push("US inference: 1.1x token rates.".into());
        }
    } else {
        notes.push("Custom effective rates replace model tiers and speed/region modifiers.".into());
    }
    let one_hour = meta.cache_1h.unwrap_or(0).clamp(0, t.creation);
    if meta.cache_1h.is_none() && t.creation > 0 {
        notes.push("Cache duration missing: cache writes assumed to be 5 minutes.".into());
    }
    if meta.speed.is_empty() {
        notes.push("Speed missing: standard mode assumed.".into());
    }
    if meta.geo.is_empty() || meta.geo == "not_available" {
        notes.push("Inference geography unavailable: global assumed.".into());
    }
    let input_usd = t.input as f64 * rates.input / 1e6;
    let cache_write_usd =
        ((t.creation - one_hour) as f64 * rates.cache_5m + one_hour as f64 * rates.cache_1h) / 1e6;
    let cache_read_usd = t.read as f64 * rates.read / 1e6;
    let output_usd = t.output as f64 * rates.output / 1e6;
    let web_search_usd = meta.searches as f64 * 0.01;
    Some(Estimate {
        total_usd: input_usd + cache_write_usd + cache_read_usd + output_usd + web_search_usd,
        input_usd,
        cache_write_usd,
        cache_read_usd,
        output_usd,
        web_search_usd,
        rates_per_million: rates,
        basis: if custom.is_some() {
            "Custom effective USD rates".into()
        } else {
            format!("Anthropic list prices verified {VERIFIED}; {SOURCE}")
        },
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn near(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-10, "{a} != {b}");
    }
    #[test]
    fn cache_durations_output_and_searches_are_charged_separately() {
        let e = estimate(
            "claude-opus-5-5",
            Tokens {
                input: 1000,
                creation: 3000,
                read: 10000,
                output: 500,
            },
            &Metadata {
                cache_1h: Some(1000),
                speed: "standard".into(),
                geo: "global".into(),
                searches: 2,
            },
            &BTreeMap::new(),
        )
        .unwrap();
        near(e.total_usd, 0.004 + 0.010 + 0.008 + 0.002 + 0.010 + 0.020);
    }
    #[test]
    fn haiku_threshold_counts_reads_and_changes_all_categories() {
        let t = Tokens {
            input: 1,
            creation: 0,
            read: 99999,
            output: 1,
        };
        let small = estimate(
            "claude-haiku-5-5",
            t,
            &Metadata::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        let large = estimate(
            "claude-haiku-5-5",
            Tokens { read: 100000, ..t },
            &Metadata::default(),
            &BTreeMap::new(),
        )
        .unwrap();
        near(small.rates_per_million.input, 0.1);
        near(large.rates_per_million.input, 0.5);
        near(large.rates_per_million.output, 2.5);
    }
    #[test]
    fn fast_geo_snapshots_and_unknown_models() {
        let t = Tokens {
            input: 1_000_000,
            creation: 0,
            read: 0,
            output: 0,
        };
        let meta = Metadata {
            speed: "fast".into(),
            geo: "us".into(),
            ..Metadata::default()
        };
        near(
            estimate("claude-opus-5-5-20261001", t, &meta, &BTreeMap::new())
                .unwrap()
                .total_usd,
            8.8,
        );
        assert!(estimate("claude-opus-9", t, &meta, &BTreeMap::new()).is_none());
        let custom = BTreeMap::from([("claude-opus-9".into(), Rates::new(1.0, 0.1, 5.0))]);
        near(
            estimate("claude-opus-9", t, &meta, &custom)
                .unwrap()
                .total_usd,
            1.0,
        );
    }

    #[test]
    fn unknown_speed_or_geography_requires_explicit_rates() {
        let t = Tokens {
            input: 1000,
            creation: 0,
            read: 0,
            output: 0,
        };
        let meta = Metadata {
            speed: "turbo".into(),
            ..Metadata::default()
        };
        assert!(estimate("claude-opus-5-5", t, &meta, &BTreeMap::new()).is_none());
        let custom = BTreeMap::from([("claude-opus-5-5".into(), Rates::new(1.0, 0.1, 5.0))]);
        near(
            estimate("claude-opus-5-5", t, &meta, &custom)
                .unwrap()
                .total_usd,
            0.001,
        );
        assert!(estimate(
            "claude-opus-5-5",
            t,
            &Metadata {
                geo: "future-region".into(),
                ..Metadata::default()
            },
            &BTreeMap::new()
        )
        .is_none());
        let unavailable = estimate(
            "claude-opus-5-5",
            t,
            &Metadata {
                geo: "not_available".into(),
                ..Metadata::default()
            },
            &BTreeMap::new(),
        )
        .unwrap();
        near(unavailable.total_usd, 0.004);
        assert!(unavailable
            .notes
            .iter()
            .any(|note| note.contains("geography unavailable")));
    }
}
