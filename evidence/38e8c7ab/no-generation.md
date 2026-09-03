| Q  | Description                                                | Cardinal path              | Cardinal p50 (us) | Cardinal p95 (us) | Tantivy p50 (us) | Tantivy p95 (us) | C vs T | Parity     |
|----|------------------------------------------------------------|----------------------------|------------------:|------------------:|------------------:|------------------:|:------:|:-----------|
| Q4 | count(timeout)                                             | file_local_postings        |            587963 |            599210 |              3078 |              3208 |   T    | PASS
| Q5 | count(5-token OR)                                          | file_local_postings        |            772468 |            777687 |             60407 |             60614 |   T    | PASS
| Q6 | count(3-token OR) by service                               | file_local_postings        |            858313 |            863024 |            140757 |            143270 |   T    | PASS
| Q7 | count(connection AND reset) by service                     | file_local_postings        |            820635 |            829515 |             52482 |             56089 |   T    | PASS
| Q8 | hourly(checkout AND payment)                               | file_local_postings        |            684925 |            688339 |             67094 |             67225 |   T    | PASS
| Q9 | hourly(connection AND reset)                               | file_local_postings        |            633099 |            639530 |            104146 |            108438 |   T    | PASS
