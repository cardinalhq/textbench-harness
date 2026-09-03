# Q1-Q9 benchmark methodology

This document defines when a Cardinal-vs-Tantivy result from this repository is
publishable.

## Fixed workload

Use `part_000.parquet` from
[ClickHouse/TextBench](https://github.com/ClickHouse/TextBench): one billion
OpenTelemetry log rows. Q1-Q9 are serialized by `textbench --list-queries` and
must not be reimplemented in a runner script.

The frozen semantics are:

- whole-token, case-sensitive matching over maximal ASCII-alphanumeric runs;
- Q1-Q3 return the newest 100 matching rows;
- Q4-Q5 return exact scalar counts;
- Q6-Q7 return exact non-empty `service_name` groups;
- Q8-Q9 return exact UTC hourly buckets;
- the Q1/Q2 time interval is half-open: `[2025-09-23 00:00:00,
  2025-09-23 00:30:00)`.

## Corpus identity

Both engines must ingest the same Parquet bytes and preserve the same source row
order. Record a cryptographic digest as `corpus_id`; both protocol handshakes
must return it.

Tantivy's builder stores the original Parquet row ordinal in `RowIdentity`.
Cardinal's preparation path must carry that ordinal into LKRN. Q1-Q3 compare the
complete ordered top-100 identity, so timestamp ties cannot hide different
answers.

Before the full run, independently verify row count and field fingerprints for
`Timestamp`, `Body`, `ServiceName`, and `SeverityNumber`. A fingerprint mismatch
invalidates every latency.

## Frozen artifacts

Record a build ID for:

- the Cardinal adapter binary;
- the LKRN bundles and selected auxiliary indexes;
- the Tantivy adapter binary;
- the Tantivy index;
- this harness commit.

Do not rebuild, modify, or swap an artifact during the run.

## Correctness before timing

For each query the harness performs one untimed call to each engine and applies
the shape-specific exact comparator. If parity fails, it records no timing for
that query.

The comparison is exact for all result shapes. `matched_rows` is diagnostic and
is not compared for Q1-Q3 because early-terminating and exhaustive collectors
count different amounts of work while returning the same top 100.

## Physical-path evidence

Every Cardinal response must include both the representation selected by its
planner and the representation that executed. The harness refuses to record a
sample when `planned_path != path`.

The adapter should expose enough counters to audit the path: object opens,
dictionary lookups, bitmap operations, rows visited/materialized, worker count,
and fallback counters where available. `external_slicing` and `benchmark_pool`
must be false for a production-path claim.

The logical query ID is only a label. It must not enter Cardinal's planner input.
A planner implementation should include tests that changing only the label does
not alter the plan.

## Timing

Run on one dedicated machine in one sitting:

1. Start both long-lived adapters and finish index opening before `hello`.
2. For each query, run the untimed parity calls.
3. Run Cardinal then Tantivy once and discard both warmups.
4. Retain at least seven interleaved samples; ten is the default.
5. Report p50 and p95 request/response latency in microseconds.

Do not mix internal engine time with protocol round-trip time. Internal phase
counters are attribution only. Do not run builds or unrelated workloads during
the query window.

Record instance type, CPU, RAM, kernel, filesystem, attached-volume settings,
UTC start/end, and whether SMT or NUMA controls differ from the canonical host.

## Build and storage

Query latency is only one part of an index result. Also publish:

- wall-clock build duration;
- user plus system CPU seconds;
- peak RSS;
- data bytes, auxiliary-index bytes, and total serving bytes;
- exactly which artifacts the canonical planner selected.

CPU efficiency is:

```text
events per CPU-core-second = rows / (user CPU seconds + system CPU seconds)
```

Unused experimental indexes must not be silently charged to the selected stack,
but their size should be disclosed separately. If one engine cannot return the
original log without retaining a separate source copy, include that copy in the
production-equivalent serving total and also show the standalone index number.

## Failure conditions

A query or run is not publishable if any of these occur:

- corpus IDs or source fingerprints differ;
- row counts differ;
- answer parity fails;
- planned and actual Cardinal paths differ;
- an engine reports the wrong identity or protocol version;
- fewer than seven measured samples are retained;
- artifacts change during the run;
- one engine includes process startup or index-open time and the other does not.

Keep failed runs. They are useful evidence, but label them as failures rather
than latency results.

## Frozen reference run

The evidence under `evidence/38e8c7ab` predates the external process boundary
and was produced by the in-tree predecessor of this harness. It used the same
query contract, exact parity gate, interleaving, warmup policy, and path-evidence
rules, but called both engines in-process. Its timings remain the source for the
published 5.05x claim. New runs from this repository use symmetric protocol
round trips and should be published as a new evidence bundle, not spliced into
the old one.
