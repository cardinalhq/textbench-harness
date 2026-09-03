// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Newline-delimited JSON protocol used by benchmark engines.
//!
//! The harness keeps both engine processes alive for the complete run and
//! measures one request/response round trip per sample. This keeps process
//! startup and index-open time outside every timing sample for both engines.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::queries::{Predicate, Query};
use crate::{Answer, Engine, ExpectedShape};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireGroupBy {
    pub columns: Vec<String>,
    pub bucket_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireQuery {
    /// A reporting label only. Engines must plan from the remaining fields.
    pub id: String,
    pub description: String,
    pub predicate: Predicate,
    pub group_by: WireGroupBy,
    pub limit: Option<usize>,
    pub reverse_chrono: bool,
    pub shape: ExpectedShape,
}

impl From<&Query> for WireQuery {
    fn from(q: &Query) -> Self {
        Self {
            id: q.id.as_str().to_string(),
            description: q.description.to_string(),
            predicate: (q.predicate)(),
            group_by: WireGroupBy {
                columns: q
                    .group_by
                    .columns
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
                bucket_ms: q.group_by.bucket_ms,
            },
            limit: q.limit,
            reverse_chrono: q.reverse_chrono,
            shape: q.shape,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineMetadata {
    pub engine: Engine,
    pub version: String,
    pub build_id: String,
    pub corpus_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EngineRequest {
    Hello { protocol_version: u32 },
    Run { request_id: u64, query: WireQuery },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EngineResponse {
    Hello {
        protocol_version: u32,
        metadata: EngineMetadata,
    },
    Result {
        request_id: u64,
        answer: Answer,
    },
    Error {
        request_id: Option<u64>,
        message: String,
    },
    Bye,
}

pub fn write_message<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub fn read_message<R: BufRead, T: for<'de> Deserialize<'de>>(reader: &mut R) -> Result<T> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        bail!("engine closed stdout before sending a response");
    }
    serde_json::from_str(&line).with_context(|| format!("invalid engine JSON: {line:?}"))
}

pub struct EngineProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    metadata: EngineMetadata,
    next_request_id: u64,
}

impl EngineProcess {
    pub fn spawn(expected_engine: Engine, binary: &Path, args: &[String]) -> Result<Self> {
        let mut child = Command::new(binary)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("start engine {}", binary.display()))?;
        let stdin = child.stdin.take().context("engine stdin was not piped")?;
        let stdout = child.stdout.take().context("engine stdout was not piped")?;
        let mut process = Self {
            child,
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            metadata: EngineMetadata {
                engine: expected_engine,
                version: String::new(),
                build_id: String::new(),
                corpus_id: String::new(),
            },
            next_request_id: 1,
        };

        process.send(&EngineRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        })?;
        let response: EngineResponse = process.receive()?;
        let EngineResponse::Hello {
            protocol_version,
            metadata,
        } = response
        else {
            bail!("engine did not answer hello with a hello response");
        };
        if protocol_version != PROTOCOL_VERSION {
            bail!(
                "engine protocol version {protocol_version}, harness requires {PROTOCOL_VERSION}"
            );
        }
        if metadata.engine != expected_engine {
            bail!(
                "engine identity mismatch: expected {}, got {}",
                expected_engine.as_str(),
                metadata.engine.as_str()
            );
        }
        process.metadata = metadata;
        Ok(process)
    }

    pub fn metadata(&self) -> &EngineMetadata {
        &self.metadata
    }

    pub fn run(&mut self, query: &Query) -> Result<Answer> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.send(&EngineRequest::Run {
            request_id,
            query: WireQuery::from(query),
        })?;
        match self.receive::<EngineResponse>()? {
            EngineResponse::Result {
                request_id: got,
                answer,
            } if got == request_id => {
                if answer.path_trace.engine != self.metadata.engine {
                    bail!(
                        "engine response identity mismatch: hello={}, result={}",
                        self.metadata.engine.as_str(),
                        answer.path_trace.engine.as_str()
                    );
                }
                Ok(answer)
            }
            EngineResponse::Result {
                request_id: got, ..
            } => bail!("engine response id mismatch: sent {request_id}, got {got}"),
            EngineResponse::Error {
                request_id: got,
                message,
            } => Err(anyhow!("engine error for request {got:?}: {message}")),
            other => bail!("unexpected engine response: {other:?}"),
        }
    }

    fn send(&mut self, request: &EngineRequest) -> Result<()> {
        write_message(&mut self.stdin, request)
    }

    fn receive<T: for<'de> Deserialize<'de>>(&mut self) -> Result<T> {
        read_message(&mut self.stdout)
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        if write_message(&mut self.stdin, &EngineRequest::Shutdown).is_ok() {
            let _: Result<EngineResponse> = read_message(&mut self.stdout);
        }
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QUERIES;

    #[test]
    fn wire_query_contains_semantics_not_only_an_id() {
        let wire = WireQuery::from(&QUERIES[1]);
        assert_eq!(wire.id, "Q2");
        assert!(matches!(wire.predicate, Predicate::And(_)));
        assert_eq!(wire.limit, Some(100));
        assert!(wire.reverse_chrono);
    }

    #[test]
    fn protocol_is_one_json_value_per_line() {
        let mut bytes = Vec::new();
        write_message(
            &mut bytes,
            &EngineRequest::Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .unwrap();
        assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
        let decoded: EngineRequest = read_message(&mut bytes.as_slice()).unwrap();
        assert!(matches!(
            decoded,
            EngineRequest::Hello {
                protocol_version: 1
            }
        ));
    }
}
