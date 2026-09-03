# We Removed the Benchmark Hardcoding. Here Is What Was Left.

*Cardinal is 3.15x faster than ClickHouse and 5.05x faster than Tantivy by geometric mean on one billion logs. Its selected indexes use 8.45x fewer CPU-core-seconds to build than Tantivy's and 58% fewer index bytes. More importantly, the query engine—not the benchmark harness—now chooses the representation.*

I spent more than seven years working on observability at Netflix. One belief I came away with is that almost all observability data should live in object storage.

Logs are a particularly strange thing to keep on expensive infrastructure. We generate enormous amounts of them, occasionally need a tiny fraction very badly, and have almost no idea today which fraction will matter six months from now.

Agents make that tension worse. A human may run a handful of queries during an investigation. An agent can run hundreds per hypothesis, explore several hypotheses at once, and decide to look back three months because of something it learned ten seconds ago.

So our preference at Cardinal is simple: retain full-fidelity telemetry in object storage. Do not guess which logs will still deserve a hot searchable copy later.

That creates an obvious problem. S3 is a great place to keep a billion logs. It is not automatically a great place to search them.

We have spent the past several months finding out how far an object-storage-native format and query engine can go. Along the way we built several useful physical representations: file-local token postings, low-cardinality dimension bitmaps, generation-wide postings, dense dimension codes, timestamp bounds, and time masks.

Then we discovered that our benchmark was choosing some of them the wrong way.

## The result was fast, but the adaptivity was not real

Our previous TextBench result looked excellent. Cardinal beat Tantivy on all nine queries and ClickHouse on seven.

There was a serious problem with that claim: the canonical harness routed Q4 through Q7 by query number to a benchmark-only generation index. Q8 and Q9 used a different production generation path. The query engine was not looking at a logical query, comparing exact representations, and choosing the cheapest one. The harness already knew which implementation each benchmark query was supposed to exercise.

That is useful while developing a kernel. It is not an adaptive query engine.

So we deleted that path from the canonical benchmark and built the thing the architecture actually required: a universal representation planner.

The new planner receives no TextBench query ID. Its input is:

- the exact-token Boolean predicate;
- dimension filters;
- residual predicates that an index cannot prove;
- the time window;
- whether the result is ordered rows, a scalar count, a group, or a time histogram;
- concrete catalog capabilities and topology: bundles, rows, terms, dimensions, shards, artifacts, time spans, and identity coverage.

It considers three paths today:

```text
logical query
    |
    +-- file-local postings (.tms) ------> rows or aggregation over LKRN
    |
    +-- generation postings/dimensions --> count, group, or time buckets
    |
    +-- exact LKRN bundle scan ----------> universal fallback
```

For each candidate, the planner first proves that the representation can return the exact answer. Only then does it compare estimated work: object opens, dictionary lookups, bitmap operations, row visits, and output materialization.

A missing term does not become a fast zero. A partially covered generation does not get to answer most of a query. A bundle crossing an hourly boundary cannot be assigned to one hour because that would be convenient. Those candidates become ineligible and the planner falls back.

Cost can choose among correct answers. It never changes what correct means.

## How we proved it was not another lookup table

We used three kinds of proof.

First, query identity never enters the planner's types. We mutate the benchmark label from Q4 to Q9 while keeping semantics and topology unchanged; the serialized plan remains identical.

Second, a small synthetic topology chooses a bundle scan even when a generation index is available. That matters because a planner that always prefers generation whenever it can answer would just be a priority list wearing a cost-model costume.

Third, we reran the one-billion-row benchmark three ways on the same frozen `m6i.8xlarge`:

1. all representations available;
2. generation removed, forcing Q4-Q9 to reconsider file-local postings;
3. generation and postings removed, forcing Q1-Q3 to the bundle fallback.

The selected paths won every ablation:

| Query | Selected representation | Selected p50 | Next exact path | Next-path p50 | Advantage |
|---|---|---:|---|---:|---:|
| Q1 | file-local postings | 35.392 ms | bundle scan | 55.804 ms | 1.58x |
| Q2 | file-local postings | 30.846 ms | bundle scan | 92.003 ms | 2.98x |
| Q3 | file-local postings | 34.516 ms | bundle scan | 194.892 ms | 5.65x |
| Q4 | generation | 0.737 ms | file-local postings | 587.963 ms | 797.78x |
| Q5 | generation | 18.773 ms | file-local postings | 772.468 ms | 41.15x |
| Q6 | generation | 85.893 ms | file-local postings | 858.313 ms | 9.99x |
| Q7 | generation | 31.794 ms | file-local postings | 820.635 ms | 25.81x |
| Q8 | generation | 32.600 ms | file-local postings | 684.925 ms | 21.01x |
| Q9 | generation | 7.420 ms | file-local postings | 633.099 ms | 85.32x |

Every answer still matched Tantivy exactly, and every timed PathTrace recorded the planned path and the path that actually answered. A mismatch invalidates the timing.

The planner chose the right Cardinal representation on all nine queries.

The first run still did not win all nine. That exposed a production executor problem rather than a reason to retreat to harness routing.

Generation predicates already produced one independent bitmap result per shard. The answer path immediately flattened those parts into one generation-wide bitmap, then built an all-row bucket mask and intersected the two—even when the bucket owned every bundle. For Q5 that unnecessary copy and re-intersection turned roughly 11 ms of predicate work into a 144 ms query.

The generic fix was to preserve ordered shard-local row sets through token unions/intersections, dimension filters, bucket ownership, cardinality, and grouping. Different representations are merge-joined by their row ranges; disjoint shards are skipped. A whole-generation bucket feeds the predicate directly to count or group. No query name is involved, and no artifact format changed.

## The honest query table

Cardinal and Tantivy are ten-sample warm p50 engine times from one interleaved process. ClickHouse is the better of its two published warm executions on the same AWS instance class, so the comparison gives ClickHouse the more favorable statistic.

| Query | Shape | Cardinal | ClickHouse | Tantivy | Fastest |
|---|---|---:|---:|---:|---|
| Q1 | checkout + `failed AND order`, newest 100 | 35.392 ms | **25 ms** | 335.533 ms | ClickHouse |
| Q2 | compound Boolean + service/severity, newest 100 | **30.846 ms** | 49 ms | 571.448 ms | Cardinal |
| Q3 | five-token OR, newest 100 | **34.516 ms** | 206 ms | 494.240 ms | Cardinal |
| Q4 | `count(timeout)` | **0.737 ms** | 28 ms | 2.395 ms | Cardinal |
| Q5 | five-token OR count | **18.773 ms** | 105 ms | 60.242 ms | Cardinal |
| Q6 | three-token OR count by service | **85.893 ms** | 91 ms | 144.170 ms | Cardinal |
| Q7 | `connection AND reset` count by service | **31.794 ms** | 72 ms | 52.464 ms | Cardinal |
| Q8 | checkout + payment, hourly histogram | 32.600 ms | **30 ms** | 66.727 ms | ClickHouse |
| Q9 | connection + reset, hourly histogram | **7.420 ms** | 72 ms | 107.225 ms | Cardinal |

Cardinal wins **seven of nine against ClickHouse** and **all nine against Tantivy**. By geometric mean it is **3.15x faster than ClickHouse** and **5.05x faster than Tantivy**.

Those are numbers we can defend as the output of a real planner and production representations.

## There is not one Cardinal index

This is the part I still find most interesting.

I started this work thinking about the problem as:

```text
LKRN + inverted index
```

What we have now is closer to:

```text
                         +-- file-local token postings
                         |
                         +-- bitmap dimensions
                         |
LKRN objects ------------+-- generation postings
                         |
                         +-- dense dimension codes
                         |
                         +-- timestamp bounds
                         |
                         +-- time masks
```

The planner chooses among the representations that can prove the requested answer, based on estimated work. The benchmark does not choose for it.

For newest-100, generation is not eligible because we need actual rows. File-local postings combine the token predicate with metadata, search newest objects first, and stop when unopened time ranges cannot beat the current top 100. All three ordered queries opened 32 out of 2,008 bundles.

For a rare count, the answer can be a posting cardinality. Q4 found 151 matches using one dictionary lookup and 14 shard views. It opened zero LKRN bundles and read zero timestamps.

For a 100-million-row group-by, the point is not to decode `service_name` 100 million times. Q6 builds a predicate bitmap and intersects it with generation dimension bitmaps. The query emits ten group rows without opening the source objects.

For hourly histograms, verified bundle-to-hour ownership means the engine can intersect postings, dimensions, and time masks. Q8 and Q9 opened no bundles and read no timestamp values.

And if a specialized representation cannot answer exactly, the planner retains LKRN bundle evaluation as the correctness path.

That is what we mean by an adaptive index: not one clever encoding, and not a table mapping known queries to kernels, but a catalog of exact representations plus a planner that tries to do the minimum justified work.

## The first loss was useful

The first honest planner run made Q5 and Q6 dramatically slower than Tantivy. The planner was not confused: removing generation made them slower again. The defect was what happened *after* generation won.

That distinction pointed directly at the unnecessary global materialization. Once shard-local row sets remained shard-local, the final HEAD verification measured Q5 at 18.773 ms, Q6 at 85.893 ms, Q7 at 31.794 ms, and Q9 at 7.420 ms. Q4's rare count fell below one millisecond.

This is why a universal planner is more than cleanup. It separates a bad physical choice from a slow implementation of the right choice—and lets us fix the production primitive instead of adding another query-specific route.

## Query speed is only one-third of an index result

An inverted index can always buy query latency by spending more ingest CPU and storage. A useful benchmark has to publish all three.

### Build efficiency

We measured actual process CPU, not configured thread count:

```text
events per CPU-core-second
  = one billion / (user CPU seconds + system CPU seconds)
```

Only artifacts selected by the canonical planner are in the primary Cardinal total: `.tms` for Q1-Q3 and production generation for Q4-Q9.

| Index stack | Build wall time | CPU core-seconds | Events/CPU-core-second |
|---|---:|---:|---:|
| Cardinal selected Q1-Q9 indexes | 721.50 s | 2,897.93 | **345,074** |
| Tantivy | 5,274.43 s | 24,473.93 | **40,860** |

Cardinal used **8.45x fewer CPU-core-seconds per indexed event** and built **7.31x faster by sequential wall time**.

The Tantivy build really did take 87.9 minutes. Its average measured CPU occupancy was 4.64 logical cores, versus 4.02 for Cardinal's two sequential builders, so the difference is not that we simply threw more cores at the job.

ClickHouse does not publish consumed CPU for this exact 1B build. Its article says 50B records loaded in under four hours on 32 vCPUs, a lower bound of 108,507 events per provisioned-vCPU-second. That is useful context, but it is not the same metric and should not be smuggled into the actual-CPU table.

### Storage

Here is the complete bill:

| System/view | Data/source | Auxiliary index | Total |
|---|---:|---:|---:|
| Cardinal selected Q1-Q9 | 35.706 GB LKRN | 19.048 GB | **54.753 GB** |
| Cardinal if we also ship unused `.bmd` | 35.706 GB LKRN | 19.842 GB | **55.548 GB** |
| ClickHouse published table | included | included | **53.136 GB** |
| Tantivy production-equivalent | 35.706 GB LKRN | 45.871 GB | **81.577 GB** |

The `.tms` sidecars are 16.961 GB. Production generation is 2.086 GB. That is the selected 19.048 GB auxiliary stack.

We also built 0.795 GB of `.bmd`, but no canonical query selected or opened it. Charging unused bytes to the selected benchmark stack would be misleading; pretending they do not exist would also be misleading. So both totals are published.

The old 1.083 GB benchmark-only Q4-Q7 genindex is gone entirely.

Cardinal's selected auxiliary indexes are **58.48% smaller than Tantivy's index**. For equivalent original-log fetch semantics, Cardinal source-plus-index is **32.88% smaller**.

Why “production-equivalent”? The Tantivy `Body` field is indexed but not stored. Its standalone 45.871 GB index can answer this benchmark, but cannot return the original message. Keeping LKRN beside it makes the comparable serving total 81.577 GB. If original-message fetch is not part of the contract, the standalone Tantivy index is the smaller number and we show that too.

ClickHouse wins complete storage. Cardinal's selected serving stack is **3.04% larger**; including unused `.bmd` makes it **4.54% larger**. ClickHouse stores the complete published OTel schema while our frozen fingerprint contract covers the benchmark fields, so this is not a universal compression claim. It is the honest working-set comparison we have.

## What we learned

The most important result is not that Cardinal beats ClickHouse on seven queries and Tantivy on all nine.

It is that we can now explain every physical choice without saying “because this is Q6.”

- Ordered row queries choose file-local postings because generation cannot materialize the requested rows and scanning costs more.
- Counts and groups choose production generation because complete exact coverage lets bitmap work replace object opens and row visits.
- A tiny synthetic query chooses scan despite an available generation, because indexed setup costs more than the work avoided.
- Missing terms, dimensions, identities, or time ownership fail closed.
- Every execution reports whether it followed the selected plan.

The benchmark also stopped protecting us from our own slow paths. Its first honest result showed Q5 and Q6 losing; the path evidence localized the cost; the generic sharded-composition fix restored the wins without inventing another query-specific route.

Agents are going to ask far more questions of telemetry than humans ever did. Keeping only the subset we guessed would remain useful is not going to work.

So keep the original telemetry. Keep the indexes attached to it in object storage. Let exactness decide what is legal. Let cost decide what is cheap. And when the specialized representation cannot prove the answer, fall back to the data.

That is the adaptive query engine we are building at Cardinal.

---

## Methodology and reproducibility

- Implementation: `38e8c7ab2b9fbd94d7d47561f0b7b8eeaeab86d2`
- Draft PR: [cardinalhq/lakerunner#1334](https://github.com/cardinalhq/lakerunner/pull/1334)
- Cardinal/Tantivy raw canonical samples: `canonical.json`
- Planner ablations: `no-generation.json`, `no-generation-no-postings.json`
- Build/storage evidence: `build-storage.json`
- Published ClickHouse baseline: `clickhouse-published-baseline.json`
- Full technical report: `REPORT.md`

Cardinal and Tantivy ran on a dedicated AWS `m6i.8xlarge` with 32 vCPUs. The harness performed an untimed exact-correctness call, discarded one warmup per engine, then retained ten interleaved samples per query. Every answer passed parity. Every Cardinal sample recorded the planner candidates, chosen representation, and actual representation.

ClickHouse numbers are borrowed from the public TextBench artifact pinned at `ClickHouse/TextBench@a7bb4e024f4648e90a30b6f0d00ef1223fd7d7e5`. They ran on the same instance class in a separate sitting. The comparison uses the better of its two warm runs and does not claim same-sitting statistical equivalence.
