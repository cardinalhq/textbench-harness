# `textbench`

This crate owns the frozen Q1-Q9 semantics, NDJSON engine protocol, exact
parity gate, physical-path gate, interleaved timing, and JSON/Markdown reports.

It has no dependency on Cardinal/LKRN or Tantivy. Both are long-lived child
processes implementing `docs/engine-protocol.md`.

Use `cargo test -p textbench` for the query-contract, comparator, timing, and
fallback tests. See the repository root `README.md` for runnable commands.
