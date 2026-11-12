# Benchmark results

These are measured workload snapshots, not framework rankings or performance
guarantees. Read the conditions and limitations before comparing results from
different backends, drivers, client runtimes or dates.

- [PostgreSQL read comparison, 2026-11-12](postgres/2026-11-12/README.md):
  che-orm versus equivalent SQL through Python psycopg2, with and without a
  datetime index; includes raw measurements, execution plans and live-backend
  test results. This is not an HTTP benchmark of che-rest.
