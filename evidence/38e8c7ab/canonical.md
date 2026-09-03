| Q  | Description                                                | Cardinal path              | Cardinal p50 (us) | Cardinal p95 (us) | Tantivy p50 (us) | Tantivy p95 (us) | C vs T | Parity     |
|----|------------------------------------------------------------|----------------------------|------------------:|------------------:|------------------:|------------------:|:------:|:-----------|
| Q1 | top-100 checkout AND failed AND order in W                 | file_local_postings        |             35392 |             36995 |            335533 |            347388 |   C    | PASS
| Q2 | top-100 frontend {(conn&reset)|timeout} sev>=13 in W       | file_local_postings        |             30846 |             31935 |            571448 |            583381 |   C    | PASS
| Q3 | top-100 5-token OR                                         | file_local_postings        |             34516 |             35483 |            494240 |            501333 |   C    | PASS
| Q4 | count(timeout)                                             | generation                 |               737 |               800 |              2395 |              2553 |   C    | PASS
| Q5 | count(5-token OR)                                          | generation                 |             18773 |             24793 |             60242 |             60660 |   C    | PASS
| Q6 | count(3-token OR) by service                               | generation                 |             85893 |             96508 |            144170 |            145731 |   C    | PASS
| Q7 | count(connection AND reset) by service                     | generation                 |             31794 |             32643 |             52464 |             55904 |   C    | PASS
| Q8 | hourly(checkout AND payment)                               | generation                 |             32600 |             34156 |             66727 |             67472 |   C    | PASS
| Q9 | hourly(connection AND reset)                               | generation                 |              7420 |              7737 |            107225 |            108168 |   C    | PASS
