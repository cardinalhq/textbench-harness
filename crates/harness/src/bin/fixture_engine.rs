// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Tiny protocol engine used for end-to-end harness checks.

use std::io::{self, BufReader, BufWriter};

use anyhow::{anyhow, Result};
use clap::{Parser, ValueEnum};
use textbench::naive::{self, EngineTag};
use textbench::protocol::{
    read_message, write_message, EngineMetadata, EngineRequest, EngineResponse, WireQuery,
    PROTOCOL_VERSION,
};
use textbench::{Engine, QUERIES};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum EngineArg {
    Cardinal,
    Tantivy,
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, value_enum)]
    engine: EngineArg,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (engine, tag) = match args.engine {
        EngineArg::Cardinal => (Engine::Cardinal, EngineTag::Cardinal),
        EngineArg::Tantivy => (Engine::Tantivy, EngineTag::Tantivy),
    };
    let metadata = EngineMetadata {
        engine,
        version: env!("CARGO_PKG_VERSION").into(),
        build_id: "fixture-v1".into(),
        corpus_id: "fixture-v1".into(),
    };
    let corpus = naive::small_fixture();
    let input = io::stdin();
    let output = io::stdout();
    let mut reader = BufReader::new(input.lock());
    let mut writer = BufWriter::new(output.lock());

    loop {
        match read_message::<_, EngineRequest>(&mut reader)? {
            EngineRequest::Hello { protocol_version } => {
                let response = if protocol_version == PROTOCOL_VERSION {
                    EngineResponse::Hello {
                        protocol_version: PROTOCOL_VERSION,
                        metadata: metadata.clone(),
                    }
                } else {
                    EngineResponse::Error {
                        request_id: None,
                        message: format!("unsupported protocol {protocol_version}"),
                    }
                };
                write_message(&mut writer, &response)?;
            }
            EngineRequest::Run { request_id, query } => {
                let response = match QUERIES
                    .iter()
                    .find(|candidate| WireQuery::from(*candidate) == query)
                {
                    Some(query) => EngineResponse::Result {
                        request_id,
                        answer: naive::dispatch(&corpus, query, tag),
                    },
                    None => EngineResponse::Error {
                        request_id: Some(request_id),
                        message: anyhow!("query contract is not canonical").to_string(),
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
