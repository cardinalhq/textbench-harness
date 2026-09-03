// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Engine-neutral Q1-Q9 TextBench harness.
//!
//! See `docs/methodology.md`. This crate:
//!   1. Defines Q1-Q9 exactly once (`queries::QUERIES`).
//!   2. Sends the complete logical query to two long-lived engine adapters.
//!   3. Runs correctness parity vs Tantivy BEFORE timing.
//!   4. Times both engines interleaved per iteration and reports p50/p95.
//!   5. Refuses to time a query whose primitive silently fell back — the
//!      `PathTrace.path` must match `Query.intended_path`.

pub mod naive;
pub mod protocol;
pub mod queries;

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use queries::{Predicate, Query, QueryId, QUERIES};

// -----------------------------------------------------------------------------
// PathTrace — evidence the intended physical plan actually ran.
// -----------------------------------------------------------------------------

/// Which engine ran a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Cardinal,
    Tantivy,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Engine::Cardinal => "Cardinal",
            Engine::Tantivy => "Tantivy",
        }
    }
}

/// Physical plan evidence returned by a query dispatcher.
///
/// A dispatcher MUST return `path` equal to the `Query.intended_path` that
/// selected it; if it doesn't, the harness aborts that Q as
/// `FALLBACK_DETECTED`. That check is wired through
/// [`run_one`], not inferred from timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathTrace {
    pub engine: Engine,
    /// Physical path tag (e.g. `file_local_postings`, `generation`, or
    /// `tantivy_top_docs`). It must match the planner result or intended path.
    pub path: String,
    /// Representation selected by Cardinal's semantic cost planner.  The
    /// harness requires this to equal `path`; otherwise execution substituted
    /// a different representation and the sample is rejected.  Tantivy and
    /// fixture-only traces leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_path: Option<String>,
    /// Worker/task count actually used by the engine.
    pub workers: usize,
    /// True if an adapter sliced work outside the production primitive.
    pub external_slicing: bool,
    /// True if a benchmark-only thread pool was used. Production API only.
    pub benchmark_pool: bool,
    /// Optional counters the primitive exposes (bytes_touched, dictionary
    /// consults, engaged/declined). Pass-through, harness prints these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counters: BTreeMap<String, u64>,
}

impl PathTrace {
    pub fn new(engine: Engine, path: impl Into<String>) -> Self {
        Self {
            engine,
            path: path.into(),
            planned_path: None,
            workers: 1,
            external_slicing: false,
            benchmark_pool: false,
            counters: BTreeMap::new(),
        }
    }
    pub fn workers(mut self, n: usize) -> Self {
        self.workers = n;
        self
    }
    pub fn planned(mut self, path: impl Into<String>) -> Self {
        self.planned_path = Some(path.into());
        self
    }
    pub fn counter(mut self, k: &str, v: u64) -> Self {
        self.counters.insert(k.into(), v);
        self
    }
}

// -----------------------------------------------------------------------------
// Answer — one comparable output vector, plus the counters and path trace.
// -----------------------------------------------------------------------------

/// Expected output shape for a query. Selects the correctness comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpectedShape {
    /// Top-K row identity: Q1-Q3. The canonical harness compares
    /// `[["nrows", N], ["top_ts", TS_NS], ["top_rows", [[TS_NS,f,g,row] × N]]]`.
    ///
    /// `top_rows` is the actual ordered row-set fingerprint: timestamp plus
    /// a stable source `(file, group, row)` identity. Both engines preserve
    /// this tuple, so a timestamp tie at the LIMIT boundary cannot hide a
    /// substituted or reordered row.
    ///
    /// The scalar `matched_rows` counters reported alongside each engine's
    /// `Answer` are NOT part of this parity signal and are not comparable
    /// between engines: an early-terminating collector may count only visited
    /// partitions while Tantivy's `Count` collector visits every match. Both
    /// can still return the same top-K rows.
    Top100Rows,
    /// Exact scalar count: Q4/Q5.
    Scalar,
    /// Sorted `(group, count)` pairs; empty groups dropped by both engines
    /// per the one-documented-null-semantic rule. Q6/Q7.
    GroupedVector,
    /// Sorted `(bucket_ts_ms, count)` pairs on hourly boundaries in UTC.
    /// Q8/Q9.
    HistogramVector,
}

/// A materialized answer from one engine. `answer` is the canonical JSON
/// vector and is the only field used for the parity gate.
///
/// `matched_rows` is a per-engine bookkeeping counter reported alongside the
/// parity gate but is NOT itself compared. For `Top100Rows` (Q1-Q3) it is
/// definitionally incomparable across engines: Cardinal counts only rows in
/// partitions scanned before the top-K early-stop fires, Tantivy counts
/// every matching doc. See the `ExpectedShape::Top100Rows` doc for the
/// details. Do not build a parity check on this field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Answer {
    pub matched_rows: u64,
    pub answer: Value,
    pub path_trace: PathTrace,
}

// -----------------------------------------------------------------------------
// Correctness parity.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParityDiff {
    pub shape: ExpectedShape,
    pub reason: String,
    pub cardinal: Value,
    pub tantivy: Value,
}

/// Compare two engine outputs per the shape's contract.
///
/// - `Top100Rows`: field-for-field equality of the three-tuple
///   `[["nrows", N], ["top_ts", TS_NS], ["top_rows", [[i64,u32,u32,u32] × N]]]`.
///   The ordered source identities ARE the row-set fingerprint — see the
///   `ExpectedShape::Top100Rows` doc for why `matched_rows` counters cannot
///   serve this role.
/// - `Scalar`: exact equality.
/// - `GroupedVector`: sorted `(group DESC by count, then key ASC)` equality.
/// - `HistogramVector`: sorted `(bucket_ms ASC)` equality.
pub fn parity(shape: ExpectedShape, cardinal: &Value, tantivy: &Value) -> Result<(), ParityDiff> {
    if cardinal == tantivy {
        return Ok(());
    }
    Err(ParityDiff {
        shape,
        reason: match shape {
            ExpectedShape::Top100Rows => "top100 tuple mismatch".into(),
            ExpectedShape::Scalar => "scalar count mismatch".into(),
            ExpectedShape::GroupedVector => "group vectors differ".into(),
            ExpectedShape::HistogramVector => "hourly bucket vectors differ".into(),
        },
        cardinal: cardinal.clone(),
        tantivy: tantivy.clone(),
    })
}

// -----------------------------------------------------------------------------
// Fallback detection.
// -----------------------------------------------------------------------------

/// Error class the harness uses to refuse a timing when the physical plan
/// doesn't match `intended_path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackDetected {
    pub query: QueryId,
    pub engine: Engine,
    pub intended: String,
    pub actual: String,
}

impl std::fmt::Display for FallbackDetected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "FALLBACK_DETECTED {:?} on {}: expected path={:?}, got path={:?}",
            self.query,
            self.engine.as_str(),
            self.intended,
            self.actual
        )
    }
}

impl std::error::Error for FallbackDetected {}

/// Assert the physical path matches what the query declared. Called before
/// any timing sample is recorded.
pub fn check_path(
    query: QueryId,
    engine: Engine,
    intended: &str,
    got: &PathTrace,
) -> Result<(), FallbackDetected> {
    let expected = got.planned_path.as_deref().unwrap_or(intended);
    if got.path == expected {
        Ok(())
    } else {
        Err(FallbackDetected {
            query,
            engine,
            intended: expected.into(),
            actual: got.path.clone(),
        })
    }
}

// -----------------------------------------------------------------------------
// Timing — interleaved C/T per iteration, one warm-up discarded.
// -----------------------------------------------------------------------------

/// Per-engine timing samples for one query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timings {
    pub samples_us: Vec<u64>,
    pub warmup_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
}

impl Timings {
    fn from_samples(samples_us: Vec<u64>, warmup_us: u64) -> Self {
        let mut sorted = samples_us.clone();
        sorted.sort_unstable();
        let p50 = percentile(&sorted, 50);
        let p95 = percentile(&sorted, 95);
        Self {
            samples_us,
            warmup_us,
            p50_us: p50,
            p95_us: p95,
        }
    }
}

fn percentile(sorted: &[u64], pct: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // Nearest-rank; matches the methodology "p50 and p95" without
    // interpolation surprises.
    let rank = ((pct as f64 / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

fn percentile_sample(samples: &[(u64, PathTrace)], pct: u64) -> Option<(u64, PathTrace)> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered: Vec<&(u64, PathTrace)> = samples.iter().collect();
    ordered.sort_by_key(|sample| sample.0);
    let rank = ((pct as f64 / 100.0) * ordered.len() as f64).ceil() as usize;
    let sample = ordered[rank.saturating_sub(1).min(ordered.len() - 1)];
    Some((sample.0, sample.1.clone()))
}

#[cfg(test)]
mod timing_selector_tests {
    use super::*;

    #[test]
    fn even_sample_p50_uses_the_same_nearest_rank_as_timings() {
        let samples = [40, 10, 30, 20].map(|us| {
            (
                us,
                PathTrace::new(Engine::Cardinal, "p").counter("sample", us),
            )
        });
        let (us, trace) = percentile_sample(&samples, 50).unwrap();
        assert_eq!(us, 20);
        assert_eq!(trace.counters["sample"], 20);
        assert_eq!(Timings::from_samples(vec![40, 10, 30, 20], 0).p50_us, us);
    }
}

/// One dispatcher call — engine-time only, timed by the harness.
pub type Runner<'a> = Box<dyn FnMut() -> Result<Answer> + 'a>;

/// Run one query end to end: correctness parity first, then interleaved
/// warm-up + measured runs.
///
/// If `parity` fails, no timings are recorded and the return carries the
/// diff. If either engine's first call reports a path that does not match
/// its `intended_path`, the return carries `FallbackDetected`.
///
/// The two dispatchers are called ONCE for parity, then INTERLEAVED
/// (`C_iter0, T_iter0, C_iter1, T_iter1, ...`) for `iters + 1` iterations;
/// the first sample per engine is discarded as warm-up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryOutcome {
    pub query: QueryId,
    pub description: String,
    pub cardinal_intended: String,
    pub tantivy_intended: String,
    pub cardinal_path: Option<PathTrace>,
    pub tantivy_path: Option<PathTrace>,
    pub cardinal_timings: Option<Timings>,
    pub tantivy_timings: Option<Timings>,
    pub cardinal_answer: Option<Value>,
    pub tantivy_answer: Option<Value>,
    pub cardinal_matched: Option<u64>,
    pub tantivy_matched: Option<u64>,
    pub parity: ParityStatus,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ParityStatus {
    Pass,
    Fail(ParityDiff),
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RunStatus {
    Ok,
    NotRun { reason: String },
    FallbackDetected { detail: String },
    ParityFail,
    DispatchError { detail: String },
}

/// The whole per-Q pipeline. Both `card` and `tant` must be side-effect-free
/// enough that repeated calls produce the same JSON answer (both engines are
/// deterministic given a fixed corpus).
pub fn run_one<F1, F2>(q: &Query, iters: usize, mut cardinal: F1, mut tantivy: F2) -> QueryOutcome
where
    F1: FnMut() -> Result<Answer>,
    F2: FnMut() -> Result<Answer>,
{
    let base = QueryOutcome {
        query: q.id,
        description: q.description.into(),
        cardinal_intended: q.cardinal_intended_path.into(),
        tantivy_intended: q.tantivy_intended_path.into(),
        cardinal_path: None,
        tantivy_path: None,
        cardinal_timings: None,
        tantivy_timings: None,
        cardinal_answer: None,
        tantivy_answer: None,
        cardinal_matched: None,
        tantivy_matched: None,
        parity: ParityStatus::Skipped {
            reason: "not evaluated".into(),
        },
        status: RunStatus::Ok,
    };
    if iters == 0 {
        return QueryOutcome {
            status: RunStatus::DispatchError {
                detail: "iters must be at least 1".into(),
            },
            ..base
        };
    }

    // Parity call — both engines run exactly once, no timing kept.
    let card0 = match cardinal() {
        Ok(a) => a,
        Err(e) => {
            return QueryOutcome {
                status: RunStatus::DispatchError {
                    detail: format!("cardinal: {e}"),
                },
                ..base
            };
        }
    };
    let tant0 = match tantivy() {
        Ok(a) => a,
        Err(e) => {
            return QueryOutcome {
                status: RunStatus::DispatchError {
                    detail: format!("tantivy: {e}"),
                },
                cardinal_path: Some(card0.path_trace.clone()),
                ..base
            };
        }
    };

    // Path checks — before parity, before timing.
    if let Err(fb) = check_path(
        q.id,
        Engine::Cardinal,
        q.cardinal_intended_path,
        &card0.path_trace,
    ) {
        return QueryOutcome {
            cardinal_path: Some(card0.path_trace.clone()),
            tantivy_path: Some(tant0.path_trace.clone()),
            status: RunStatus::FallbackDetected {
                detail: fb.to_string(),
            },
            ..base
        };
    }
    if let Err(fb) = check_path(
        q.id,
        Engine::Tantivy,
        q.tantivy_intended_path,
        &tant0.path_trace,
    ) {
        return QueryOutcome {
            cardinal_path: Some(card0.path_trace.clone()),
            tantivy_path: Some(tant0.path_trace.clone()),
            status: RunStatus::FallbackDetected {
                detail: fb.to_string(),
            },
            ..base
        };
    }

    // Parity gate.
    let parity_status = match parity(q.shape, &card0.answer, &tant0.answer) {
        Ok(()) => ParityStatus::Pass,
        Err(d) => ParityStatus::Fail(d),
    };
    if let ParityStatus::Fail(_) = &parity_status {
        return QueryOutcome {
            cardinal_path: Some(card0.path_trace.clone()),
            tantivy_path: Some(tant0.path_trace.clone()),
            cardinal_answer: Some(card0.answer.clone()),
            tantivy_answer: Some(tant0.answer.clone()),
            cardinal_matched: Some(card0.matched_rows),
            tantivy_matched: Some(tant0.matched_rows),
            parity: parity_status,
            status: RunStatus::ParityFail,
            ..base
        };
    }

    // Timed loop — interleave C/T; discard first sample per engine as warmup.
    let n = iters + 1;
    let mut c_samples: Vec<(u64, PathTrace)> = Vec::with_capacity(n);
    let mut t_samples: Vec<(u64, PathTrace)> = Vec::with_capacity(n);
    for _ in 0..n {
        let s = Instant::now();
        let card_sample = match cardinal() {
            Ok(answer) => answer,
            Err(e) => {
                return QueryOutcome {
                    cardinal_path: Some(card0.path_trace.clone()),
                    tantivy_path: Some(tant0.path_trace.clone()),
                    status: RunStatus::DispatchError {
                        detail: format!("cardinal iter: {e}"),
                    },
                    ..base
                }
            }
        };
        if let Err(fb) = check_path(
            q.id,
            Engine::Cardinal,
            q.cardinal_intended_path,
            &card_sample.path_trace,
        ) {
            return QueryOutcome {
                cardinal_path: Some(card_sample.path_trace),
                tantivy_path: Some(tant0.path_trace.clone()),
                status: RunStatus::FallbackDetected {
                    detail: fb.to_string(),
                },
                ..base
            };
        }
        c_samples.push((s.elapsed().as_micros() as u64, card_sample.path_trace));
        let s = Instant::now();
        let tant_sample = match tantivy() {
            Ok(answer) => answer,
            Err(e) => {
                return QueryOutcome {
                    cardinal_path: Some(card0.path_trace.clone()),
                    tantivy_path: Some(tant0.path_trace.clone()),
                    status: RunStatus::DispatchError {
                        detail: format!("tantivy iter: {e}"),
                    },
                    ..base
                }
            }
        };
        if let Err(fb) = check_path(
            q.id,
            Engine::Tantivy,
            q.tantivy_intended_path,
            &tant_sample.path_trace,
        ) {
            return QueryOutcome {
                cardinal_path: Some(card0.path_trace.clone()),
                tantivy_path: Some(tant_sample.path_trace),
                status: RunStatus::FallbackDetected {
                    detail: fb.to_string(),
                },
                ..base
            };
        }
        t_samples.push((s.elapsed().as_micros() as u64, tant_sample.path_trace));
    }
    let c_warm = c_samples[0].0;
    let t_warm = t_samples[0].0;
    let c_rest: Vec<u64> = c_samples[1..].iter().map(|sample| sample.0).collect();
    let t_rest: Vec<u64> = t_samples[1..].iter().map(|sample| sample.0).collect();

    // Pair the published phase counters with the same invocation that
    // supplied p50 wall time. The old harness kept `card0` — an earlier,
    // untimed correctness pass — so cold-page phases could be printed beside
    // a warm p50 and look as if they were one measurement.
    let (c_p50, mut cardinal_path) = percentile_sample(&c_samples[1..], 50).expect("iters >= 1");
    cardinal_path
        .counters
        .insert("timed_sample_micros".to_string(), c_p50);

    let (t_p50, mut tantivy_path) = percentile_sample(&t_samples[1..], 50).expect("iters >= 1");
    tantivy_path
        .counters
        .insert("timed_sample_micros".to_string(), t_p50);

    QueryOutcome {
        cardinal_path: Some(cardinal_path),
        tantivy_path: Some(tantivy_path),
        cardinal_timings: Some(Timings::from_samples(c_rest, c_warm)),
        tantivy_timings: Some(Timings::from_samples(t_rest, t_warm)),
        cardinal_answer: Some(card0.answer),
        tantivy_answer: Some(tant0.answer),
        cardinal_matched: Some(card0.matched_rows),
        tantivy_matched: Some(tant0.matched_rows),
        parity: parity_status,
        status: RunStatus::Ok,
        ..base
    }
}

/// `not_run` shortcut for a Q whose primitive is UNAVAILABLE at this SHA
/// (e.g. corpus paths not provided, primitive genuinely missing).
pub fn not_run(q: &Query, reason: impl Into<String>) -> QueryOutcome {
    QueryOutcome {
        query: q.id,
        description: q.description.into(),
        cardinal_intended: q.cardinal_intended_path.into(),
        tantivy_intended: q.tantivy_intended_path.into(),
        cardinal_path: None,
        tantivy_path: None,
        cardinal_timings: None,
        tantivy_timings: None,
        cardinal_answer: None,
        tantivy_answer: None,
        cardinal_matched: None,
        tantivy_matched: None,
        parity: ParityStatus::Skipped {
            reason: "not run".into(),
        },
        status: RunStatus::NotRun {
            reason: reason.into(),
        },
    }
}

// -----------------------------------------------------------------------------
// Winner column.
// -----------------------------------------------------------------------------

pub fn winner(outcome: &QueryOutcome) -> &'static str {
    match (
        &outcome.parity,
        &outcome.cardinal_timings,
        &outcome.tantivy_timings,
    ) {
        (ParityStatus::Pass, Some(c), Some(t)) => {
            let cp = c.p50_us as f64;
            let tp = t.p50_us as f64;
            let delta = (cp - tp).abs() / tp.max(1.0);
            if delta <= 0.05 {
                "TIE"
            } else if cp < tp {
                "C"
            } else {
                "T"
            }
        }
        _ => "-",
    }
}

// -----------------------------------------------------------------------------
// Table + JSON emitters.
// -----------------------------------------------------------------------------

pub fn render_table(outcomes: &[QueryOutcome]) -> String {
    let mut s = String::new();
    s.push_str("| Q  | Description                                                | Cardinal path              | Cardinal p50 (us) | Cardinal p95 (us) | Tantivy p50 (us) | Tantivy p95 (us) | C vs T | Parity     |\n");
    s.push_str("|----|------------------------------------------------------------|----------------------------|------------------:|------------------:|------------------:|------------------:|:------:|:-----------|\n");
    for o in outcomes {
        let cpath = o
            .cardinal_path
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| o.cardinal_intended.clone());
        let (cp50, cp95) = o
            .cardinal_timings
            .as_ref()
            .map(|t| (t.p50_us.to_string(), t.p95_us.to_string()))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        let (tp50, tp95) = o
            .tantivy_timings
            .as_ref()
            .map(|t| (t.p50_us.to_string(), t.p95_us.to_string()))
            .unwrap_or_else(|| ("-".into(), "-".into()));
        let parity_s = match &o.parity {
            ParityStatus::Pass => "PASS".to_string(),
            ParityStatus::Fail(_) => "FAIL".to_string(),
            ParityStatus::Skipped { reason } => format!("SKIP({reason})"),
        };
        let status_note = match &o.status {
            RunStatus::Ok => String::new(),
            RunStatus::NotRun { reason } => format!(" NOT_RUN:{reason}"),
            RunStatus::FallbackDetected { .. } => " FALLBACK".to_string(),
            RunStatus::ParityFail => " PARITY_FAIL".to_string(),
            RunStatus::DispatchError { detail } => format!(" ERR:{detail}"),
        };
        s.push_str(&format!(
            "| {qid:<2} | {desc:<58} | {path:<26} | {cp50:>17} | {cp95:>17} | {tp50:>17} | {tp95:>17} | {win:^6} | {parity:<10}{note}\n",
            qid = format!("{:?}", o.query),
            desc = truncate(&o.description, 58),
            path = truncate(&cpath, 26),
            cp50 = cp50,
            cp95 = cp95,
            tp50 = tp50,
            tp95 = tp95,
            win = winner(o),
            parity = parity_s,
            note = status_note,
        ));
    }
    s
}

fn truncate(s: &str, w: usize) -> String {
    if s.len() <= w {
        s.into()
    } else {
        format!("{}…", &s[..w.saturating_sub(1)])
    }
}

/// Full run report as one JSON document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub sha: String,
    pub host: BTreeMap<String, String>,
    pub started_utc: String,
    pub ended_utc: String,
    pub corpus_id: Option<String>,
    pub tantivy_index_id: Option<String>,
    pub generation_build_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub engines: BTreeMap<String, protocol::EngineMetadata>,
    pub iters: usize,
    pub outcomes: Vec<QueryOutcome>,
}
