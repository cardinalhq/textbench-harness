# Engine protocol

The benchmark runner communicates with each engine over newline-delimited JSON
on standard input and output. Standard output must contain protocol messages
only; diagnostics belong on standard error.

The process is long-lived. Index opening, memory mapping, worker-pool creation,
and other startup work must finish before the adapter answers `hello`.

## Handshake

Harness request:

```json
{"op":"hello","protocol_version":1}
```

Engine response:

```json
{
  "op": "hello",
  "protocol_version": 1,
  "metadata": {
    "engine": "Cardinal",
    "version": "1.2.3",
    "build_id": "sha256:...",
    "corpus_id": "sha256:..."
  }
}
```

`engine` is `Cardinal` or `Tantivy`. `build_id` identifies the executable and
its index artifacts. `corpus_id` identifies the logical rows and their stable
source order. The harness refuses to compare two different corpus IDs.

## Query request

```json
{
  "op": "run",
  "request_id": 7,
  "query": {
    "id": "Q4",
    "description": "count(timeout)",
    "predicate": {"ExactToken":"timeout"},
    "group_by": {"columns":[],"bucket_ms":null},
    "limit": null,
    "reverse_chrono": false,
    "shape": "Scalar"
  }
}
```

The `id` is a reporting label. Cardinal's planner must choose from the logical
predicate, result shape, and available artifact topology—not from the label.

## Result response

```json
{
  "op": "result",
  "request_id": 7,
  "answer": {
    "matched_rows": 151,
    "answer": [["count",151]],
    "path_trace": {
      "engine": "Cardinal",
      "path": "generation",
      "planned_path": "generation",
      "workers": 32,
      "external_slicing": false,
      "benchmark_pool": false,
      "counters": {"engine_micros":737}
    }
  }
}
```

For Cardinal, `planned_path` is required in practice: the harness compares it
with `path` and rejects a mismatch before timing. For Tantivy, the harness
checks the fixed path appropriate to the query shape.

Answers are exact JSON values:

- Q1-Q3: `[["nrows",N],["top_ts",TS],["top_rows",[[TS,file,group,row],...]]]`
- Q4-Q5: `[["count",N]]`
- Q6-Q7: `[[service,count],...]`, sorted by count descending then key ascending
- Q8-Q9: `[[bucket_ms,count],...]`, sorted by bucket ascending

The Q1-Q3 identity is the original source row ordinal encoded as
`file=0, group=0, row=ordinal` for the canonical 1B file. Both ingestion paths
must preserve it. This closes timestamp-tie ambiguity without requiring
Tantivy to store the complete message body.

The harness measures the full request/response round trip symmetrically.
`engine_micros` is useful attribution but is not the primary comparison.

## Error and shutdown

```json
{"op":"error","request_id":7,"message":"explanation"}
{"op":"shutdown"}
{"op":"bye"}
```

Any error, malformed response, request-ID mismatch, engine-identity mismatch,
path mismatch, or parity mismatch makes the affected query non-publishable and
the harness exits nonzero.
