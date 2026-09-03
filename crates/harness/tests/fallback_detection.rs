// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Fallback detection: if a dispatcher reports a `PathTrace.path` that
//! doesn't match `Query.<engine>_intended_path`, the harness must refuse
//! to time that query.
//!
//! A mock Cardinal primitive returns the WRONG path tag; the harness
//! MUST return `RunStatus::FallbackDetected` and both timings vectors
//! must be `None`.

use serde_json::json;
use std::cell::Cell;
use textbench::naive::{self, EngineTag};
use textbench::queries::QUERIES;
use textbench::{run_one, Answer, Engine, PathTrace, RunStatus};

#[test]
fn fallback_detected_when_cardinal_path_lies() {
    let fx = naive::small_fixture();
    for q in QUERIES {
        let card = || {
            // Return a correct answer, but a WRONG path tag — as if a
            // fallback primitive quietly answered without engaging the
            // intended postings/generation plan.
            let mut a: Answer = naive::dispatch(&fx, &q, EngineTag::Cardinal);
            a.path_trace = PathTrace::new(Engine::Cardinal, "regex_full_scan_fallback");
            Ok(a)
        };
        let tant = || Ok(naive::dispatch(&fx, &q, EngineTag::Tantivy));
        let outcome = run_one(&q, 2, card, tant);
        match outcome.status {
            RunStatus::FallbackDetected { detail } => {
                assert!(
                    detail.contains("regex_full_scan_fallback"),
                    "detail={detail}"
                );
                assert!(detail.contains(q.cardinal_intended_path), "detail={detail}");
            }
            other => panic!("{:?} expected FallbackDetected, got {other:?}", q.id),
        }
        assert!(outcome.cardinal_timings.is_none(), "{:?}", q.id);
        assert!(outcome.tantivy_timings.is_none(), "{:?}", q.id);
    }
}

#[test]
fn fallback_detected_when_tantivy_path_lies() {
    let fx = naive::small_fixture();
    for q in QUERIES {
        let card = || Ok(naive::dispatch(&fx, &q, EngineTag::Cardinal));
        let tant = || {
            let mut a: Answer = naive::dispatch(&fx, &q, EngineTag::Tantivy);
            a.path_trace = PathTrace::new(Engine::Tantivy, "bogus_fallback");
            Ok(a)
        };
        let outcome = run_one(&q, 2, card, tant);
        match outcome.status {
            RunStatus::FallbackDetected { .. } => {}
            other => panic!("{:?} expected FallbackDetected, got {other:?}", q.id),
        }
    }
}

/// Correctness gate: parity mismatch is not "less severe than fallback";
/// it's ALSO a refusal to time. This test perturbs the JSON so
/// `ParityFail` fires; both timings must stay `None`.
#[test]
fn parity_fail_also_refuses_timing() {
    let fx = naive::small_fixture();
    for q in QUERIES {
        let card = || Ok(naive::dispatch(&fx, &q, EngineTag::Cardinal));
        let tant = || {
            let mut a = naive::dispatch(&fx, &q, EngineTag::Tantivy);
            // Force a shape-appropriate divergence.
            a.answer = json!([["FORCED_MISMATCH", 0]]);
            Ok(a)
        };
        let outcome = run_one(&q, 2, card, tant);
        assert!(
            matches!(outcome.status, RunStatus::ParityFail),
            "{:?}",
            q.id
        );
        assert!(outcome.cardinal_timings.is_none(), "{:?}", q.id);
        assert!(outcome.tantivy_timings.is_none(), "{:?}", q.id);
    }
}

#[test]
fn fallback_in_a_timed_sample_refuses_all_timings() {
    let fx = naive::small_fixture();
    let q = &QUERIES[0];
    let calls = Cell::new(0usize);
    let card = || {
        let call = calls.get();
        calls.set(call + 1);
        let mut answer = naive::dispatch(&fx, q, EngineTag::Cardinal);
        // Call 0 is the parity gate. Call 1 is the discarded timed warmup.
        if call == 1 {
            answer.path_trace = PathTrace::new(Engine::Cardinal, "timed_fallback");
        }
        Ok(answer)
    };
    let tant = || Ok(naive::dispatch(&fx, q, EngineTag::Tantivy));
    let outcome = run_one(q, 2, card, tant);
    assert!(matches!(outcome.status, RunStatus::FallbackDetected { .. }));
    assert!(outcome.cardinal_timings.is_none());
    assert!(outcome.tantivy_timings.is_none());
}

#[test]
fn zero_iterations_fails_closed_instead_of_panicking() {
    let fx = naive::small_fixture();
    let q = &QUERIES[0];
    let outcome = run_one(
        q,
        0,
        || Ok(naive::dispatch(&fx, q, EngineTag::Cardinal)),
        || Ok(naive::dispatch(&fx, q, EngineTag::Tantivy)),
    );
    assert!(matches!(outcome.status, RunStatus::DispatchError { .. }));
    assert!(outcome.cardinal_timings.is_none());
    assert!(outcome.tantivy_timings.is_none());
}
