use crate::{
    pricing::{self, Estimate, Metadata, Tokens},
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
pub struct Metrics {
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
    pub model_count: usize,
}

#[derive(Serialize)]
pub struct TrendPoint {
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(flatten)]
    pub metrics: Metrics,
}

#[derive(Serialize)]
pub struct Trend {
    pub bucket_ms: i64,
    pub points: Vec<TrendPoint>,
}

#[derive(Serialize)]
pub struct Statistics {
    pub earliest_ms: Option<i64>,
    pub latest_ms: Option<i64>,
    pub undated_calls: i64,
    #[serde(flatten)]
    pub metrics: Metrics,
    pub models: Vec<ModelStatistics>,
    pub trend: Trend,
}

#[derive(Default)]
struct Accumulator {
    metrics: Metrics,
    sessions: HashSet<String>,
    agents: HashSet<(String, String)>,
    projects: HashSet<String>,
    models: BTreeMap<String, ModelStatistics>,
}

struct Call<'a> {
    model: &'a str,
    tokens: Tokens,
    metadata: &'a Metadata,
    session: &'a str,
    agent: &'a str,
    project: &'a str,
    alert: &'a str,
    estimate: Option<&'a Estimate>,
}

impl Accumulator {
    fn add(&mut self, call: &Call<'_>, settings: &Settings) {
        let t = call.tokens;
        let uncached = t.input.saturating_add(t.creation);
        let m = &mut self.metrics;
        m.calls += 1;
        m.ordinary_input = m.ordinary_input.saturating_add(t.input);
        m.cache_writes = m.cache_writes.saturating_add(t.creation);
        m.cache_1h = m
            .cache_1h
            .saturating_add(call.metadata.cache_1h.unwrap_or(0));
        m.cache_reads = m.cache_reads.saturating_add(t.read);
        m.output = m.output.saturating_add(t.output);
        m.uncached = m.uncached.saturating_add(uncached);
        m.max_uncached = m.max_uncached.max(uncached);
        m.total_input = m
            .total_input
            .saturating_add(uncached.saturating_add(t.read));
        m.zero_cache_calls += u64::from(t.read == 0);
        m.expensive_calls +=
            u64::from(uncached > settings.threshold && (!settings.zero_cache_only || t.read == 0));
        match call.alert {
            "sent" => m.alerts_sent += 1,
            "failed" => m.alerts_failed += 1,
            "pending" => m.alerts_pending += 1,
            _ => {}
        }
        if !call.agent.is_empty() {
            self.agents
                .insert((call.session.to_owned(), call.agent.to_owned()));
        }
        if !call.session.is_empty() {
            self.sessions.insert(call.session.to_owned());
        }
        if !call.project.is_empty() {
            self.projects.insert(call.project.to_owned());
        }
        let by_model =
            self.models
                .entry(call.model.to_owned())
                .or_insert_with(|| ModelStatistics {
                    model: call.model.to_owned(),
                    ..ModelStatistics::default()
                });
        by_model.calls += 1;
        by_model.uncached_tokens = by_model.uncached_tokens.saturating_add(uncached);
        by_model.cache_read_tokens = by_model.cache_read_tokens.saturating_add(t.read);
        by_model.output_tokens = by_model.output_tokens.saturating_add(t.output);
        if let Some(e) = call.estimate {
            m.priced_calls += 1;
            m.estimated_usd += e.total_usd;
            m.max_cost_usd = m.max_cost_usd.max(e.total_usd);
            m.cache_read_discount_usd +=
                (t.read as f64 * e.rates_per_million.input / 1e6 - e.cache_read_usd).max(0.);
            by_model.priced_calls += 1;
            by_model.estimated_usd += e.total_usd;
        }
    }
    fn finish(mut self) -> (Metrics, Vec<ModelStatistics>) {
        self.metrics.sessions = self.sessions.len();
        self.metrics.subagents = self.agents.len();
        self.metrics.projects = self.projects.len();
        self.metrics.model_count = self.models.len();
        let mut models: Vec<_> = self.models.into_values().collect();
        models.sort_by(|a, b| b.estimated_usd.total_cmp(&a.estimated_usd));
        (self.metrics, models)
    }
}

fn bucket_width(span_ms: i64) -> i64 {
    let target = span_ms.saturating_add(59) / 60;
    [
        1000, 5000, 10000, 30000, 60000, 300000, 900000, 1800000, 3600000, 10800000, 21600000,
        43200000, 86400000, 604800000, 2592000000,
    ]
    .into_iter()
    .find(|width| *width >= target)
    .unwrap_or_else(|| (target / 86400000 + 1).saturating_mul(86400000))
}

pub fn calculate(
    conn: &Connection,
    start_ms: Option<i64>,
    end_ms: i64,
    settings: &Settings,
) -> rusqlite::Result<Statistics> {
    let rates = store::load_rates(conn)?;
    let (earliest,latest):(Option<f64>,Option<f64>)=conn.query_row("SELECT (SELECT MIN(julianday(timestamp)) FROM calls),(SELECT MAX(julianday(timestamp)) FROM calls)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let epoch_ms = |jd: f64| ((jd - 2440587.5) * 86_400_000.).round() as i64;
    let earliest_ms = earliest.map(epoch_ms);
    let latest_ms = latest.map(epoch_ms);
    let undated_calls = conn.query_row(
        "SELECT COUNT(*) FROM calls WHERE julianday(timestamp) IS NULL",
        [],
        |r| r.get(0),
    )?;
    let trend_start = start_ms
        .unwrap_or(earliest_ms.unwrap_or(end_ms))
        .min(end_ms);
    let span = end_ms.saturating_sub(trend_start).max(1);
    let width = bucket_width(span);
    let count = ((span - 1) / width + 1).clamp(1, 60) as usize;
    let mut buckets: Vec<_> = (0..count).map(|_| Accumulator::default()).collect();
    let mut overall = Accumulator::default();
    let mut query=conn.prepare("SELECT model,input,creation,read,output,cache_1h,speed,geo,searches,session,agent_id,project,alert,julianday(timestamp)
        FROM calls WHERE julianday(timestamp)>=COALESCE(julianday(?1/1000.0,'unixepoch'),0) AND julianday(timestamp)<=julianday(?2/1000.0,'unixepoch')")?;
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
        let timestamp = epoch_ms(row.get(13)?);
        let estimate = pricing::estimate(&model, tokens, &meta, &rates);
        let call = Call {
            model: &model,
            tokens,
            metadata: &meta,
            session: &session,
            agent: &agent,
            project: &project,
            alert: &alert,
            estimate: estimate.as_ref(),
        };
        overall.add(&call, settings);
        let index =
            (timestamp.saturating_sub(trend_start) / width).clamp(0, count as i64 - 1) as usize;
        buckets[index].add(&call, settings);
    }
    let (metrics, models) = overall.finish();
    let points = buckets
        .into_iter()
        .enumerate()
        .map(|(i, bucket)| {
            let start = trend_start.saturating_add((i as i64).saturating_mul(width));
            TrendPoint {
                start_ms: start,
                end_ms: if i + 1 == count {
                    end_ms
                } else {
                    start.saturating_add(width - 1).min(end_ms)
                },
                metrics: bucket.finish().0,
            }
        })
        .collect();
    Ok(Statistics {
        earliest_ms,
        latest_ms,
        undated_calls,
        metrics,
        models,
        trend: Trend {
            bucket_ms: width,
            points,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Usage;
    fn record(db: &Connection, id: &str, date: &str, model: &str, input: i64, read: i64) {
        let u=Usage::parse(serde_json::json!({"type":"assistant","requestId":id,"timestamp":date,"sessionId":"s1","agentId":"a1","cwd":"project","message":{"model":model,"usage":{"input_tokens":input,"cache_read_input_tokens":read,"output_tokens":10}}}).to_string().as_bytes()).unwrap();
        store::record(db, &u, "test", false, &Settings::default()).unwrap();
    }
    #[test]
    fn selected_interval_is_inclusive_and_unknown_models_are_not_free() {
        let db = store::open(std::path::Path::new(":memory:")).unwrap();
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
            record(&db, id, date, model, input, read);
        }
        let start = 1792022400000;
        let s = calculate(&db, Some(start), start + 86_400_000, &Settings::default()).unwrap();
        assert_eq!(s.metrics.calls, 2);
        assert_eq!(s.metrics.priced_calls, 1);
        assert_eq!(s.metrics.expensive_calls, 1);
        assert_eq!(s.metrics.sessions, 1);
        assert_eq!(s.metrics.subagents, 1);
        assert_eq!(s.metrics.cache_reads, 150);
        assert!(
            (s.metrics.estimated_usd - (100001. * 4. + 50. * 0.2 + 10. * 20.) / 1e6).abs() < 1e-10
        );
        assert_eq!(
            calculate(
                &db,
                Some(start + 1),
                start + 86_400_000 - 1,
                &Settings::default()
            )
            .unwrap()
            .metrics
            .calls,
            0
        );
        assert_eq!(
            s.trend.points.iter().map(|p| p.metrics.calls).sum::<u64>(),
            s.metrics.calls
        );
        assert_eq!(s.trend.points.last().unwrap().end_ms, start + 86_400_000);
    }
    #[test]
    fn trend_buckets_fill_gaps_and_share_totals_pricing_and_distinct_counts() {
        let db = store::open(std::path::Path::new(":memory:")).unwrap();
        record(
            &db,
            "first",
            "2026-10-15T00:00:00Z",
            "claude-opus-5-5",
            100,
            0,
        );
        record(
            &db,
            "boundary",
            "2026-10-15T00:01:00Z",
            "claude-opus-5-5",
            200,
            50,
        );
        record(
            &db,
            "unpriced",
            "2026-10-15T00:03:00Z",
            "unknown-model",
            300,
            100,
        );
        let start = 1792022400000;
        let s = calculate(
            &db,
            Some(start),
            start + 3_600_000 - 1,
            &Settings::default(),
        )
        .unwrap();
        assert_eq!(s.trend.bucket_ms, 60_000);
        assert_eq!(s.trend.points.len(), 60);
        assert_eq!(s.trend.points[0].metrics.calls, 1);
        assert_eq!(s.trend.points[1].metrics.calls, 1);
        assert_eq!(s.trend.points[2].metrics.calls, 0);
        assert_eq!(s.trend.points[3].metrics.priced_calls, 0);
        assert_eq!(s.trend.points[3].metrics.calls, 1);
        assert_eq!(s.metrics.sessions, 1);
        assert_eq!(s.trend.points[0].metrics.sessions, 1);
        assert_eq!(s.trend.points[1].metrics.sessions, 1);
        assert!(
            (s.trend
                .points
                .iter()
                .map(|p| p.metrics.estimated_usd)
                .sum::<f64>()
                - s.metrics.estimated_usd)
                .abs()
                < 1e-12
        );
        assert_eq!(
            s.trend
                .points
                .iter()
                .map(|p| p.metrics.uncached)
                .sum::<i64>(),
            s.metrics.uncached
        );
    }
    #[test]
    fn empty_and_single_timestamp_intervals_are_bounded() {
        let db = store::open(std::path::Path::new(":memory:")).unwrap();
        let empty = calculate(&db, None, 1792022400000, &Settings::default()).unwrap();
        assert_eq!(empty.trend.points.len(), 1);
        assert_eq!(empty.trend.points[0].metrics.calls, 0);
        record(&db, "only", "2026-10-15T00:00:00Z", "claude-opus-5-5", 1, 0);
        let s = calculate(
            &db,
            Some(1792022400000),
            1792022400000,
            &Settings::default(),
        )
        .unwrap();
        assert_eq!(s.trend.points.len(), 1);
        assert_eq!(s.trend.points[0].metrics.calls, 1);
        assert!(
            calculate(&db, Some(0), 1792022400000, &Settings::default())
                .unwrap()
                .trend
                .points
                .len()
                <= 60
        );
    }
}
