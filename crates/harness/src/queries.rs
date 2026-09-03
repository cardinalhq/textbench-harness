// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! The Q1-Q9 constant table.
//!
//! Semantics are pinned per methodology §5: predicate serialization form,
//! token-vs-substring distinction, time range, label filters, grouping,
//! histogram bucket, ordering/limit, and expected output shape.
//!
//! Tokens are whole-token (`ExactToken`) against maximal `[A-Za-z0-9]+`,
//! case-sensitive runs. Both adapters receive this serialized contract.

use serde::{Deserialize, Serialize};

use crate::ExpectedShape;

/// Q1 window: 30-minute slice used by the Cardinal Python harness. Matches
/// `bench/tantivy/src/lib.rs` `W_START_NS` / `W_END_NS` exactly.
pub const W_START_NS: i64 = 1_758_585_600 * 1_000_000_000;
pub const W_END_NS: i64 = 1_758_587_400 * 1_000_000_000;

/// Q3/Q5 5-token disjunction.
pub const FIVE_TOKENS: &[&str] = &["error", "exception", "failed", "timeout", "refused"];
/// Q6 3-token disjunction.
pub const THREE_TOKENS: &[&str] = &["exception", "timeout", "failed"];

/// Q1-Q9 identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum QueryId {
    Q1,
    Q2,
    Q3,
    Q4,
    Q5,
    Q6,
    Q7,
    Q8,
    Q9,
}

impl QueryId {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryId::Q1 => "Q1",
            QueryId::Q2 => "Q2",
            QueryId::Q3 => "Q3",
            QueryId::Q4 => "Q4",
            QueryId::Q5 => "Q5",
            QueryId::Q6 => "Q6",
            QueryId::Q7 => "Q7",
            QueryId::Q8 => "Q8",
            QueryId::Q9 => "Q9",
        }
    }
}

/// One AND/OR predicate tree. Case-sensitive, whole-token.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Predicate {
    /// Whole-token equality against Body's tokenizer. Corresponds to
    /// tantivy's `TermQuery` and Cardinal's `hasToken(...)` /
    /// `ExactToken` arm.
    ExactToken(String),
    /// Substring test — reaches Cardinal's `SubstringUnion` arm and
    /// tantivy's `PhraseQuery`. Left in the tree so a future query can add
    /// one, though Q1-Q9 use `ExactToken` exclusively today.
    #[allow(dead_code)]
    SubstringUnion(Vec<String>),
    /// Boolean AND.
    And(Vec<Predicate>),
    /// Boolean OR.
    Or(Vec<Predicate>),
    /// Label-column exact equality (e.g. service_name = "checkout").
    LabelEq { column: String, value: String },
    /// SeverityNumber >= N.
    SeverityGe(u64),
    /// Half-open time window [lo_ns, hi_ns).
    TimeRange { lo_ns: i64, hi_ns: i64 },
    /// Match everything (identity element of AND).
    All,
}

impl Predicate {
    pub fn and(children: Vec<Predicate>) -> Self {
        Predicate::And(children)
    }
    pub fn or(children: Vec<Predicate>) -> Self {
        Predicate::Or(children)
    }
    pub fn token(s: impl Into<String>) -> Self {
        Predicate::ExactToken(s.into())
    }
    pub fn label(col: impl Into<String>, val: impl Into<String>) -> Self {
        Predicate::LabelEq {
            column: col.into(),
            value: val.into(),
        }
    }
}

/// Group-by request. Empty = ungrouped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupBy {
    pub columns: &'static [&'static str],
    /// Hourly histogram bucket in ms, `None` if no time bucketing.
    pub bucket_ms: Option<i64>,
}

impl GroupBy {
    pub const fn none() -> Self {
        Self {
            columns: &[],
            bucket_ms: None,
        }
    }
    pub const fn service() -> Self {
        Self {
            columns: &["service_name"],
            bucket_ms: None,
        }
    }
    pub const fn hourly() -> Self {
        Self {
            columns: &[],
            bucket_ms: Some(3_600_000),
        }
    }
}

/// Q1-Q9 record.
///
/// `cardinal_intended_path` and `tantivy_intended_path` are the physical
/// plan tags the harness expects the dispatcher to report. A mismatch
/// aborts the query as `FALLBACK_DETECTED`.
#[derive(Debug, Clone)]
pub struct Query {
    pub id: QueryId,
    pub description: &'static str,
    pub predicate: fn() -> Predicate,
    pub group_by: GroupBy,
    /// `Some(N)` for Q1-Q3 top-100 rows, `None` otherwise.
    pub limit: Option<usize>,
    /// Order-by `chq_tsns` DESC for Q1-Q3. `false` for the rest.
    pub reverse_chrono: bool,
    pub shape: ExpectedShape,
    pub cardinal_intended_path: &'static str,
    pub tantivy_intended_path: &'static str,
}

// -----------------------------------------------------------------------------
// Predicate constructors. Split out so the unit tests can inspect the
// serialized form without touching the QUERIES table itself.
// -----------------------------------------------------------------------------

pub fn q1_predicate() -> Predicate {
    Predicate::and(vec![
        Predicate::label("service_name", "checkout"),
        Predicate::token("failed"),
        Predicate::token("order"),
        Predicate::TimeRange {
            lo_ns: W_START_NS,
            hi_ns: W_END_NS,
        },
    ])
}

pub fn q2_predicate() -> Predicate {
    // (connection AND reset) OR timeout, gated by service=frontend and sev>=13.
    let body = Predicate::or(vec![
        Predicate::and(vec![
            Predicate::token("connection"),
            Predicate::token("reset"),
        ]),
        Predicate::token("timeout"),
    ]);
    Predicate::and(vec![
        Predicate::label("service_name", "frontend"),
        body,
        Predicate::SeverityGe(13),
        Predicate::TimeRange {
            lo_ns: W_START_NS,
            hi_ns: W_END_NS,
        },
    ])
}

pub fn q3_predicate() -> Predicate {
    Predicate::or(FIVE_TOKENS.iter().map(|t| Predicate::token(*t)).collect())
}

pub fn q4_predicate() -> Predicate {
    Predicate::token("timeout")
}

pub fn q5_predicate() -> Predicate {
    Predicate::or(FIVE_TOKENS.iter().map(|t| Predicate::token(*t)).collect())
}

pub fn q6_predicate() -> Predicate {
    Predicate::or(THREE_TOKENS.iter().map(|t| Predicate::token(*t)).collect())
}

pub fn q7_predicate() -> Predicate {
    Predicate::and(vec![
        Predicate::token("connection"),
        Predicate::token("reset"),
    ])
}

pub fn q8_predicate() -> Predicate {
    Predicate::and(vec![
        Predicate::label("service_name", "checkout"),
        Predicate::token("payment"),
    ])
}

pub fn q9_predicate() -> Predicate {
    Predicate::and(vec![
        Predicate::token("connection"),
        Predicate::token("reset"),
    ])
}

/// Physical-plan tags. Kept as `pub const` so `queries::PATH_*` name lookup
/// is trivial in the dispatcher and in tests.
pub const CARDINAL_PATH_ARCH_B_POSTINGS: &str = "arch_b_postings";
pub const CARDINAL_PATH_GENERATION_P10_5B: &str = "generation_p10_5b";
pub const CARDINAL_PATH_ARCH_B_TMS_BMD: &str = "arch_b_tms_bmd";
pub const CARDINAL_PATH_GENERATION_HOURLY: &str = "generation_hourly_exact_tokens";
/// The Cardinal contract: semantic cost planning must select a
/// representation and execution must report that same representation.  It is
/// intentionally identical for every query.
pub const CARDINAL_PATH_ADAPTIVE_PLANNER: &str = "adaptive_planner";
/// Physical-path tag stamped when the arch-B `.tms` sidecar consult did NOT
/// engage (env off, sidecar absent, unsupported needle shape, or the
/// evaluator fell back to a body scan for any part of the answer). This
/// tag NEVER equals any query's `cardinal_intended_path`, so a dispatcher
/// that returns it causes `check_path` to fire `FallbackDetected` and the
/// harness refuses to time the query. See
/// Adapters may use this label for an exact scan fallback.
pub const CARDINAL_PATH_FALLBACK_BODY_SCAN: &str = "fallback_body_scan";
pub const TANTIVY_PATH_TOP_DOCS: &str = "tantivy_top_docs";
pub const TANTIVY_PATH_COUNT: &str = "tantivy_count";
pub const TANTIVY_PATH_TERMS_AGG: &str = "tantivy_terms_agg";
pub const TANTIVY_PATH_HOUR_AGG: &str = "tantivy_hour_agg";

pub const QUERIES: [Query; 9] = [
    Query {
        id: QueryId::Q1,
        description: "top-100 checkout AND failed AND order in W",
        predicate: q1_predicate,
        group_by: GroupBy::none(),
        limit: Some(100),
        reverse_chrono: true,
        shape: ExpectedShape::Top100Rows,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_TOP_DOCS,
    },
    Query {
        id: QueryId::Q2,
        description: "top-100 frontend {(conn&reset)|timeout} sev>=13 in W",
        predicate: q2_predicate,
        group_by: GroupBy::none(),
        limit: Some(100),
        reverse_chrono: true,
        shape: ExpectedShape::Top100Rows,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_TOP_DOCS,
    },
    Query {
        id: QueryId::Q3,
        description: "top-100 5-token OR",
        predicate: q3_predicate,
        group_by: GroupBy::none(),
        limit: Some(100),
        reverse_chrono: true,
        shape: ExpectedShape::Top100Rows,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_TOP_DOCS,
    },
    Query {
        id: QueryId::Q4,
        description: "count(timeout)",
        predicate: q4_predicate,
        group_by: GroupBy::none(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::Scalar,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_COUNT,
    },
    Query {
        id: QueryId::Q5,
        description: "count(5-token OR)",
        predicate: q5_predicate,
        group_by: GroupBy::none(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::Scalar,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_COUNT,
    },
    Query {
        id: QueryId::Q6,
        description: "count(3-token OR) by service",
        predicate: q6_predicate,
        group_by: GroupBy::service(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::GroupedVector,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_TERMS_AGG,
    },
    Query {
        id: QueryId::Q7,
        description: "count(connection AND reset) by service",
        predicate: q7_predicate,
        group_by: GroupBy::service(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::GroupedVector,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_TERMS_AGG,
    },
    Query {
        id: QueryId::Q8,
        description: "hourly(checkout AND payment)",
        predicate: q8_predicate,
        group_by: GroupBy::hourly(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::HistogramVector,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_HOUR_AGG,
    },
    Query {
        id: QueryId::Q9,
        description: "hourly(connection AND reset)",
        predicate: q9_predicate,
        group_by: GroupBy::hourly(),
        limit: None,
        reverse_chrono: false,
        shape: ExpectedShape::HistogramVector,
        cardinal_intended_path: CARDINAL_PATH_ADAPTIVE_PLANNER,
        tantivy_intended_path: TANTIVY_PATH_HOUR_AGG,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Per methodology §5: predicate semantics are pinned. If any of these
    /// serialization forms drift, so does what Cardinal and Tantivy compute.
    /// A mismatch here is a semantic bug, not a test flake.
    #[test]
    fn q1_predicate_serializes_to_expected_and() {
        let p = q1_predicate();
        let s = serde_json::to_string(&p).unwrap();
        assert!(
            s.contains(r#""LabelEq":{"column":"service_name","value":"checkout"}"#),
            "{s}"
        );
        assert!(s.contains(r#""ExactToken":"failed""#), "{s}");
        assert!(s.contains(r#""ExactToken":"order""#), "{s}");
        assert!(
            s.contains(r#""TimeRange":{"lo_ns":1758585600000000000,"hi_ns":1758587400000000000}"#),
            "{s}"
        );
    }

    #[test]
    fn q2_predicate_is_dnf_with_severity() {
        let p = q2_predicate();
        // Structural check — not string-based, so key-order changes don't break it.
        match &p {
            Predicate::And(children) => {
                assert!(children.iter().any(|c| matches!(c, Predicate::LabelEq { column, value } if column == "service_name" && value == "frontend")));
                assert!(children
                    .iter()
                    .any(|c| matches!(c, Predicate::SeverityGe(13))));
                assert!(children
                    .iter()
                    .any(|c| matches!(c, Predicate::TimeRange { .. })));
                // The DNF `(conn AND reset) OR timeout` MUST be an OR whose
                // first arm is an AND(conn, reset) — this is the shape the
                // tantivy side builds and the shape the Cardinal side sends
                // as a `ContainsAnyAll` LineFilter.
                let or = children
                    .iter()
                    .find_map(|c| {
                        if let Predicate::Or(a) = c {
                            Some(a)
                        } else {
                            None
                        }
                    })
                    .expect("q2 has an OR arm");
                assert_eq!(or.len(), 2);
                match &or[0] {
                    Predicate::And(inner) => {
                        assert_eq!(inner.len(), 2);
                        assert!(matches!(&inner[0], Predicate::ExactToken(s) if s == "connection"));
                        assert!(matches!(&inner[1], Predicate::ExactToken(s) if s == "reset"));
                    }
                    other => panic!("expected And, got {other:?}"),
                }
                assert!(matches!(&or[1], Predicate::ExactToken(s) if s == "timeout"));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn q3_and_q5_disjoin_five_tokens_in_order() {
        for p in [q3_predicate(), q5_predicate()] {
            let Predicate::Or(children) = p else {
                panic!("expected Or")
            };
            let toks: Vec<&str> = children
                .iter()
                .map(|c| match c {
                    Predicate::ExactToken(s) => s.as_str(),
                    other => panic!("expected ExactToken, got {other:?}"),
                })
                .collect();
            assert_eq!(toks, FIVE_TOKENS);
        }
    }

    #[test]
    fn q4_is_single_exact_token() {
        assert_eq!(q4_predicate(), Predicate::ExactToken("timeout".into()));
    }

    #[test]
    fn q6_disjoins_three_tokens() {
        let Predicate::Or(children) = q6_predicate() else {
            panic!("expected Or")
        };
        let toks: Vec<&str> = children
            .iter()
            .map(|c| match c {
                Predicate::ExactToken(s) => s.as_str(),
                other => panic!("expected ExactToken, got {other:?}"),
            })
            .collect();
        assert_eq!(toks, THREE_TOKENS);
    }

    #[test]
    fn q7_and_q9_are_conn_and_reset() {
        for p in [q7_predicate(), q9_predicate()] {
            let Predicate::And(children) = p else {
                panic!("expected And")
            };
            assert_eq!(children.len(), 2);
            assert!(matches!(&children[0], Predicate::ExactToken(s) if s == "connection"));
            assert!(matches!(&children[1], Predicate::ExactToken(s) if s == "reset"));
        }
    }

    #[test]
    fn q8_pins_checkout_and_payment() {
        let Predicate::And(children) = q8_predicate() else {
            panic!("expected And")
        };
        assert_eq!(children.len(), 2);
        assert!(
            matches!(&children[0], Predicate::LabelEq { column, value } if column == "service_name" && value == "checkout")
        );
        assert!(matches!(&children[1], Predicate::ExactToken(s) if s == "payment"));
    }

    #[test]
    fn intended_path_tags_are_canonical() {
        for q in QUERIES {
            assert_eq!(q.cardinal_intended_path, CARDINAL_PATH_ADAPTIVE_PLANNER);
            match q.id {
                QueryId::Q1 | QueryId::Q2 | QueryId::Q3 => {
                    assert_eq!(q.tantivy_intended_path, TANTIVY_PATH_TOP_DOCS);
                }
                QueryId::Q4 | QueryId::Q5 => {
                    assert_eq!(q.tantivy_intended_path, TANTIVY_PATH_COUNT);
                }
                QueryId::Q6 | QueryId::Q7 => {
                    assert_eq!(q.tantivy_intended_path, TANTIVY_PATH_TERMS_AGG);
                }
                QueryId::Q8 | QueryId::Q9 => {
                    assert_eq!(q.tantivy_intended_path, TANTIVY_PATH_HOUR_AGG);
                }
            }
        }
    }
}
