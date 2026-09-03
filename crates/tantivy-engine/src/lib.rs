// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Tantivy implementation of the frozen TextBench workload.
//!
//! Uses the TextBench `splitByNonAlpha` contract: maximal `[A-Za-z0-9]+`
//! runs, case-sensitive at query time.

use std::sync::Arc;

use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions, FAST, STRING,
};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

pub mod queries;
pub use queries::{open_index, run_query, TantivyAnswer};

pub const TOKENIZER_NAME: &str = "splitByNonAlpha";

pub const W_START_NS: i64 = 1_758_585_600 * 1_000_000_000;
pub const W_END_NS: i64 = 1_758_587_400 * 1_000_000_000;

pub const FIVE_TOKENS: &[&str] = &["error", "exception", "failed", "timeout", "refused"];
pub const THREE_TOKENS: &[&str] = &["exception", "timeout", "failed"];

// -----------------------------------------------------------------------------
// Tokenizer — the benchmark's frozen splitByNonAlpha contract.
// -----------------------------------------------------------------------------

/// Whether `b` is one byte of a postings token. `_` is NOT: matches
/// `b.is_ascii_alphanumeric()` from Cardinal.
#[inline]
pub fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}

/// Emit every maximal `[A-Za-z0-9]+` run in `body` verbatim, in order.
/// Case is preserved — the query-side `|=` on the Cardinal side is
/// case-sensitive.
#[inline]
pub fn for_each_token(body: &[u8], mut f: impl FnMut(&[u8])) {
    let n = body.len();
    let mut i = 0usize;
    while i < n {
        while i < n && !is_token_byte(body[i]) {
            i += 1;
        }
        let s = i;
        while i < n && is_token_byte(body[i]) {
            i += 1;
        }
        if i > s {
            f(&body[s..i]);
        }
    }
}

/// Convenience: collect the token stream for a text.
pub fn tokenize_to_vec(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for_each_token(body.as_bytes(), |t| {
        // Safe: token bytes are all ASCII alphanumeric.
        out.push(std::str::from_utf8(t).unwrap().to_string());
    });
    out
}

// -----------------------------------------------------------------------------
// Tantivy Tokenizer impl over the same contract.
// -----------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct SplitByNonAlphaTokenizer;

pub struct SplitByNonAlphaTokenStream {
    text: String,
    // Byte positions of pending tokens (start_offset, end_offset).
    // We pre-compute them in `token_stream` for simplicity — bodies are
    // typically short (< 4 KB) and this keeps the state machine trivial.
    offsets: Vec<(usize, usize)>,
    idx: usize,
    current: Token,
}

impl Tokenizer for SplitByNonAlphaTokenizer {
    type TokenStream<'a> = SplitByNonAlphaTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let bytes = text.as_bytes();
        let mut offsets = Vec::new();
        let mut i = 0usize;
        let n = bytes.len();
        while i < n {
            while i < n && !is_token_byte(bytes[i]) {
                i += 1;
            }
            let s = i;
            while i < n && is_token_byte(bytes[i]) {
                i += 1;
            }
            if i > s {
                offsets.push((s, i));
            }
        }
        SplitByNonAlphaTokenStream {
            text: text.to_string(),
            offsets,
            idx: 0,
            current: Token::default(),
        }
    }
}

impl TokenStream for SplitByNonAlphaTokenStream {
    fn advance(&mut self) -> bool {
        if self.idx >= self.offsets.len() {
            return false;
        }
        let (s, e) = self.offsets[self.idx];
        self.current.offset_from = s;
        self.current.offset_to = e;
        self.current.position = self.idx;
        self.current.text.clear();
        self.current.text.push_str(&self.text[s..e]);
        self.idx += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.current
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current
    }
}

// -----------------------------------------------------------------------------
// Schema.
// -----------------------------------------------------------------------------

/// Field handles for the Tantivy schema.
#[derive(Clone, Copy)]
pub struct Fields {
    pub body: Field,
    pub timestamp: Field,
    pub row_identity: Field,
    pub service_name: Field,
    pub severity_number: Field,
}

pub fn build_schema() -> (Schema, Fields) {
    let mut b = Schema::builder();

    // Body: TEXT indexed with our tokenizer; positions not needed for
    // token containment queries (Cardinal never asks about proximity).
    let body_indexing = TextFieldIndexing::default()
        .set_tokenizer(TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::Basic);
    let body_options = TextOptions::default().set_indexing_options(body_indexing);
    let body = b.add_text_field("Body", body_options);

    // Timestamp: i64 nanoseconds — FAST + INDEXED so we can range-filter
    // (Q1/Q2) AND sort DESC (Q1/Q2/Q3).
    let ts_opts = NumericOptions::default().set_fast().set_indexed();
    let timestamp = b.add_i64_field("Timestamp", ts_opts);

    // Stable source row identity used only as the deterministic secondary key
    // for Q1-Q3 timestamp ties. Layout: file:16 | group:16 | row:32.
    let row_identity = b.add_u64_field("RowIdentity", FAST);

    // ServiceName: raw STRING (no tokenization) + FAST for GROUP BY.
    let service_name = b.add_text_field("ServiceName", STRING | FAST);

    // SeverityNumber: u64 FAST + INDEXED for Q2's `>= 13`.
    let sev_opts = NumericOptions::default().set_fast().set_indexed();
    let severity_number = b.add_u64_field("SeverityNumber", sev_opts);

    let schema = b.build();
    (
        schema,
        Fields {
            body,
            timestamp,
            row_identity,
            service_name,
            severity_number,
        },
    )
}

pub fn pack_row_identity(bundle_idx: u32, partition_idx: u32, row_id: u32) -> u64 {
    assert!(
        bundle_idx <= u16::MAX as u32,
        "bundle index exceeds 16 bits"
    );
    assert!(
        partition_idx <= u16::MAX as u32,
        "partition index exceeds 16 bits"
    );
    ((bundle_idx as u64) << 48) | ((partition_idx as u64) << 32) | row_id as u64
}

pub fn unpack_row_identity(v: u64) -> (u32, u32, u32) {
    ((v >> 48) as u32, ((v >> 32) & 0xffff) as u32, v as u32)
}

/// Register the tokenizer into a Tantivy `TokenizerManager`.
pub fn register_tokenizer(index: &tantivy::Index) {
    let manager = index.tokenizers();
    manager.register(TOKENIZER_NAME, SplitByNonAlphaTokenizer);
}

// -----------------------------------------------------------------------------
// Runtime metadata — surfaced in every result JSON.
// -----------------------------------------------------------------------------

pub fn tantivy_version() -> &'static str {
    "0.27.0+266a6c48"
}

pub fn arc_str(s: &str) -> Arc<str> {
    Arc::from(s)
}
