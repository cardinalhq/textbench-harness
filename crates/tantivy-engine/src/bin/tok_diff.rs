// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Correctness gate #1: tokenizer differential.
//!
//! Rejects any deployment where the Tantivy tokenizer disagrees with the
//! Cardinal `splitByNonAlpha` contract. Two checks:
//!
//! (a) Golden fixture — hand-picked strings with expected token vectors,
//!     covering the traps: `_` as separator, punctuation, unicode, digits.
//! (b) Corpus sample — read N rows from a parquet file, run BOTH the
//!     library `for_each_token` and the registered Tantivy tokenizer, and
//!     verify they produce identical token sequences on every row.
//!
//! Any mismatch prints and exits non-zero.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Result};
use arrow_array::{Array, StringArray};
use clap::Parser;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Deserialize;
use tantivy::tokenizer::{TokenStream, Tokenizer};

use textbench_tantivy::{for_each_token, tokenize_to_vec, SplitByNonAlphaTokenizer};

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "bench/tantivy/fixtures/tokenizer_golden.jsonl")]
    golden: PathBuf,

    #[arg(long)]
    corpus: Option<PathBuf>,

    #[arg(long, default_value_t = 1_000_000)]
    corpus_sample: usize,
}

#[derive(Deserialize)]
struct GoldenCase {
    input: String,
    expected: Vec<String>,
}

fn tantivy_tokens(text: &str) -> Vec<String> {
    let mut tk = SplitByNonAlphaTokenizer;
    let mut stream = tk.token_stream(text);
    let mut out = Vec::new();
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

fn run_golden(path: &PathBuf) -> Result<()> {
    let f = File::open(path).map_err(|e| anyhow!("open {}: {e}", path.display()))?;
    let br = BufReader::new(f);
    let mut fails = 0usize;
    let mut total = 0usize;
    for line in br.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        let case: GoldenCase = serde_json::from_str(&line)?;
        let lib = tokenize_to_vec(&case.input);
        let tan = tantivy_tokens(&case.input);
        if lib != case.expected {
            fails += 1;
            eprintln!(
                "LIB!=EXPECTED for {:?}\n  lib     = {:?}\n  expected= {:?}",
                case.input, lib, case.expected
            );
        }
        if tan != case.expected {
            fails += 1;
            eprintln!(
                "TANTIVY!=EXPECTED for {:?}\n  tantivy = {:?}\n  expected= {:?}",
                case.input, tan, case.expected
            );
        }
    }
    println!("golden: {} cases, {} failures", total, fails);
    if fails > 0 {
        return Err(anyhow!("golden tokenizer differential FAILED"));
    }
    Ok(())
}

fn run_corpus(path: &PathBuf, target_rows: usize) -> Result<()> {
    let f = File::open(path).map_err(|e| anyhow!("open {}: {e}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(f)?;
    let schema = builder.schema().clone();
    let body_idx = schema
        .index_of("Body")
        .map_err(|e| anyhow!("Body column not found: {e}"))?;
    let reader = builder.with_batch_size(8192).build()?;

    let started = Instant::now();
    let mut rows_checked = 0usize;
    let mut mismatches = 0usize;
    for rb in reader {
        let rb = rb?;
        let col = rb
            .column(body_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("Body is not StringArray"))?;
        for i in 0..col.len() {
            if col.is_null(i) {
                continue;
            }
            let body = col.value(i);
            let mut lib_toks: Vec<String> = Vec::new();
            for_each_token(body.as_bytes(), |t| {
                lib_toks.push(std::str::from_utf8(t).unwrap().to_string());
            });
            let tan_toks = tantivy_tokens(body);
            if lib_toks != tan_toks {
                mismatches += 1;
                if mismatches <= 5 {
                    eprintln!(
                        "MISMATCH row_seq={}: body={:?}\n  lib = {:?}\n  tan = {:?}",
                        rows_checked, body, lib_toks, tan_toks
                    );
                }
            }
            rows_checked += 1;
            if rows_checked >= target_rows {
                break;
            }
        }
        if rows_checked >= target_rows {
            break;
        }
    }
    let elapsed = started.elapsed();
    println!(
        "corpus: {} rows checked in {:.2}s, {} mismatches",
        rows_checked,
        elapsed.as_secs_f64(),
        mismatches
    );
    if mismatches > 0 {
        return Err(anyhow!("corpus tokenizer differential FAILED"));
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    run_golden(&args.golden)?;
    if let Some(corpus) = &args.corpus {
        run_corpus(corpus, args.corpus_sample)?;
    }
    println!("TOKENIZER_GATE=PASS");
    Ok(())
}
