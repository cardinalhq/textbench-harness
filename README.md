# Cardinal TextBench harness

This repository contains the open-source harness and evidence for Cardinal's one-billion-log
benchmark against Tantivy:

- the frozen Q1-Q9 query contract;
- the Tantivy index builder and query adapter;
- an engine-neutral, parity-gated benchmark runner;
- the complete evidence bundle for the `38e8c7ab` run;
- a process protocol that lets the harness drive a released LKRN binary without
  publishing LKRN source.

The corpus is the public
[ClickHouse/TextBench](https://github.com/ClickHouse/TextBench) dataset. This
repository does not copy or redefine that corpus.

## Published result

On the frozen one-billion-row run, Cardinal beat Tantivy on all nine queries and
was **5.05x faster by geometric mean**. Cardinal's selected indexes also used
**8.45x fewer CPU-core-seconds per indexed event** and **58% fewer auxiliary
index bytes** than Tantivy.

Those claims are backed by the raw samples, host metadata, build logs, artifact
hashes, ablations, and technical report in
[`evidence/38e8c7ab`](evidence/38e8c7ab). The draft article is
[`BLOG.md`](evidence/38e8c7ab/BLOG.md); the denser audit trail is
[`REPORT.md`](evidence/38e8c7ab/REPORT.md).

## Trust boundary

The harness owns the parts that make the comparison defensible:

1. It sends the same serialized logical query to both engines.
2. It checks complete answer parity before recording a timing.
3. It keeps both engines alive and runs samples interleaved.
4. It discards one warmup, then reports p50 and p95.
5. It rejects a Cardinal sample when the planned and executed physical paths
   differ.
6. It rejects engines that report different corpus identities.

Tantivy is built from source in this repository. Cardinal is supplied as a
binary implementing the protocol in [`docs/engine-protocol.md`](docs/engine-protocol.md).
Both sides use the same long-lived protocol, so startup and index-open time are
outside every sample for both engines.

## Build and smoke test

Rust 1.96 is pinned in `rust-toolchain.toml`.

```bash
cargo build --release --workspace
cargo test --workspace

target/release/textbench \
  --cardinal-bin target/release/textbench-fixture-engine \
  --cardinal-arg=--engine --cardinal-arg=cardinal \
  --tantivy-bin target/release/textbench-fixture-engine \
  --tantivy-arg=--engine --tantivy-arg=tantivy \
  --corpus-id fixture-v1 \
  --iters 10
```

The fixture invocation crosses the real subprocess protocol and must report
`PASS` for Q1-Q9. It measures only the test oracle, not either search engine.

## Run the one-billion-row benchmark

Download `part_000.parquet` directly from the public TextBench bucket:

```bash
mkdir -p data
aws s3 cp \
  s3://public-pme/text_bench/part_000.parquet \
  data/part_000.parquet \
  --region eu-west-3 \
  --no-sign-request
```

Build the Tantivy index. The builder assigns every record its original Parquet
row ordinal; the LKRN preparation path must preserve that same ordinal for the
Q1-Q3 row-identity gate.

```bash
cargo run --release -p textbench-tantivy --bin index -- \
  --parquet data/part_000.parquet \
  --out-dir data/tantivy-index \
  --threads 24 \
  --reader-threads 8 \
  --memory-budget 17179869184
```

Choose a stable corpus digest and a stable digest for each built index. Supply
the same corpus digest to both adapters. Then run the harness:

```bash
target/release/textbench \
  --cardinal-bin /path/to/lkrn-textbench \
  --cardinal-arg=--config --cardinal-arg=/path/to/lkrn-benchmark.json \
  --tantivy-bin target/release/textbench-tantivy \
  --tantivy-arg=--index-dir --tantivy-arg=data/tantivy-index \
  --tantivy-arg=--threads --tantivy-arg=32 \
  --tantivy-arg=--corpus-id --tantivy-arg="$CORPUS_ID" \
  --tantivy-arg=--build-id --tantivy-arg="$TANTIVY_BUILD_ID" \
  --corpus-id "$CORPUS_ID" \
  --iters 10
```

The Cardinal binary is intentionally not included here. Its configuration must
point at the LKRN corpus, sidecars, and generation manifest built from the same
logical rows. The protocol handshake records its version, build ID, and corpus
ID in every result.

See [`docs/methodology.md`](docs/methodology.md) before treating a run as
publishable.

## Repository layout

```text
crates/harness/          query contract, protocol client, parity/timing gates
crates/tantivy-engine/   Tantivy tokenizer, index builder, and protocol server
docs/                    public methodology and engine protocol
evidence/38e8c7ab/       frozen raw evidence behind the published result
```

## License

Apache License 2.0. The TextBench corpus and upstream benchmark repository have
their own licensing terms.
