// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Long-lived Tantivy adapter for the TextBench NDJSON protocol.

use std::io::{self, BufReader, BufWriter};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use clap::Parser;

use textbench::protocol::{
    read_message, write_message, EngineMetadata, EngineRequest, EngineResponse, WireQuery,
    PROTOCOL_VERSION,
};
use textbench::queries::{
    Query, QueryId, TANTIVY_PATH_COUNT, TANTIVY_PATH_HOUR_AGG, TANTIVY_PATH_TERMS_AGG,
    TANTIVY_PATH_TOP_DOCS,
};
use textbench::{Answer, Engine, PathTrace, QUERIES};
use textbench_tantivy::{open_index, run_query};

#[derive(Parser, Debug)]
#[command(name = "textbench-tantivy")]
struct Args {
    #[arg(long)]
    index_dir: PathBuf,

    #[arg(long, default_value_t = 32)]
    threads: usize,

    /// Stable digest identifying the indexed logical corpus.
    #[arg(long)]
    corpus_id: String,

    /// Stable digest identifying this index build.
    #[arg(long)]
    build_id: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let index = open_index(&args.index_dir, args.threads)
        .with_context(|| format!("open Tantivy index at {}", args.index_dir.display()))?;
    let metadata = EngineMetadata {
        engine: Engine::Tantivy,
        version: format!(
            "adapter={} tantivy={}",
            env!("CARGO_PKG_VERSION"),
            textbench_tantivy::tantivy_version()
        ),
        build_id: args.build_id,
        corpus_id: args.corpus_id,
    };

    let input = io::stdin();
    let output = io::stdout();
    let mut reader = BufReader::new(input.lock());
    let mut writer = BufWriter::new(output.lock());

    loop {
        let request: EngineRequest = match read_message(&mut reader) {
            Ok(request) => request,
            Err(error) if error.to_string().contains("closed stdout") => break,
            Err(error) => return Err(error),
        };
        match request {
            EngineRequest::Hello { protocol_version } => {
                if protocol_version != PROTOCOL_VERSION {
                    write_message(
                        &mut writer,
                        &EngineResponse::Error {
                            request_id: None,
                            message: format!(
                                "protocol {protocol_version} is unsupported; expected {PROTOCOL_VERSION}"
                            ),
                        },
                    )?;
                    continue;
                }
                write_message(
                    &mut writer,
                    &EngineResponse::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        metadata: metadata.clone(),
                    },
                )?;
            }
            EngineRequest::Run { request_id, query } => {
                let response = match dispatch(&index, args.threads, &query) {
                    Ok(answer) => EngineResponse::Result { request_id, answer },
                    Err(error) => EngineResponse::Error {
                        request_id: Some(request_id),
                        message: format!("{error:#}"),
                    },
                };
                write_message(&mut writer, &response)?;
            }
            EngineRequest::Shutdown => {
                write_message(&mut writer, &EngineResponse::Bye)?;
                break;
            }
        }
    }
    Ok(())
}

fn dispatch(index: &tantivy::Index, threads: usize, wire: &WireQuery) -> Result<Answer> {
    let query = canonical_query(wire)?;
    let started = Instant::now();
    let result = run_query(index, query.id.as_str())
        .map_err(|error| anyhow!("Tantivy {}: {error}", query.id.as_str()))?;
    let elapsed_us = started.elapsed().as_micros() as u64;
    Ok(Answer {
        matched_rows: result.matched_rows,
        answer: result.answer,
        path_trace: PathTrace::new(Engine::Tantivy, intended_path(query.id))
            .workers(threads)
            .counter("engine_micros", elapsed_us),
    })
}

fn canonical_query(wire: &WireQuery) -> Result<&'static Query> {
    let query = QUERIES
        .iter()
        .find(|query| query.id.as_str() == wire.id)
        .ok_or_else(|| anyhow!("unknown query label {:?}", wire.id))?;
    let expected = WireQuery::from(query);
    if &expected != wire {
        return Err(anyhow!(
            "query contract mismatch for {}: expected {}, got {}",
            wire.id,
            serde_json::to_string(&expected)?,
            serde_json::to_string(wire)?
        ));
    }
    Ok(query)
}

fn intended_path(id: QueryId) -> &'static str {
    match id {
        QueryId::Q1 | QueryId::Q2 | QueryId::Q3 => TANTIVY_PATH_TOP_DOCS,
        QueryId::Q4 | QueryId::Q5 => TANTIVY_PATH_COUNT,
        QueryId::Q6 | QueryId::Q7 => TANTIVY_PATH_TERMS_AGG,
        QueryId::Q8 | QueryId::Q9 => TANTIVY_PATH_HOUR_AGG,
    }
}
