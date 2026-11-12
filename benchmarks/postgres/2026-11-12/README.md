# PostgreSQL read benchmark: 2026-11-12

Comparison of the local che-orm working tree with equivalent SQL executed by
Python psycopg2 on PostgreSQL 18.6. The ORM package identifies itself as 0.3.3;
this was an unreleased working tree, not a measurement of the published package.

**This is not an HTTP benchmark of che-rest, FastAPI or Django.** At the time of
measurement, che-rest selected SQLite; PostgreSQL REST support was
not added for this experiment. The measured operation includes database access,
result materialization and JSON serialization, but no HTTP server or client.

## Saved artifacts

- [results.json](results.json): exact raw measurements from all 56 runs and
  server/client metadata, including the concurrency-1 controls.
- [plans-unindexed.json](plans-unindexed.json): `EXPLAIN (ANALYZE, BUFFERS, FORMAT
  JSON)` for the list, filtered list, retrieve and count before adding the index.
- [plans-indexed.json](plans-indexed.json): corresponding plans after indexing.
- [tests.json](tests.json): PostgreSQL test commands and aggregate outcomes.

The generated PostgreSQL cluster, database files, connection URIs and local
build artifacts are not archived. The temporary server was stopped after the
measurement. This snapshot does not introduce a supported benchmark runner or
change application configuration.

## Conditions

| Parameter | Value |
| --- | --- |
| Server | PostgreSQL 18.6, Linux x86-64 |
| Fixture | 10,000 tasks, one author, nullable assignee, three status values |
| Datetime storage | Native TIMESTAMPTZ; seeded timestamps have second precision |
| Connections | Separate runs with pools of 4 and 8 for both implementations |
| Active clients | 50; additional sorted-list controls used concurrency 1 |
| Measured operations | 2,000 per run |
| Warmup | 200 operations per run |
| Repetitions | 2, with reversed configuration order in the second repetition |
| Rust | Release, rustc 1.92.0, Tokio multithread runtime |
| Python | CPython 3.12.12, psycopg2 2.9.11, libpq 17.6 |
| Client processes | One per implementation |
| Transport | Local TCP, TLS disabled |
| PostgreSQL configuration | shared_buffers 128 MB, work_mem 4 MB, timezone UTC |
| Durability setting | fsync off for the disposable cluster; timed operations were read-only |

Python used a threadpool and a semaphore bounding simultaneous connections to
the selected pool size. Rust used async tasks and its bounded database pool.
Python's GIL still applies, and CPU execution/scheduling were not normalized
between the runtimes. The psycopg2 driver was available locally; psycopg 3 could
not be downloaded because DNS resolution was unavailable.

The Rust benchmark reused workspace dependency lock versions:
deadpool-postgres 0.14.2, tokio-postgres 0.7.18 and postgres-types 0.2.14. See the
raw metadata for compiler and server build details.

### Operations

- **Sorted list:** count all matching tasks, then select 50 tasks ordered by
  `created_at DESC`, joining their author, and serialize the response to JSON.
- **Filtered list:** count and select with `name LIKE '%task%'`, order by task
  ID, limit 50, and serialize JSON. All seeded task names match this predicate.
- **Retrieve:** select task ID 500 with its author and serialize JSON.

Both implementations selected the full task and author projection, including
author fields omitted from the response. Both list counts retained the author
JOIN, avoiding the projection/count asymmetry of the earlier SQLite HTTP test.
Counts and pages used separate pool checkouts. JSON samples were compared across
implementations, index configurations, pool sizes and repetitions.

The comparison remains **ORM versus SQL driver**, not identical client code:
statement preparation, session setup, pool recycling, driver protocols and model
decoding were not normalized away. There were no request-time authorization or
sync-runtime checks in this fixture.

One list operation executes COUNT plus page selection; retrieve executes one
selection. Therefore operations/second below must not be read as SQL
statements/second or HTTP requests/second.

## Results at concurrency 50

Arithmetic mean throughput across two runs, rounded to whole operations/second.
All raw observations are preserved rather than replacing them with the averages.

### Without an index on created_at

| Operation | che-orm, pool 4 | Python, pool 4 | che-orm, pool 8 | Python, pool 8 |
| --- | ---: | ---: | ---: | ---: |
| Sorted list | 793 | 894 | 1,728 | 1,554 |
| Filtered list | 1,788 | 1,932 | 3,744 | 2,540 |
| Retrieve | 13,847 | 17,820 | 19,005 | 15,891 |

### With an index on created_at

| Operation | che-orm, pool 4 | Python, pool 4 | che-orm, pool 8 | Python, pool 8 |
| --- | ---: | ---: | ---: | ---: |
| Sorted list | 2,299 | 2,131 | 4,173 | 2,394 |
| Filtered list | 1,596 | 1,631 | 3,033 | 2,314 |
| Retrieve | 12,034 | 15,345 | 19,765 | 14,215 |

The indexed series used the same fixture, with an ordinary B-tree index added:

```sql
CREATE INDEX bench_tasks_created_at_idx ON bench_tasks(created_at);
ANALYZE bench_tasks;
```

The fixture already had its primary-key and author/assignee indexes. No
implementation-specific indexes or data changes were introduced between clients.
This disposable setup is not a production migration: application indexes must
be declared through compiled migration history.

### Concurrency-1 sorted-list controls

Pool capacity was 4, but only one operation was active at a time:

| Index | che-orm | Python |
| --- | ---: | ---: |
| Absent | 321 | 303 |
| Present | 967 | 1,077 |

These controls must not be mixed with the concurrency-50 throughput tables.

## Query plans and interpretation

Without the datetime index, the inspected list plan scanned the tables and used
an in-memory `top-N heapsort`. With the index it used a backward scan of
`bench_tasks_created_at_idx` and no separate sort. The complete plans include
buffers and instrumented execution timings; those timings are not end-to-end
client latency measurements.

Within this PostgreSQL workload:

- Raising the Rust pool from 4 to 8 improved sorted-list throughput from about
  793 to 1,728 operations/s without the index and 2,299 to 4,173 with it. The
  negative scaling observed in the earlier bundled-SQLite experiment was not
  reproduced here. This is not a guarantee that larger PostgreSQL pools always
  improve throughput.
- At pool size 8, the datetime index improved the Rust sorted list about 2.4x.
  It does not serve the contains filter or primary-key retrieval. Changes in
  those scenarios between series should not be attributed to the date index.
- At pool size 4, Python was faster in several scenarios; at pool size 8,
  che-orm had higher throughput in all three scenarios in this sample. The
  different abstraction levels and runtime CPU behavior prevent a general
  Rust-versus-Python ranking.
- Two repetitions are a small local sample, and some cases had substantial
  variation. Do not combine the best observations from different runs into a
  single claimed configuration.

There were **112,000 measured operations**, plus warmups, and zero failed
operations. JSON sample parity passed in every configuration. Raw mean/p95
latencies are local per-operation harness timings, not HTTP response times.
Client scheduling and semaphore/pool queueing differ, so percentile comparisons
between implementations require caution.

## Live PostgreSQL tests

The same temporary PostgreSQL server hosted a separate test database. These
commands ran with `TEST_POSTGRES_URL` set, SQLite defaults disabled, and ignored
runtime tests explicitly included:

```bash
cargo test -p che-orm --no-default-features --features postgres -- --include-ignored
cargo test -p che-sync --no-default-features --features postgres -- --include-ignored
```

| Package | Passed | Failed | Ignored |
| --- | ---: | ---: | ---: |
| che-orm | 61 | 0 | 0 |
| che-sync | 20 | 0 | 0 |

These totals aggregate the package's reported test-binary and doctest summaries.
SQLite-only tests excluded by feature gates are not claimed as PostgreSQL tests.
No existing application database was used or modified.
