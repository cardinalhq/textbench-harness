# Contributing

Changes to the query contract, answer shapes, tokenizer, or timing boundary can
invalidate comparisons with existing evidence. Call those changes out explicitly
and publish results under a new evidence directory.

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Do not commit benchmark corpora, built indexes, credentials, or proprietary LKRN
binaries. Engine adapters must keep protocol messages on stdout and diagnostics
on stderr.
