use crate::{
    pricing::{self, Metadata, Tokens},
    store,
    usage::Settings,
};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Default, Serialize)]
pub struct ModelStatistics {
    pub model: String,
    pub calls: u64,
    pub estimated_usd: f64,
    pub priced_calls: u64,
    pub uncached_tokens: i64,
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Default, Serialize)]
pub struct Statistics {
    pub earliest_ms: Option<i64>,
    pub latest_ms: Option<i64>,
    pub undated_calls: i64,
    pub calls: u64,
    pub priced_calls: u64,
    pub estimated_usd: f64,
    pub max_cost_usd: f64,
    pub ordinary_input: i64,
    pub cache_writes: i64,
    pub cache_1h: i64,
    pub cache_reads: i64,
    pub output: i64,
    pub uncached: i64,
    pub max_uncached: i64,
    pub total_input: i64,
    pub cache_read_discount_usd: f64,
    pub zero_cache_calls: u64,
    pub expensive_calls: u64,
    pub alerts_sent: u64,
    pub alerts_failed: u64,
    pub alerts_pending: u64,
    pub sessions: usize,
    pub subagents: usize,
    pub projects: usize,
    pub models: Vec<ModelStatistics>,
}

pub fn calculate(
    conn: &Connection,
    start_ms: Option<i64>,
    end_ms: i64,
    settings: &Settings,
) -> rusqlite::Result<Statistics> {
    let rates = store::load_rates(conn)?;
    let mut stats = Statistics::default();
    let (earliest,latest):(Option<f64>,Option<f64>)=conn.query_row(
        "SELECT (SELECT MIN(julianday(timestamp)) FROM calls),(SELECT MAX(julianday(timestamp)) FROM calls)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let epoch_ms = |jd: f64| ((jd - 2440587.5) * 86_400_000.).round() as i64;
    stats.earliest_ms = earliest.map(epoch_ms);
    stats.latest_ms = latest.map(epoch_ms);
    stats.undated_calls = conn.query_row(
        "SELECT COUNT(*) FROM calls WHERE julianday(timestamp) IS NULL",
        [],
        |r| r.get(0),
    )?;
    let mut sessions = HashSet::new();
    let mut agents = HashSet::new();
    let mut projects = HashSet::new();
    let mut models = BTreeMap::<String, ModelStatistics>::new();
    let mut query=conn.prepare("SELECT model,input,creation,read,output,cache_1h,speed,geo,searches,session,agent_id,project,alert
        FROM calls WHERE julianday(timestamp)>=COALESCE(julianday(?1/1000.0,'unixepoch'),0)
        AND julianday(timestamp)<=julianday(?2/1000.0,'unixepoch')")?;
    let mut rows = query.query(params![start_ms, end_ms])?;
    while let Some(row) = rows.next()? {
        let model: String = row.get(0)?;
        let tokens = Tokens {
            input: row.get(1)?,
            creation: row.get(2)?,
            read: row.get(3)?,
            output: row.get(4)?,
        };
        let meta = Metadata {
            cache_1h: row.get(5)?,
            speed: row.get(6)?,
            geo: row.get(7)?,
            searches: row.get(8)?,
        };
        let session: String = row.get(9)?;
        let agent: String = row.get(10)?;
        let project: String = row.get(11)?;
        let alert: String = row.get(12)?;
        let uncached = tokens.input.saturating_add(tokens.creation);
        stats.calls += 1;
        stats.ordinary_input = stats.ordinary_input.saturating_add(tokens.input);
        stats.cache_writes = stats.cache_writes.saturating_add(tokens.creation);
        stats.cache_1h = stats.cache_1h.saturating_add(meta.cache_1h.unwrap_or(0));
        stats.cache_reads = stats.cache_reads.saturating_add(tokens.read);
        stats.output = stats.output.saturating_add(tokens.output);
        stats.uncached = stats.uncached.saturating_add(uncached);
        stats.max_uncached = stats.max_uncached.max(uncached);
        stats.total_input = stats
            .total_input
            .saturating_add(uncached.saturating_add(tokens.read));
        stats.zero_cache_calls += u64::from(tokens.read == 0);
        stats.expensive_calls += u64::from(
            uncached > settings.threshold && (!settings.zero_cache_only || tokens.read == 0),
        );
        match alert.as_str() {
            "sent" => stats.alerts_sent += 1,
            "failed" => stats.alerts_failed += 1,
            "pending" => stats.alerts_pending += 1,
            _ => {}
        }
        if !agent.is_empty() {
            agents.insert((session.clone(), agent));
        }
        if !session.is_empty() {
            sessions.insert(session);
        }
        if !project.is_empty() {
            projects.insert(project);
        }
        let by_model = models
            .entry(model.clone())
            .or_insert_with(|| ModelStatistics {
                model: model.clone(),
                ..ModelStatistics::default()
            });
        by_model.calls += 1;
        by_model.uncached_tokens = by_model.uncached_tokens.saturating_add(uncached);
        by_model.cache_read_tokens = by_model.cache_read_tokens.saturating_add(tokens.read);
        by_model.output_tokens = by_model.output_tokens.saturating_add(tokens.output);
        if let Some(estimate) = pricing::estimate(&model, tokens, &meta, &rates) {
            stats.priced_calls += 1;
            stats.estimated_usd += estimate.total_usd;
            stats.max_cost_usd = stats.max_cost_usd.max(estimate.total_usd);
            stats.cache_read_discount_usd +=
                (tokens.read as f64 * estimate.rates_per_million.input / 1e6
                    - estimate.cache_read_usd)
                    .max(0.);
            by_model.priced_calls += 1;
            by_model.estimated_usd += estimate.total_usd;
        }
    }
    stats.sessions = sessions.len();
    stats.subagents = agents.len();
    stats.projects = projects.len();
    stats.models = models.into_values().collect();
    stats
        .models
        .sort_by(|a, b| b.estimated_usd.total_cmp(&a.estimated_usd));
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Usage;
    #[test]
    fn selected_interval_is_inclusive_and_unknown_models_are_not_free() {
        let db = store::open(std::path::Path::new(":memory:")).unwrap();
        let settings = Settings::default();
        for (id, date, model, input, read) in [
            ("old", "2026-10-14T00:00:00Z", "claude-opus-5-5", 20, 0),
            (
                "start",
                "2026-10-15T00:00:00Z",
                "claude-opus-5-5",
                100001,
                50,
            ),
            ("end", "2026-10-16T00:00:00Z", "custom-model", 10, 100),
            ("after", "2026-10-17T00:00:00Z", "claude-opus-5-5", 10, 0),
        ] {
            let u=Usage::parse(serde_json::json!({"type":"assistant","requestId":id,"timestamp":date,"sessionId":"s1","agentId":"a1","cwd":"project","message":{"model":model,"usage":{"input_tokens":input,"cache_read_input_tokens":read,"output_tokens":10}}}).to_string().as_bytes()).unwrap();
            store::record(&db, &u, "test", false, &settings).unwrap();
        }
        let start = 1760486400000i64 + 31536000000; // 2026-10-15 UTC
        let s = calculate(&db, Some(start), start + 86_400_000, &settings).unwrap();
        assert_eq!(s.calls, 2);
        assert_eq!(s.priced_calls, 1);
        assert_eq!(s.expensive_calls, 1);
        assert_eq!(s.sessions, 1);
        assert_eq!(s.subagents, 1);
        assert_eq!(s.cache_reads, 150);
        assert!((s.estimated_usd - (100001. * 4. + 50. * 0.2 + 10. * 20.) / 1e6).abs() < 1e-10);
        assert_eq!(
            calculate(&db, Some(start + 1), start + 86_400_000 - 1, &settings)
                .unwrap()
                .calls,
            0
        );
    }
}
