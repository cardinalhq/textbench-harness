| Q  | Description                                                | Cardinal path              | Cardinal p50 (us) | Cardinal p95 (us) | Tantivy p50 (us) | Tantivy p95 (us) | C vs T | Parity     |
|----|------------------------------------------------------------|----------------------------|------------------:|------------------:|------------------:|------------------:|:------:|:-----------|
| Q1 | top-100 checkout AND failed AND order in W                 | bundle_scan                |             55804 |             56658 |            338689 |            339417 |   C    | PASS
| Q2 | top-100 frontend {(conn&reset)|timeout} sev>=13 in W       | bundle_scan                |             92003 |             94793 |            560729 |            566921 |   C    | PASS
| Q3 | top-100 5-token OR                                         | bundle_scan                |            194892 |            195645 |            491818 |            497740 |   C    | PASS
