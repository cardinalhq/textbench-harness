// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! TextBench Q1-Q9 as library functions.
//!
//! Every function returns the canonical JSON answer plus a diagnostic
//! `matched_rows` counter. Timing is owned by the protocol adapter and harness.

use std::ops::Bound;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use tantivy::aggregation::agg_req::Aggregations;
use tantivy::aggregation::AggregationCollector;
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::IndexRecordOption;
use tantivy::{DocId, Index, Score, Term};

use crate::{
    build_schema, register_tokenizer, unpack_row_identity, Fields, FIVE_TOKENS, THREE_TOKENS,
    W_END_NS, W_START_NS,
};

// -----------------------------------------------------------------------------
// Query primitives — identical byte-for-byte to `src/bin/q.rs`.
// -----------------------------------------------------------------------------

fn body_term(fields: &Fields, s: &str) -> Term {
    Term::from_field_text(fields.body, s)
}

fn tq(field: tantivy::schema::Field, s: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, s),
        IndexRecordOption::Basic,
    ))
}

fn tq_body(fields: &Fields, tok: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        body_term(fields, tok),
        IndexRecordOption::Basic,
    ))
}

fn ts_range(fields: &Fields, lo_ns: i64, hi_ns: i64) -> Box<dyn Query> {
    let lo = Term::from_field_i64(fields.timestamp, lo_ns);
    let hi = Term::from_field_i64(fields.timestamp, hi_ns);
    Box::new(RangeQuery::new(Bound::Included(lo), Bound::Excluded(hi)))
}

fn sev_ge(fields: &Fields, floor: u64) -> Box<dyn Query> {
    let lo = Term::from_field_u64(fields.severity_number, floor);
    let hi = Term::from_field_u64(fields.severity_number, u64::MAX);
    Box::new(RangeQuery::new(Bound::Included(lo), Bound::Included(hi)))
}

fn all_tokens_query(fields: &Fields, toks: &[&str]) -> Box<dyn Query> {
    let clauses: Vec<(Occur, Box<dyn Query>)> = toks
        .iter()
        .map(|t| (Occur::Must, tq_body(fields, t)))
        .collect();
    Box::new(BooleanQuery::new(clauses))
}

fn any_tokens_query(fields: &Fields, toks: &[&str]) -> Box<dyn Query> {
    let clauses: Vec<(Occur, Box<dyn Query>)> = toks
        .iter()
        .map(|t| (Occur::Should, tq_body(fields, t)))
        .collect();
    Box::new(BooleanQuery::new(clauses))
}

fn q2_body(fields: &Fields) -> Box<dyn Query> {
    // (connection AND reset) OR timeout
    let and_pair = Box::new(BooleanQuery::new(vec![
        (Occur::Must, tq_body(fields, "connection")),
        (Occur::Must, tq_body(fields, "reset")),
    ]));
    Box::new(BooleanQuery::new(vec![
        (Occur::Should, and_pair as Box<dyn Query>),
        (Occur::Should, tq_body(fields, "timeout")),
    ]))
}

// -----------------------------------------------------------------------------
// Executors.
// -----------------------------------------------------------------------------

fn run_fetch(index: &Index, query: Box<dyn Query>) -> Result<(u64, Value)> {
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let top = TopDocs::with_limit(100).tweak_score(|segment_reader: &tantivy::SegmentReader| {
        let timestamps = segment_reader
            .fast_fields()
            .i64("Timestamp")
            .expect("Timestamp fast field")
            .first_or_default_col(0);
        let row_ids = segment_reader
            .fast_fields()
            .u64("RowIdentity")
            .expect("RowIdentity fast field")
            .first_or_default_col(0);
        move |doc: DocId, _score: Score| (timestamps.get_val(doc), row_ids.get_val(doc))
    });
    let (top_hits, matched) = searcher.search(&*query, &(top, Count))?;
    let nrows = top_hits.len() as u64;
    let top_ts: i64 = top_hits.first().map(|((ts, _), _)| *ts).unwrap_or(0);
    let top_rows: Vec<Value> = top_hits
        .iter()
        .map(|((ts, packed), _)| {
            let (bundle, partition, row) = unpack_row_identity(*packed);
            json!([ts, bundle, partition, row])
        })
        .collect();
    Ok((
        matched as u64,
        json!([["nrows", nrows], ["top_ts", top_ts], ["top_rows", top_rows]]),
    ))
}

fn run_count(index: &Index, query: Box<dyn Query>) -> Result<(u64, Value)> {
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let count = searcher.search(&*query, &Count)?;
    Ok((count as u64, json!([["count", count]])))
}

fn run_terms(index: &Index, query: Box<dyn Query>) -> Result<(u64, Value)> {
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let agg_json = json!({
        "svc": { "terms": { "field": "ServiceName", "size": 10000 } }
    });
    let aggs: Aggregations = serde_json::from_value(agg_json)?;
    let collector = AggregationCollector::from_aggs(aggs, Default::default());
    let result: Value = serde_json::to_value(searcher.search(&*query, &collector)?)?;
    let buckets = result
        .get("svc")
        .and_then(|v| v.get("buckets"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut pairs: Vec<(String, u64)> = buckets
        .iter()
        .filter_map(|b| {
            let key = b.get("key")?.as_str()?.to_string();
            if key.is_empty() {
                return None;
            }
            let cnt = b.get("doc_count")?.as_u64()?;
            Some((key, cnt))
        })
        .collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let matched: u64 = pairs.iter().map(|(_, c)| *c).sum();
    let answer: Vec<Value> = pairs.iter().map(|(k, c)| json!([k, c])).collect();
    Ok((matched, Value::Array(answer)))
}

fn run_hour_histogram(index: &Index, query: Box<dyn Query>) -> Result<(u64, Value)> {
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let agg_json = json!({
        "hour": {
            "histogram": {
                "field": "Timestamp",
                "interval": 3_600_000_000_000.0_f64
            }
        }
    });
    let aggs: Aggregations = serde_json::from_value(agg_json)?;
    let collector = AggregationCollector::from_aggs(aggs, Default::default());
    let result: Value = serde_json::to_value(searcher.search(&*query, &collector)?)?;
    let buckets = result
        .get("hour")
        .and_then(|v| v.get("buckets"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut pairs: Vec<(i64, u64)> = buckets
        .iter()
        .filter_map(|b| {
            let key_ns = b.get("key")?.as_f64()? as i64;
            let cnt = b.get("doc_count")?.as_u64()?;
            Some((key_ns / 1_000_000, cnt))
        })
        .filter(|(_, c)| *c > 0)
        .collect();
    pairs.sort_by_key(|(k, _)| *k);
    let matched: u64 = pairs.iter().map(|(_, c)| *c).sum();
    let answer: Vec<Value> = pairs.iter().map(|(k, c)| json!([k, c])).collect();
    Ok((matched, Value::Array(answer)))
}

// -----------------------------------------------------------------------------
// Public entry point.
// -----------------------------------------------------------------------------

/// Open a Tantivy index directory, register the split-by-non-alpha tokenizer,
/// and (optionally) install a multi-thread executor.
pub fn open_index(dir: &Path, threads: usize) -> Result<Index> {
    let mut index = Index::open_in_dir(dir)?;
    register_tokenizer(&index);
    if threads > 1 {
        index.set_multithread_executor(threads)?;
    }
    Ok(index)
}

/// One canonical answer for a query: matched row count and the JSON
/// answer vector in `merge()` shape.
pub struct TantivyAnswer {
    pub matched_rows: u64,
    pub answer: Value,
}

/// Dispatch Q1..Q9 by string id. Returns the answer without any timing.
pub fn run_query(index: &Index, q: &str) -> Result<TantivyAnswer> {
    let (_, fields) = build_schema();
    let (matched, answer) = match q {
        "Q1" => {
            let q = Box::new(BooleanQuery::new(vec![
                (Occur::Must, tq(fields.service_name, "checkout")),
                (Occur::Must, all_tokens_query(&fields, &["failed", "order"])),
                (Occur::Must, ts_range(&fields, W_START_NS, W_END_NS)),
            ]));
            run_fetch(index, q)?
        }
        "Q2" => {
            let q = Box::new(BooleanQuery::new(vec![
                (Occur::Must, tq(fields.service_name, "frontend")),
                (Occur::Must, q2_body(&fields)),
                (Occur::Must, sev_ge(&fields, 13)),
                (Occur::Must, ts_range(&fields, W_START_NS, W_END_NS)),
            ]));
            run_fetch(index, q)?
        }
        "Q3" => run_fetch(index, any_tokens_query(&fields, FIVE_TOKENS))?,
        "Q4" => run_count(index, tq_body(&fields, "timeout"))?,
        "Q5" => run_count(index, any_tokens_query(&fields, FIVE_TOKENS))?,
        "Q6" => run_terms(index, any_tokens_query(&fields, THREE_TOKENS))?,
        "Q7" => run_terms(index, all_tokens_query(&fields, &["connection", "reset"]))?,
        "Q8" => {
            let q = Box::new(BooleanQuery::new(vec![
                (Occur::Must, tq(fields.service_name, "checkout")),
                (Occur::Must, tq_body(&fields, "payment")),
            ]));
            run_hour_histogram(index, q)?
        }
        "Q9" => run_hour_histogram(index, all_tokens_query(&fields, &["connection", "reset"]))?,
        other => return Err(anyhow!("unknown query {other}")),
    };
    Ok(TantivyAnswer {
        matched_rows: matched,
        answer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_row_identity;
    use tantivy::doc;

    #[test]
    fn top_docs_orders_and_reports_complete_identity_on_timestamp_ties() {
        let (schema, fields) = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        for (bundle, partition, row) in [(0, 0, 1), (2, 0, 0), (1, 3, 9)] {
            writer
                .add_document(doc!(
                    fields.body => "error",
                    fields.timestamp => 42i64,
                    fields.row_identity => pack_row_identity(bundle, partition, row),
                    fields.service_name => "checkout",
                    fields.severity_number => 13u64,
                ))
                .unwrap();
        }
        writer.commit().unwrap();
        let answer = run_query(&index, "Q3").unwrap().answer;
        assert_eq!(
            answer[2][1],
            json!([[42, 2, 0, 0], [42, 1, 3, 9], [42, 0, 0, 1]])
        );
    }

    #[test]
    fn grouped_queries_drop_empty_and_missing_service_buckets() {
        let (schema, fields) = build_schema();
        let index = Index::create_in_ram(schema);
        register_tokenizer(&index);
        let mut writer = index.writer(20_000_000).unwrap();
        writer
            .add_document(doc!(
                fields.body => "connection reset",
                fields.timestamp => W_START_NS,
                fields.row_identity => pack_row_identity(0, 0, 0),
                fields.service_name => "checkout",
                fields.severity_number => 1u64,
            ))
            .unwrap();
        writer
            .add_document(doc!(
                fields.body => "connection reset",
                fields.timestamp => W_START_NS,
                fields.row_identity => pack_row_identity(0, 0, 1),
                fields.service_name => "",
                fields.severity_number => 1u64,
            ))
            .unwrap();
        writer
            .add_document(doc!(
                fields.body => "connection reset",
                fields.timestamp => W_START_NS,
                fields.row_identity => pack_row_identity(0, 0, 2),
                fields.severity_number => 1u64,
            ))
            .unwrap();
        writer.commit().unwrap();

        let answer = run_query(&index, "Q7").unwrap();
        assert_eq!(answer.matched_rows, 1);
        assert_eq!(answer.answer, json!([["checkout", 1]]));
    }
}
