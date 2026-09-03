// Copyright (c) 2025-2026 CardinalHQ, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Report exact bytes of a Tantivy index directory, broken down by
//! extension. No caches or build tempfiles are included — the caller is
//! expected to point us at the final committed index dir.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use serde_json::json;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "/data/tantivy_idx")]
    index_dir: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut by_ext: BTreeMap<String, u64> = BTreeMap::new();
    let mut file_count = 0u64;
    let mut total = 0u64;
    for entry in std::fs::read_dir(&args.index_dir)? {
        let e = entry?;
        let md = e.metadata()?;
        if !md.is_file() {
            continue;
        }
        file_count += 1;
        total += md.len();
        let ext = e
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "no_ext".to_string());
        *by_ext.entry(ext).or_insert(0) += md.len();
    }
    let out = json!({
        "index_dir": args.index_dir.display().to_string(),
        "total_bytes": total,
        "total_gb": (total as f64) / (1024.0 * 1024.0 * 1024.0),
        "file_count": file_count,
        "by_extension": by_ext,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}
