// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Build a Tantivy index from the public TextBench Parquet source.
//!
//! Every document gets a stable source-row ordinal. The Cardinal/LKRN
//! adapter must preserve the same ordinal while converting the same Parquet
//! file; Q1-Q3 use it as the deterministic tie-break identity.
//!
//! Row-group parallel reader: each worker opens the parquet file and
//! consumes its assigned slice of row groups, feeding `IndexWriter`
//! concurrently. IndexWriter is thread-safe on `add_document`.
//!
//! Reports: wall seconds, user/sys CPU, peak RSS (from getrusage),
//! docs indexed, docs/sec, docs/core/sec, final segment count, merge
//! wall (as the `wait_merging_threads` duration). Emits JSON to stdout.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use arrow_array::{Array, Int64Array, StringArray, TimestampNanosecondArray, UInt8Array};
use clap::Parser;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::json;
use tantivy::{doc, Index, IndexWriter};

use textbench_tantivy::{build_schema, pack_row_identity, register_tokenizer};

#[derive(Parser, Debug)]
struct Args {
    /// Public TextBench Parquet file (the canonical 1B run uses part_000).
    #[arg(long)]
    parquet: PathBuf,

    #[arg(long)]
    out_dir: PathBuf,

    /// Writer indexing threads (Tantivy internal pool).
    #[arg(long, default_value_t = 24)]
    threads: usize,

    /// Reader worker threads (parquet decode + doc construction).
    #[arg(long, default_value_t = 8)]
    reader_threads: usize,

    /// Total heap budget for the writer, bytes.
    #[arg(long, default_value_t = 16 * 1024 * 1024 * 1024)]
    memory_budget: usize,

    #[arg(long, default_value_t = 8192)]
    batch_size: usize,

    /// Limit total docs indexed (0 = full corpus).
    #[arg(long, default_value_t = 0u64)]
    max_docs: u64,
}

#[derive(Clone, Copy)]
struct RowGroupJob {
    row_group: usize,
    first_row: u64,
    rows: u64,
}

#[derive(Clone, Copy)]
struct IngestConfig<'a> {
    parquet_path: &'a std::path::Path,
    batch_size: usize,
    max_docs: u64,
    started: Instant,
    fields: textbench_tantivy::Fields,
}

fn getrusage_kb() -> (f64, f64, i64) {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        let ut = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 * 1e-6;
        let st = ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 * 1e-6;
        (ut, st, ru.ru_maxrss)
    }
}

fn total_bytes(dir: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                total += md.len();
            }
        }
    }
    total
}

fn worker_ingest(
    jobs: Vec<RowGroupJob>,
    writer: &IndexWriter,
    doc_count: &AtomicU64,
    config: IngestConfig<'_>,
) -> Result<()> {
    let f = std::fs::File::open(config.parquet_path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(f)?;
    let arrow_schema = builder.schema().clone();
    let ts_i = arrow_schema
        .index_of("Timestamp")
        .map_err(|e| anyhow!("Timestamp: {e}"))?;
    let sn_i = arrow_schema
        .index_of("ServiceName")
        .map_err(|e| anyhow!("ServiceName: {e}"))?;
    let bd_i = arrow_schema
        .index_of("Body")
        .map_err(|e| anyhow!("Body: {e}"))?;
    let sv_i = arrow_schema
        .index_of("SeverityNumber")
        .map_err(|e| anyhow!("SeverityNumber: {e}"))?;
    let row_groups: Vec<usize> = jobs.iter().map(|job| job.row_group).collect();
    let reader = builder
        .with_batch_size(config.batch_size)
        .with_row_groups(row_groups)
        .build()?;

    let mut job_index = 0usize;
    let mut row_in_group = 0u64;

    for rb in reader {
        let rb = rb?;
        let ts_col = rb.column(ts_i);
        let ts_ns: Vec<i64> =
            if let Some(a) = ts_col.as_any().downcast_ref::<TimestampNanosecondArray>() {
                (0..a.len()).map(|i| a.value(i)).collect()
            } else if let Some(a) = ts_col.as_any().downcast_ref::<Int64Array>() {
                (0..a.len()).map(|i| a.value(i)).collect()
            } else {
                return Err(anyhow!(
                    "Timestamp column: unexpected arrow type {:?}",
                    ts_col.data_type()
                ));
            };

        let sn = rb
            .column(sn_i)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("ServiceName not StringArray"))?;
        let bd = rb
            .column(bd_i)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("Body not StringArray"))?;
        let sv_col = rb.column(sv_i);
        let sv: Vec<u64> = if let Some(a) = sv_col.as_any().downcast_ref::<UInt8Array>() {
            (0..a.len()).map(|i| a.value(i) as u64).collect()
        } else if let Some(a) = sv_col.as_any().downcast_ref::<Int64Array>() {
            (0..a.len()).map(|i| a.value(i) as u64).collect()
        } else {
            return Err(anyhow!(
                "SeverityNumber column: unexpected type {:?}",
                sv_col.data_type()
            ));
        };

        for i in 0..rb.num_rows() {
            while job_index < jobs.len() && row_in_group == jobs[job_index].rows {
                job_index += 1;
                row_in_group = 0;
            }
            let job = jobs.get(job_index).ok_or_else(|| {
                anyhow!("Parquet reader emitted more rows than metadata declares")
            })?;
            let source_row = job.first_row + row_in_group;
            let source_row: u32 = source_row.try_into().map_err(|_| {
                anyhow!("source row ordinal exceeds u32; this adapter is for the 1B run")
            })?;
            let body = if bd.is_null(i) { "" } else { bd.value(i) };
            let svc = if sn.is_null(i) { "" } else { sn.value(i) };
            let ts = ts_ns[i];
            let sev = sv[i];
            writer.add_document(doc!(
                config.fields.body => body,
                config.fields.timestamp => ts,
                config.fields.row_identity => pack_row_identity(0, 0, source_row),
                config.fields.service_name => svc,
                config.fields.severity_number => sev,
            ))?;
            row_in_group += 1;
            let n = doc_count.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(10_000_000) {
                let el = config.started.elapsed().as_secs_f64();
                eprintln!(
                    "  progress: {} docs, {:.0} docs/sec, {:.1} min elapsed",
                    n,
                    n as f64 / el,
                    el / 60.0
                );
            }
            if config.max_docs > 0 && n >= config.max_docs {
                return Ok(());
            }
        }
        if config.max_docs > 0 && doc_count.load(Ordering::Relaxed) >= config.max_docs {
            return Ok(());
        }
    }
    if job_index + 1 != jobs.len()
        || jobs
            .get(job_index)
            .is_none_or(|job| row_in_group != job.rows)
    {
        return Err(anyhow!(
            "Parquet reader emitted fewer rows than metadata declares"
        ));
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    let parquet_path = args.parquet.clone();

    std::fs::create_dir_all(&args.out_dir)?;
    // Clear any pre-existing files so bytes measurement is clean.
    if let Ok(rd) = std::fs::read_dir(&args.out_dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(&args.out_dir, schema)?;
    register_tokenizer(&index);

    let writer: IndexWriter = index.writer_with_num_threads(args.threads, args.memory_budget)?;

    // Discover row group count.
    let f = std::fs::File::open(&parquet_path)?;
    let metadata = ParquetRecordBatchReaderBuilder::try_new(f)?
        .metadata()
        .clone();
    let n_row_groups = metadata.num_row_groups();
    eprintln!("parquet row groups: {}", n_row_groups);

    let started = Instant::now();
    let doc_count = Arc::new(AtomicU64::new(0));
    let writer_arc = Arc::new(writer);

    // Round-robin row groups across reader threads.
    let mut per_worker: Vec<Vec<RowGroupJob>> =
        (0..args.reader_threads).map(|_| Vec::new()).collect();
    let mut first_row = 0u64;
    for rg in 0..n_row_groups {
        let rows: u64 = metadata.row_group(rg).num_rows().try_into()?;
        per_worker[rg % args.reader_threads].push(RowGroupJob {
            row_group: rg,
            first_row,
            rows,
        });
        first_row += rows;
    }

    std::thread::scope(|s| -> Result<()> {
        let mut handles = Vec::new();
        for rgs in per_worker.into_iter() {
            if rgs.is_empty() {
                continue;
            }
            let writer_ref = Arc::clone(&writer_arc);
            let doc_count_ref = Arc::clone(&doc_count);
            let parquet_path = parquet_path.clone();
            let batch_size = args.batch_size;
            let max_docs = args.max_docs;
            let h = s.spawn(move || {
                worker_ingest(
                    rgs,
                    &writer_ref,
                    &doc_count_ref,
                    IngestConfig {
                        parquet_path: &parquet_path,
                        batch_size,
                        max_docs,
                        started,
                        fields,
                    },
                )
            });
            handles.push(h);
        }
        for h in handles {
            match h.join() {
                Ok(res) => res?,
                Err(_) => return Err(anyhow!("reader thread panicked")),
            }
        }
        Ok(())
    })?;

    let feed_end = started.elapsed();
    eprintln!(
        "  feed complete: {} docs in {:.2}s",
        doc_count.load(Ordering::Relaxed),
        feed_end.as_secs_f64()
    );

    // Take back exclusive ownership of the writer for commit + merge wait.
    let mut writer =
        Arc::try_unwrap(writer_arc).map_err(|_| anyhow!("writer still shared after join"))?;

    let commit_start = Instant::now();
    writer.commit()?;
    let commit_ns = commit_start.elapsed();
    let merge_start = Instant::now();
    writer.wait_merging_threads()?;
    let merge_ns = merge_start.elapsed();
    let total_ns = started.elapsed();

    let index2 = Index::open_in_dir(&args.out_dir)?;
    let searchable = index2.searchable_segment_ids()?;
    let seg_count = searchable.len();

    let (ut, st, rss_kb) = getrusage_kb();
    let idx_bytes = total_bytes(&args.out_dir);
    let n_docs = doc_count.load(Ordering::Relaxed);

    let effective_cores = (args.threads + args.reader_threads) as f64;
    let out = json!({
        "docs": n_docs,
        "wall_s": total_ns.as_secs_f64(),
        "feed_s": feed_end.as_secs_f64(),
        "commit_s": commit_ns.as_secs_f64(),
        "merge_wait_s": merge_ns.as_secs_f64(),
        "user_cpu_s": ut,
        "sys_cpu_s": st,
        "peak_rss_kb": rss_kb,
        "docs_per_sec": n_docs as f64 / total_ns.as_secs_f64(),
        "docs_per_core_sec": n_docs as f64 / total_ns.as_secs_f64() / effective_cores,
        "final_segments": seg_count,
        "index_bytes": idx_bytes,
        "writer_threads": args.threads,
        "reader_threads": args.reader_threads,
        "memory_budget": args.memory_budget,
        "parquet_path": parquet_path.display().to_string(),
        "out_dir": args.out_dir.display().to_string(),
        "tantivy": textbench_tantivy::tantivy_version(),
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
