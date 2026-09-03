// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Canonical Q1-Q9 runner.
//!
//! Both engines are long-lived child processes speaking the same NDJSON
//! protocol. The harness owns query semantics, parity, interleaving, timing,
//! fallback detection, and report generation; engine binaries own indexing
//! and execution only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};
use clap::Parser;

use textbench::naive::{self, EngineTag};
use textbench::protocol::{EngineMetadata, EngineProcess};
use textbench::{render_table, run_one, Engine, QueryId, RunReport, RunStatus, QUERIES};

#[derive(Parser, Debug)]
#[command(
    name = "textbench",
    about = "Parity-gated Q1-Q9 benchmark runner (Cardinal/LKRN vs Tantivy)"
)]
struct Args {
    /// Long-lived Cardinal/LKRN protocol adapter binary.
    #[arg(long, required_unless_present_any = ["fixture", "list_queries"])]
    cardinal_bin: Option<PathBuf>,

    /// Argument passed verbatim to the Cardinal/LKRN adapter. Repeatable.
    #[arg(long, allow_hyphen_values = true)]
    cardinal_arg: Vec<String>,

    /// Long-lived Tantivy protocol adapter binary.
    #[arg(long, required_unless_present_any = ["fixture", "list_queries"])]
    tantivy_bin: Option<PathBuf>,

    /// Argument passed verbatim to the Tantivy adapter. Repeatable.
    #[arg(long, allow_hyphen_values = true)]
    tantivy_arg: Vec<String>,

    /// Warm samples per engine, after one discarded warmup. Use at least 7
    /// for a publishable run.
    #[arg(long, default_value_t = 10)]
    iters: usize,

    /// Restrict the run to a comma-separated subset such as Q1,Q4,Q9.
    #[arg(long)]
    only: Option<String>,

    #[arg(long)]
    json_out: Option<PathBuf>,

    #[arg(long)]
    table_out: Option<PathBuf>,

    /// Assert this corpus identity against both engine handshakes.
    #[arg(long)]
    corpus_id: Option<String>,

    /// Run the harness against its in-memory oracle; no binaries or corpus.
    #[arg(long, default_value_t = false)]
    fixture: bool,

    /// Print the frozen query contract as JSON and exit.
    #[arg(long, default_value_t = false)]
    list_queries: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_queries {
        let queries: Vec<_> = QUERIES
            .iter()
            .map(textbench::protocol::WireQuery::from)
            .collect();
        println!("{}", serde_json::to_string_pretty(&queries)?);
        return Ok(());
    }
    if args.iters == 0 {
        bail!("--iters must be at least 1");
    }

    let only = parse_only(args.only.as_deref())?;
    let started_utc = chrono::Utc::now().to_rfc3339();
    let mut engines = BTreeMap::<String, EngineMetadata>::new();

    let mut outcomes = if args.fixture {
        run_fixture(&only, args.iters)
    } else {
        let mut cardinal = EngineProcess::spawn(
            Engine::Cardinal,
            args.cardinal_bin
                .as_deref()
                .context("--cardinal-bin is required")?,
            &args.cardinal_arg,
        )?;
        let mut tantivy = EngineProcess::spawn(
            Engine::Tantivy,
            args.tantivy_bin
                .as_deref()
                .context("--tantivy-bin is required")?,
            &args.tantivy_arg,
        )?;
        validate_corpus_identity(
            cardinal.metadata(),
            tantivy.metadata(),
            args.corpus_id.as_deref(),
        )?;
        engines.insert("cardinal".into(), cardinal.metadata().clone());
        engines.insert("tantivy".into(), tantivy.metadata().clone());

        let mut rows = Vec::with_capacity(QUERIES.len());
        for q in &QUERIES {
            if !selected(&only, q.id) {
                continue;
            }
            rows.push(run_one(
                q,
                args.iters,
                || cardinal.run(q),
                || tantivy.run(q),
            ));
        }
        rows
    };

    outcomes.shrink_to_fit();
    let ended_utc = chrono::Utc::now().to_rfc3339();
    let sha = current_sha();
    let report = RunReport {
        sha: sha.clone(),
        host: collect_host_info(),
        started_utc,
        ended_utc,
        corpus_id: args.corpus_id,
        tantivy_index_id: engines.get("tantivy").map(|m| m.build_id.clone()),
        generation_build_id: engines.get("cardinal").map(|m| m.build_id.clone()),
        engines,
        iters: args.iters,
        outcomes,
    };

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let json_out = args
        .json_out
        .unwrap_or_else(|| PathBuf::from("results").join(format!("q1_q9_{sha}_{ts}.json")));
    let table_out = args
        .table_out
        .unwrap_or_else(|| PathBuf::from("results").join(format!("q1_q9_{sha}_{ts}.md")));
    if let Some(parent) = json_out.parent() {
        std::fs::create_dir_all(parent).context("create results directory")?;
    }
    if let Some(parent) = table_out.parent() {
        std::fs::create_dir_all(parent).context("create table directory")?;
    }

    std::fs::write(&json_out, serde_json::to_string_pretty(&report)?)
        .context("write JSON report")?;
    let table = render_table(&report.outcomes);
    std::fs::write(&table_out, &table).context("write Markdown report")?;
    println!("{table}");
    eprintln!("JSON:  {}", json_out.display());
    eprintln!("Table: {}", table_out.display());

    let failures: Vec<_> = report
        .outcomes
        .iter()
        .filter(|o| o.status != RunStatus::Ok)
        .map(|o| o.query.as_str())
        .collect();
    if !failures.is_empty() {
        bail!("benchmark gate failed for {}", failures.join(","));
    }
    Ok(())
}

fn run_fixture(only: &[QueryId], iters: usize) -> Vec<textbench::QueryOutcome> {
    let corpus = naive::small_fixture();
    QUERIES
        .iter()
        .filter(|q| selected(only, q.id))
        .map(|q| {
            run_one(
                q,
                iters,
                || Ok(naive::dispatch(&corpus, q, EngineTag::Cardinal)),
                || Ok(naive::dispatch(&corpus, q, EngineTag::Tantivy)),
            )
        })
        .collect()
}

fn parse_only(value: Option<&str>) -> Result<Vec<QueryId>> {
    value
        .map(|value| {
            value
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| match s.trim() {
                    "Q1" => Ok(QueryId::Q1),
                    "Q2" => Ok(QueryId::Q2),
                    "Q3" => Ok(QueryId::Q3),
                    "Q4" => Ok(QueryId::Q4),
                    "Q5" => Ok(QueryId::Q5),
                    "Q6" => Ok(QueryId::Q6),
                    "Q7" => Ok(QueryId::Q7),
                    "Q8" => Ok(QueryId::Q8),
                    "Q9" => Ok(QueryId::Q9),
                    other => bail!("unknown query {other}"),
                })
                .collect()
        })
        .transpose()
        .map(|v| v.unwrap_or_default())
}

fn selected(only: &[QueryId], id: QueryId) -> bool {
    only.is_empty() || only.contains(&id)
}

fn validate_corpus_identity(
    cardinal: &EngineMetadata,
    tantivy: &EngineMetadata,
    expected: Option<&str>,
) -> Result<()> {
    if cardinal.corpus_id.is_empty() || tantivy.corpus_id.is_empty() {
        bail!("both engines must report a non-empty corpus_id");
    }
    if cardinal.corpus_id != tantivy.corpus_id {
        bail!(
            "corpus mismatch: Cardinal reports {:?}, Tantivy reports {:?}",
            cardinal.corpus_id,
            tantivy.corpus_id
        );
    }
    if let Some(expected) = expected {
        if cardinal.corpus_id != expected {
            bail!(
                "corpus mismatch: --corpus-id={expected:?}, engines report {:?}",
                cardinal.corpus_id
            );
        }
    }
    Ok(())
}

fn current_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn collect_host_info() -> BTreeMap<String, String> {
    let mut host = BTreeMap::new();
    if let Ok(output) = Command::new("uname").arg("-a").output() {
        if output.status.success() {
            host.insert(
                "uname".into(),
                String::from_utf8_lossy(&output.stdout).trim().to_string(),
            );
        }
    }
    host.insert(
        "available_parallelism".into(),
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .to_string(),
    );
    host
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(engine: Engine, corpus: &str) -> EngineMetadata {
        EngineMetadata {
            engine,
            version: "test".into(),
            build_id: "build".into(),
            corpus_id: corpus.into(),
        }
    }

    #[test]
    fn corpus_identity_must_match() {
        let c = metadata(Engine::Cardinal, "a");
        let t = metadata(Engine::Tantivy, "b");
        assert!(validate_corpus_identity(&c, &t, None).is_err());
    }
}
