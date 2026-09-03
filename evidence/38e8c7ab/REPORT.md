# Adaptive Cardinal vs ClickHouse vs Tantivy on one billion logs

## Verdict

The old benchmark result was not a valid demonstration of adaptive execution. Q4-Q7 were routed by query identity to a benchmark-only index. That path is gone.

At `38e8c7ab2b9fbd94d7d47561f0b7b8eeaeab86d2`, every Cardinal query is lowered to a logical predicate and result contract, then planned from concrete artifact capabilities and estimated work. The planner never receives a TextBench query ID. Planned and actual representations matched on every correctness and timed call.

The honest result is:

- Cardinal is faster than Tantivy on **all 9 queries** and **5.05x faster by geometric mean**.
- Cardinal is faster than the separately published ClickHouse baseline on **7 of 9** queries and **3.15x faster by geometric mean**.
- The selected Cardinal Q1-Q9 indexes process **345,074 events per consumed CPU-core-second**, versus Tantivy's **40,860**: an **8.45x CPU-efficiency advantage**.
- Cardinal's selected auxiliary indexes occupy **19.048 GB**. Together with **35.706 GB** of LKRN data, the serving footprint is **54.753 GB**.
- That footprint is **32.88% smaller** than production-equivalent LKRN-plus-Tantivy, but **3.04% larger** than ClickHouse's published self-contained table.

The planner selected the fastest exact Cardinal representation in every measured ablation. The first planner run exposed a generic production-generation defect: shard-local predicate results were flattened into a generation-wide bitmap immediately before count/group. The final implementation preserves those ordered shard-local parts through token/dimension intersection, bucket ownership, cardinality, and grouping. That restores the wins without restoring query-ID routing.

## What “adaptive” means now

The planner has three universal candidates:

| Representation | Exactness requirement | Cost inputs | Execution consequence |
|---|---|---|---|
| File-local postings | Exact-token predicate and `.tms` coverage for every selected bundle | bundle/sidecar opens, dictionary lookups, bitmap operations, estimated row materialization | Open only selected LKRN objects and their postings; materialize rows or aggregate after bitmap filtering. |
| Production generation | Exact count/group shape, represented tokens and dimensions, complete bundle identity coverage, and each bundle wholly owned by a query window/output bucket | generation artifact opens, dictionary lookups, shard bitmap operations, emitted buckets | Answer from generation postings, dimensions, and time masks without opening LKRN bundles. |
| Bundle scan | Selected LKRN bundles exist | bundle opens and predicate-scaled row visits | Exact correctness fallback for unsupported or uneconomic indexed shapes. |

Exactness is checked before cost. A missing token, unsupported predicate, uncovered dimension, partial generation, or bucket-crossing bundle makes generation ineligible; it cannot win by being cheap. Among eligible candidates, the planner compares one decomposed integer cost model:

```text
fixed
  + 4,096 * object opens
  +   128 * dictionary lookups
  +    64 * bitmap operations
  +         estimated row visits
  +    16 * emitted rows or buckets
```

Those units are an auditable ranking, not fake microseconds. Every PathTrace preserves the candidate eligibility, component estimates, selected cost, and actual path so future measurements can recalibrate the weights.

The same contract exists in the Rust query engine and QueryAPI generation routing. A cross-language conformance test pins the frozen 2,008-bundle cost vector. A rename mutation changes a Q4 benchmark label to Q9 without changing a byte of the plan. A separate tiny-corpus test selects bundle scan even when generation is eligible, proving this is not a fixed representation priority.

## Canonical query result

Times are warm engine milliseconds. Cardinal and Tantivy ran interleaved in one process after an untimed correctness call and one discarded warmup; the table reports the nearest-rank p50 and p95 from ten retained samples. ClickHouse is the best of two published warm runs on the same instance class, which is a more favorable statistic for ClickHouse.

| Query | Planner choice | Cardinal p50 | Cardinal p95 | Tantivy p50 | Tantivy p95 | ClickHouse hot best | Fastest |
|---|---|---:|---:|---:|---:|---:|---|
| Q1 | file-local postings | 35.392 | 36.995 | 335.533 | 347.388 | **25** | ClickHouse |
| Q2 | file-local postings | **30.846** | 31.935 | 571.448 | 583.381 | 49 | Cardinal |
| Q3 | file-local postings | **34.516** | 35.483 | 494.240 | 501.333 | 206 | Cardinal |
| Q4 | generation | **0.737** | 0.800 | 2.395 | 2.553 | 28 | Cardinal |
| Q5 | generation | **18.773** | 24.793 | 60.242 | 60.660 | 105 | Cardinal |
| Q6 | generation | **85.893** | 96.508 | 144.170 | 145.731 | 91 | Cardinal |
| Q7 | generation | **31.794** | 32.643 | 52.464 | 55.904 | 72 | Cardinal |
| Q8 | generation | 32.600 | 34.156 | 66.727 | 67.472 | **30** | ClickHouse |
| Q9 | generation | **7.420** | 7.737 | 107.225 | 108.168 | 72 | Cardinal |

All nine correctness calls and all timed answers passed exact parity. Q1-Q3 compared complete ordered row identities `[timestamp, bundle, partition, row]`, not timestamps alone. Q4-Q9 compared complete scalar/group/histogram answers.

ClickHouse ran in a separate published sitting and its public Parquet substrate is not byte-identical to the frozen LKRN conversion. This is a provenance-pinned external reference, not a controlled same-sitting three-engine result. Cardinal and Tantivy are the controlled same-process comparison.

## Proof that the planner selected the right representation

The canonical run offered all three representations. Two same-box ablations removed capabilities without changing query semantics:

1. Remove the production generation catalog. Q4-Q9 must replan to file-local postings.
2. Remove generation and disable file-local postings. Q1-Q3 must replan to bundle scan.

| Query | Canonical choice | Canonical p50 | Next exact representation | Ablation p50 | Selected-path advantage |
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

Every ablation retained exact Cardinal/Tantivy parity and every planned path equaled the observed path. The raw evidence is in `canonical.json`, `no-generation.json`, and `no-generation-no-postings.json`.

## Why each query takes its selected path

| Query | Semantic/physical differentiator | Measured evidence |
|---|---|---|
| Q1 | Projected newest-100 rows make generation ineligible. The file-local plan composes service and two token predicates before materialization, then stops after the first newest-first wave. | 32/2,008 bundles opened, 64 partitions, 22 sidecars opened, 7.27 MB postings touched, 100 rows emitted. |
| Q2 | The compound DNF, service, and severity predicates remain exact in the row-returning evaluator. File-local postings reduce body work before the ordered heap. | 32 bundles, 2.66 MB postings, 95,878 candidates, 100 emitted; 2.98x over scan. |
| Q3 | A dense five-token OR still benefits from top-K time pruning: postings identify 674,547 candidates in the visited partitions, but only 100 rows are decoded and later bundles cannot beat the heap bound. | 32 bundles, 7.99 MB postings, one probe round; 5.65x over scan. |
| Q4 | A rare exact-token scalar count is a generation dictionary lookup plus posting cardinalities. Whole-generation ownership feeds the borrowed shard views straight to cardinality. | 151 matches, 14 shard views, 12 us filter and effectively zero emit time; 797.78x over file-local. |
| Q5 | Five generation postings are unioned shard-locally and counted without visiting 108.7M rows or flattening the 60 results into one global bitmap. | 5 dictionary lookups, 254 shard views, 11.7 ms filter and 0.2 ms emit; 41.15x over file-local and 3.21x faster than Tantivy. |
| Q6 | The sharded predicate union is grouped directly against generation dimension bitmaps rather than decoding `service_name` or first manufacturing one 1B-row bitmap. | 3 dictionary lookups, 134 shard views, 10 output rows, zero bundle opens; 9.99x over file-local and 1.68x faster than Tantivy. |
| Q7 | Two token postings intersect within each shard before dimension grouping consumes the same ordered parts. | 2 lookups, 118 shard views, 2 output rows, zero bundle opens; 25.81x over file-local and 1.65x faster than Tantivy. |
| Q8 | Exact token and checkout dimension RowSets merge-join shard ranges, then verified whole-generation hour ownership counts the result directly. | 60 token shard views, one output bucket, zero bundles/timestamps; 21.01x over file-local and 2.05x faster than Tantivy. |
| Q9 | Two generation postings intersect shard-locally; whole-generation hour ownership avoids building and re-intersecting an all-row mask. | 118 shard views, 59 tasks/14 workers, 0.133 ms emit, zero bundles/timestamps; 85.32x over file-local and 14.45x faster than Tantivy. |

This is adaptive execution in the meaningful sense: the same semantic query can move when the catalog or topology changes, unsupported specializations fail closed, and query identity cannot influence the decision.

## Index-build throughput and CPU efficiency

The requested normalization is actual process CPU consumption:

```text
events indexed per CPU-core-second
  = 1,000,000,000 / (user CPU seconds + system CPU seconds)
```

Only indexes selected in the canonical Q1-Q9 PathTraces are charged to the primary Cardinal stack: `.tms` for Q1-Q3 and production generation for Q4-Q9. The old 1.083 GB benchmark-only genindex is removed. `.bmd` was built but no canonical query opened it, so it is reported separately rather than hidden in or silently omitted from the accounting.

| Builder/output | Wall | CPU core-s | Avg. cores | Events/wall-s | Events/CPU-core-s |
|---|---:|---:|---:|---:|---:|
| `.tms` file-local postings | 53.93 s | 1,689.23 | 31.32 | 18,542,555 | 591,986 |
| Production generation | 667.57 s | 1,208.70 | 1.81 | 1,497,970 | 827,337 |
| **Cardinal selected Q1-Q9 stack** | **721.50 s** | **2,897.93** | **4.02** | **1,386,001** | **345,074** |
| **Tantivy index** | **5,274.43 s** | **24,473.93** | **4.64** | **189,594** | **40,860** |

Cardinal's selected index stack is **7.31x faster by sequential wall time** and **8.45x more efficient per consumed CPU-core-second**. Tantivy took 87.9 minutes and consumed 24,473.93 CPU-core-seconds; the long build was not explained by simply using fewer cores.

If `.bmd` is shipped despite not being selected here, Cardinal totals 727.12 wall seconds, 3,063.19 CPU-core-seconds, and 326,457 events/CPU-core-second—still **7.99x Tantivy's CPU efficiency**.

ClickHouse does not publish consumed CPU for its exact 1B build. Its article says a 50B load completed in under four hours on 32 vCPUs, establishing a lower bound of **108,507 events per provisioned-vCPU-second**. That is not interchangeable with consumed CPU-core-seconds and therefore is not inserted into the table above.

## Storage: data and index, separately and together

Decimal GB is used below; GiB is included for unambiguous capacity planning.

| Cardinal component | Bytes | GB | GiB | Selected by canonical run? |
|---|---:|---:|---:|---|
| LKRN source data | 35,705,951,640 | 35.706 | 33.254 | source of truth |
| `.tms` | 16,961,132,765 | 16.961 | 15.796 | Q1-Q3 |
| Production generation | 2,086,412,498 | 2.086 | 1.943 | Q4-Q9 |
| **Selected auxiliary indexes** | **19,047,545,263** | **19.048** | **17.739** | yes |
| **LKRN + selected indexes** | **54,753,496,903** | **54.753** | **50.993** | complete selected stack |
| `.bmd`, built but unused | 794,921,048 | 0.795 | 0.740 | no |
| **LKRN + selected indexes + `.bmd`** | **55,548,417,951** | **55.548** | **51.733** | conservative ship-all view |

So the direct answer to “is this 20 GB plus 35 GB?” is: **yes, approximately**. The measured selected stack is 19.048 GB of index plus 35.706 GB of data. `.tms + .bmd` alone is 17.756 GB decimal; adding production generation brings a ship-all auxiliary stack to 19.842 GB.

Tantivy's measured index is 45,871,043,173 bytes (45.871 GB, 42.721 GiB). Its `Body` field is indexed but not stored, so the index cannot return original log messages by itself. Two views are therefore necessary:

| Serving view | Index bytes | Source/data bytes | Complete bytes | Complete GiB |
|---|---:|---:|---:|---:|
| Cardinal selected Q1-Q9 | 19,047,545,263 | 35,705,951,640 | **54,753,496,903** | **50.993** |
| Cardinal including unused `.bmd` | 19,842,466,311 | 35,705,951,640 | **55,548,417,951** | **51.733** |
| Tantivy benchmark-only, no original-message fetch | **45,871,043,173** | 0 | **45,871,043,173** | **42.721** |
| Tantivy production-equivalent fetch | 45,871,043,173 | 35,705,951,640 | **81,576,994,813** | **75.974** |
| ClickHouse published self-contained table | included | included | **53,135,582,221** | **49.486** |

Cardinal's selected auxiliary index is **58.48% smaller than Tantivy's index**. With source retained for equivalent fetch semantics, Cardinal is **32.88% smaller end to end**. The standalone Tantivy index is smaller only under the narrower contract where returning original messages is unnecessary.

ClickHouse has the smallest complete published footprint. Cardinal's selected stack is **3.04% larger**; the conservative stack including unused `.bmd` is **4.54% larger**. ClickHouse retains the full published OTel schema, while the frozen LKRN/Tantivy identity contract covers timestamp, message, service, severity, and row identity, so this is a benchmark serving-footprint comparison rather than a universal format-compression claim.

## Provenance

- Planner/query SHA: `38e8c7ab2b9fbd94d7d47561f0b7b8eeaeab86d2`
- Release binary SHA256: `a5cd5682c2f5df3f95f167fba8248818113bf4d569d163baecac16b572332d57`
- Instance: AWS `m6i.8xlarge`, 16 physical cores/32 vCPUs, 128 GiB class, `us-east-2a`
- Canonical window: `2026-08-25T16:43:42.669Z` to `2026-08-25T16:46:16.633Z`
- Corpus: 2,008 LKRN bundles, 1,000,000,000 rows, fingerprint JSON SHA256 `181a18a2c20afd79f15cff7f53aa2313362f0f672bb99d7642e5c295091d19a6`
- Tantivy: 1,000,000,000 documents, 27 segments, `meta.json` SHA256 `30b9a80ea0b18653e03080727da0326de414b04c1c9b98ef5fed7b8ac8e2729b`
- Production generation BuildId: `92b34388b40c161f0c7d89787de7951b`
- Canonical JSON SHA256: `c425185d407e336bf7d5350cbf035fc873b6b192a5e82799d38729bcff57a280`
- ClickHouse baseline: `ClickHouse/TextBench@a7bb4e024f4648e90a30b6f0d00ef1223fd7d7e5`, result SHA256 `9e795f8307e1fbc732e4f3a8009f989e9d5bc89aaee5589066fae0166cd11a2e`

All three Cardinal/Tantivy runs embed the correct `38e8c7ab` SHA, report `status=Ok`, `parity=Pass`, `external_slicing=false`, `benchmark_pool=false`, and planned path equal to actual path for every query.

## Validation status

Passed before the benchmark:

- Rust planner tests, including cost crossover, missing-term fallback, row-output eligibility, and bucket boundaries.
- TextBench all-target suite and query-ID rename mutation.
- Production generation differential suite: 31/31.
- `queryapi`, `queryworker`, and `pkg/lkrnworker` Go suites.
- Full `make check`: 1,602 Rust tests, formatting, migrations, lint, and 69 Go packages passed. The remaining `license-eye` step has a pre-existing linked-worktree panic and six pre-existing headerless result text files on the base branch; no new source file is implicated.

The repository perfbench command was not run because local `kubectl` has no current context and the documented production target requires explicit confirmation. The frozen same-box TextBench benchmark and its representation ablations are the performance evidence for this change.

## Remaining engineering work

The planner problem and executor problem are now separated cleanly:

- Q6's p95 is 96.508 ms despite its 85.893 ms p50, so grouped-generation scheduling and teardown still have tail work to attribute.
- Q8 is 2.05x faster than Tantivy but remains 8.7% slower than ClickHouse's published best warm result; its 29.5 ms filter phase is the next dense token/dimension-intersection target.
- Q1-Q3 still pay per-call bundle/sidecar open and mmap work. A persistent production lifecycle could reduce this, but a benchmark-only prepared handle would not prove production improvement.
- The integer cost weights rank this corpus correctly, but should be calibrated from production telemetry across more topologies. Exactness gates must remain invariant while costs evolve.
- Production raw-log fallback still executes through the ordinary worker path; carrying the full selected-plan evidence through the API/worker boundary is the next observability integration.

The benchmark now measures a real adaptive planner, the selected physical paths are reproducible, all nine beat Tantivy, and the remaining losses point at actual production kernels instead of harness routing.
