# che-orm benchmarks

This crate compares SQLite CRUD paths for:

- `che-orm`;
- direct `sqlx`;
- `tokio-rusqlite` with a fixed pool of 10 connections.

Run the full benchmark from the workspace root:

```bash
cargo bench -p che-orm-benchmarks
```

The benchmark uses a file-backed SQLite database, WAL, a 30-second busy
timeout, and the same schema/data for every implementation. Setup and seeding
are outside measured iterations. Groups cover one task and 10/100 concurrent
tasks. The async-rusqlite pool remains fixed at 10 workers in every group; the
100-task result therefore measures queueing as well as database contention.

Operations are measured separately: insert one row, get by primary key,
filtered list, update one row, and filtered count. Results are machine-specific
and should be compared on the same OS, SQLite configuration, and hardware.

Save and compare Criterion baselines:

```bash
cargo bench -p che-orm-benchmarks -- --save-baseline main
cargo bench -p che-orm-benchmarks -- --baseline main
```
