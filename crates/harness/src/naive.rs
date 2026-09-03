// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Naive-scan oracle over a synthetic in-memory row set.
//!
//! Purpose: the anti-fabrication anchor.
//!
//! The fixture parity smoke test builds a small row set (~1000 rows), runs
//! it through THIS module twice (once as the "Cardinal" side, once as the
//! "Tantivy" side), and asserts that the harness's `parity()` returns
//! `Ok`. The test is deliberately not against the real Cardinal/Tantivy
//! engines — those are covered by the Phase B canonical run against the 1B
//! corpus — but against the reference semantics for every shape, so any
//! future change to the harness's comparators or predicate-serialization
//! rules gets caught.
//!
//! Whole-token tokenization mirrors both real engine adapters.

use serde_json::{json, Value};

use crate::queries::{GroupBy, Predicate, Query};
use crate::{Answer, Engine, PathTrace};

/// One synthetic row. Timestamps are ns; service_name is either a real
/// service string or empty (Q6/Q7 null semantic).
#[derive(Debug, Clone)]
pub struct FixtureRow {
    pub chq_tsns: i64,
    pub service_name: String,
    pub severity_number: u64,
    pub message: String,
}

pub struct NaiveCorpus {
    pub rows: Vec<FixtureRow>,
}

impl NaiveCorpus {
    pub fn new(rows: Vec<FixtureRow>) -> Self {
        Self { rows }
    }
}

// -----------------------------------------------------------------------------
// Tokenization — byte-identical to the two real engine adapters.
// -----------------------------------------------------------------------------

fn tokens(body: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let n = body.len();
    let mut i = 0usize;
    while i < n {
        while i < n && !body[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let s = i;
        while i < n && body[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i > s {
            out.push(&body[s..i]);
        }
    }
    out
}

fn has_token(body: &str, needle: &str) -> bool {
    let nb = needle.as_bytes();
    tokens(body.as_bytes()).contains(&nb)
}

// -----------------------------------------------------------------------------
// Predicate evaluator.
// -----------------------------------------------------------------------------

fn evaluate_predicate(p: &Predicate, r: &FixtureRow) -> bool {
    match p {
        Predicate::All => true,
        Predicate::ExactToken(s) => has_token(&r.message, s),
        Predicate::SubstringUnion(needles) => needles.iter().any(|n| r.message.contains(n)),
        Predicate::And(cs) => cs.iter().all(|c| evaluate_predicate(c, r)),
        Predicate::Or(cs) => cs.iter().any(|c| evaluate_predicate(c, r)),
        Predicate::LabelEq { column, value } => match column.as_str() {
            "service_name" => &r.service_name == value,
            other => panic!("naive: unsupported label column {other:?}"),
        },
        Predicate::SeverityGe(n) => r.severity_number >= *n,
        Predicate::TimeRange { lo_ns, hi_ns } => r.chq_tsns >= *lo_ns && r.chq_tsns < *hi_ns,
    }
}

// -----------------------------------------------------------------------------
// Answer shapes.
// -----------------------------------------------------------------------------

fn top_100_answer(corpus: &NaiveCorpus, matched: &[&FixtureRow], q: &Query) -> Value {
    // Sort reverse-chronological if requested (Q1-Q3 do).
    let mut m: Vec<(usize, &FixtureRow)> = matched
        .iter()
        .map(|r| {
            let ordinal = corpus
                .rows
                .iter()
                .position(|candidate| std::ptr::eq(candidate, *r))
                .expect("fixture row belongs to corpus");
            (ordinal, *r)
        })
        .collect();
    if q.reverse_chrono {
        m.sort_by_key(|(ordinal, row)| std::cmp::Reverse((row.chq_tsns, *ordinal)));
    } else {
        m.sort_by_key(|(ordinal, row)| (row.chq_tsns, *ordinal));
    }
    let limit = q.limit.unwrap_or(m.len()).min(m.len());
    let top_ts = m.first().map(|(_, r)| r.chq_tsns).unwrap_or(0);
    // Complete ordered source identities, matching both real engines.
    let top_rows: Vec<Value> = m
        .iter()
        .take(limit)
        .map(|(ordinal, row)| json!([row.chq_tsns, 0, 0, ordinal]))
        .collect();
    json!([
        ["nrows", limit as u64],
        ["top_ts", top_ts],
        ["top_rows", top_rows]
    ])
}

fn scalar_answer(matched: &[&FixtureRow]) -> Value {
    json!([["count", matched.len() as u64]])
}

fn grouped_answer(matched: &[&FixtureRow], _gb: &GroupBy) -> Value {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<String, u64> = BTreeMap::new();
    for r in matched {
        if r.service_name.is_empty() {
            continue; // documented null semantic: drop empty groups both sides
        }
        *m.entry(r.service_name.clone()).or_insert(0) += 1;
    }
    let mut pairs: Vec<(String, u64)> = m.into_iter().filter(|(_, c)| *c > 0).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Value::Array(pairs.into_iter().map(|(k, c)| json!([k, c])).collect())
}

fn histogram_answer(matched: &[&FixtureRow]) -> Value {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<i64, u64> = BTreeMap::new();
    for r in matched {
        let bucket_ms = (r.chq_tsns / 1_000_000) / 3_600_000 * 3_600_000;
        *m.entry(bucket_ms).or_insert(0) += 1;
    }
    let mut pairs: Vec<(i64, u64)> = m.into_iter().filter(|(_, c)| *c > 0).collect();
    pairs.sort_by_key(|(k, _)| *k);
    Value::Array(pairs.into_iter().map(|(k, c)| json!([k, c])).collect())
}

/// Naive-scan dispatcher used by the fixture parity test as BOTH engines.
///
/// `engine_tag` picks which intended-path tag to stamp on the returned
/// `PathTrace` so a `run_one(...)` against two naive-scan runners still
/// clears `check_path` for every Q.
pub fn dispatch(corpus: &NaiveCorpus, q: &Query, engine_tag: EngineTag) -> Answer {
    let matched: Vec<&FixtureRow> = corpus
        .rows
        .iter()
        .filter(|r| evaluate_predicate(&(q.predicate)(), r))
        .collect();
    let matched_rows = matched.len() as u64;
    let answer = match q.shape {
        crate::ExpectedShape::Top100Rows => top_100_answer(corpus, &matched, q),
        crate::ExpectedShape::Scalar => scalar_answer(&matched),
        crate::ExpectedShape::GroupedVector => grouped_answer(&matched, &q.group_by),
        crate::ExpectedShape::HistogramVector => histogram_answer(&matched),
    };
    // For the Top100Rows shape, `matched_rows` in the answer is the CAPPED
    // count (tantivy `top_hits.len()`), not the total; recompute for the
    // returned Answer.matched_rows field so downstream printouts match.
    let matched_scalar = match q.shape {
        crate::ExpectedShape::Top100Rows => {
            matched_rows.min(q.limit.map(|l| l as u64).unwrap_or(u64::MAX))
        }
        _ => matched_rows,
    };
    let path = match engine_tag {
        EngineTag::Cardinal => q.cardinal_intended_path.to_string(),
        EngineTag::Tantivy => q.tantivy_intended_path.to_string(),
    };
    let engine = match engine_tag {
        EngineTag::Cardinal => Engine::Cardinal,
        EngineTag::Tantivy => Engine::Tantivy,
    };
    Answer {
        matched_rows: matched_scalar,
        answer,
        path_trace: PathTrace::new(engine, path)
            .workers(1)
            .counter("rows_scanned", corpus.rows.len() as u64),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum EngineTag {
    Cardinal,
    Tantivy,
}

/// Small deterministic fixture: 1000 rows spanning enough surface to
/// exercise every Q shape. Used only by tests.
pub fn small_fixture() -> NaiveCorpus {
    let mut rows = Vec::with_capacity(1000);
    // Pin the base timestamp to Q1's window so time-bounded queries actually
    // match some rows. `W_START_NS` is 2025-09-23 00:00:00 UTC.
    let base = crate::queries::W_START_NS;
    let services = ["checkout", "frontend", "cart", ""];
    let messages = [
        "checkout failed for order 42 timeout",
        "connection reset by peer",
        "payment succeeded",
        "internal exception in refused state",
        "warm up: nothing to see here",
        "checkout payment succeeded",
        "frontend timeout again",
        "connection reset from frontend",
    ];
    for i in 0..1000i64 {
        // Spread rows across ~4 hourly buckets so Q8/Q9 buckets have counts.
        let chq_tsns = base + i * 10_000_000_000; // 10s apart
        let service_name = services[(i as usize) % services.len()].to_string();
        let severity_number = 5 + ((i as u64) % 15);
        let message = messages[(i as usize) % messages.len()].to_string();
        rows.push(FixtureRow {
            chq_tsns,
            service_name,
            severity_number,
            message,
        });
    }
    NaiveCorpus::new(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::QUERIES;
    use crate::{parity, run_one, ParityStatus, RunStatus};

    /// The anti-fabrication anchor: run Q1..Q9 through the naive oracle as
    /// BOTH engines; parity must pass every time. If future harness code
    /// changes break the correctness gate (or the top-100-shape surrogate
    /// diverges from the histogram/group shapes), this test catches it
    /// before the canonical box run.
    #[test]
    fn fixture_smoke_q1_q9_parity() {
        let corpus = small_fixture();
        for q in QUERIES {
            let cardinal = || Ok(dispatch(&corpus, &q, EngineTag::Cardinal));
            let tantivy = || Ok(dispatch(&corpus, &q, EngineTag::Tantivy));
            let outcome = run_one(&q, 2, cardinal, tantivy);
            assert!(
                matches!(outcome.parity, ParityStatus::Pass),
                "{:?} parity: {:?}",
                q.id,
                outcome.parity
            );
            assert_eq!(outcome.status, RunStatus::Ok, "{:?}", q.id);
            assert!(outcome.cardinal_timings.is_some());
            assert!(outcome.tantivy_timings.is_some());
        }
    }

    /// Same fixture, but flipped answer values on the Tantivy side prove
    /// the harness catches a real diff.
    #[test]
    fn fixture_detects_answer_divergence() {
        let corpus = small_fixture();
        for q in QUERIES {
            let card = dispatch(&corpus, &q, EngineTag::Cardinal);
            let mut tant = dispatch(&corpus, &q, EngineTag::Tantivy);
            // Perturb the answer just enough to force a mismatch.
            tant.answer = match tant.answer {
                Value::Array(mut a) => {
                    if let Some(first) = a.first_mut() {
                        *first = json!(["FORCED_MISMATCH", 0]);
                    } else {
                        a.push(json!(["FORCED_MISMATCH", 1]));
                    }
                    Value::Array(a)
                }
                other => other,
            };
            let err = parity(q.shape, &card.answer, &tant.answer).unwrap_err();
            assert_eq!(err.shape, q.shape);
        }
    }

    /// Row-set-fingerprint guard: Q1-Q3 answers are the three-tuple
    /// `[["nrows",N],["top_ts",TS],["top_rows",[[ts,b,p,row]×N]]]`, and the
    /// `top_rows` slot is what actually establishes ordered row identity. A
    /// pair of engines that agree on `[nrows, top_ts]` but disagree on
    /// which rows they picked must fail parity — otherwise the harness is
    /// silently timing two engines that answered different questions.
    #[test]
    fn top100_fingerprint_catches_divergent_row_set() {
        use crate::queries::QueryId;
        let corpus = small_fixture();
        for q in QUERIES
            .iter()
            .filter(|q| matches!(q.id, QueryId::Q1 | QueryId::Q2 | QueryId::Q3))
        {
            let card = dispatch(&corpus, q, EngineTag::Cardinal);
            let mut tant = dispatch(&corpus, q, EngineTag::Tantivy);
            // Mutate only one physical row id. Timestamps and their complete
            // multiset stay identical, so timestamp-only parity would pass.
            let Value::Array(mut a) = tant.answer.clone() else {
                panic!("Top100Rows answer must be a JSON array");
            };
            assert_eq!(a.len(), 3, "{:?}: expected [nrows, top_ts, top_rows]", q.id);
            let top_rows = match &mut a[2] {
                Value::Array(kv) => match kv.get_mut(1) {
                    Some(Value::Array(v)) => v,
                    other => panic!("{:?}: top_rows value must be an array, got {other:?}", q.id),
                },
                other => panic!("{:?}: top_rows slot must be a KV, got {other:?}", q.id),
            };
            assert!(!top_rows.is_empty(), "{:?}: empty top_rows", q.id);
            let row = top_rows[0].as_array_mut().expect("top row tuple");
            assert_eq!(row.len(), 4);
            row[3] = json!(row[3].as_u64().unwrap() + 1);
            tant.answer = Value::Array(a);
            let err = parity(q.shape, &card.answer, &tant.answer).unwrap_err();
            assert_eq!(err.shape, q.shape, "{:?}", q.id);
        }
    }
}
