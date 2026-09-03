# Frozen `38e8c7ab` evidence

This directory is the complete publication bundle for the final adaptive
Cardinal-vs-Tantivy run performed on 2026-08-25.

Start with:

- `BLOG.md` — narrative draft;
- `REPORT.md` — technical result and methodology;
- `canonical.md` — compact Q1-Q9 table;
- `canonical.json` — full samples, answers, planner choices, executed paths,
  and counters;
- `build-storage.json` — build CPU/wall/RSS and storage accounting;
- `no-generation.json` and `no-generation-no-postings.json` — planner
  ablations;
- `host-and-binary.txt` — frozen commit, harness binary digest, and host;
- `source-fingerprints.json` and `tantivy-source-fingerprints.json` — source
  parity gate.

The logs retain original absolute paths because changing them would invalidate
the accompanying hashes. They do not imply that the corpus is private: the
logical source is the public ClickHouse/TextBench `part_000.parquet` dataset.

The original harness called both engines in-process. The extracted OSS harness
uses a symmetric long-lived process protocol because LKRN source is not part of
this repository. Do not combine samples from the two harness boundaries in one
statistical summary.
