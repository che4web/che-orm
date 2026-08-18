use std::{path::PathBuf, sync::Arc, time::Duration};

use che_orm::{Database, Model};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use tokio::{runtime::Runtime, sync::Barrier, task::JoinSet};
use tokio_rusqlite::Connection as AsyncRusqliteConnection;

const SEED_ROWS: i64 = 10_000;
const RUSQLITE_POOL_SIZE: usize = 10;

#[derive(Debug, Clone, Model)]
#[model(table = "benchmark_users")]
struct BenchUser {
    #[field(primary_key)]
    id: i64,
    email: String,
    name: String,
    #[field(default = true)]
    is_active: bool,
}

#[derive(Clone)]
struct AsyncRusqlitePool {
    connections: Arc<Vec<AsyncRusqliteConnection>>,
}

impl AsyncRusqlitePool {
    async fn open(path: &str) -> Self {
        let mut connections = Vec::with_capacity(RUSQLITE_POOL_SIZE);
        for _ in 0..RUSQLITE_POOL_SIZE {
            let connection = AsyncRusqliteConnection::open(path).await.unwrap();
            connection
                .call(|connection| {
                    connection.execute_batch(
                        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 30000;",
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            connections.push(connection);
        }
        Self {
            connections: Arc::new(connections),
        }
    }

    fn connection(&self, task: usize) -> AsyncRusqliteConnection {
        self.connections[task % self.connections.len()].clone()
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Insert,
    Get,
    Filter,
    Update,
    Count,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Insert => "insert_one",
            Self::Get => "get_by_id",
            Self::Filter => "filtered_list",
            Self::Update => "update_one",
            Self::Count => "filtered_count",
        }
    }
}

struct Fixtures {
    runtime: Runtime,
    dir: PathBuf,
}

impl Fixtures {
    fn new() -> Self {
        let runtime = Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "che_orm_bench_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { runtime, dir }
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).display().to_string()
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn setup_sqlx(path: &str) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(RUSQLITE_POOL_SIZE as u32)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query(
                    "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 30000;",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect(path)
        .await
        .unwrap()
}

async fn setup_database(path: &str) -> Database {
    let database = Database::connect_with_max_connections(path, RUSQLITE_POOL_SIZE as u32)
        .await
        .unwrap();
    database.create_table::<BenchUser>().await.unwrap();
    database
}

async fn seed_sqlx(pool: &SqlitePool) {
    sqlx::query("CREATE TABLE IF NOT EXISTS benchmark_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL, name TEXT NOT NULL, is_active BOOLEAN NOT NULL DEFAULT true)")
        .execute(pool)
        .await
        .unwrap();
    let mut transaction = pool.begin().await.unwrap();
    for id in 1..=SEED_ROWS {
        sqlx::query("INSERT INTO benchmark_users (email, name, is_active) VALUES (?, ?, ?)")
            .bind(format!("user{id}@example.com"))
            .bind(format!("User {id}"))
            .bind(id % 2 == 0)
            .execute(&mut *transaction)
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
}

async fn seed_rusqlite(pool: &AsyncRusqlitePool) {
    let connection = pool.connection(0);
    connection
        .call(|connection| {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS benchmark_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL, name TEXT NOT NULL, is_active BOOLEAN NOT NULL DEFAULT true);")?;
            let transaction = connection.transaction()?;
            for id in 1..=SEED_ROWS {
                transaction.execute(
                    "INSERT INTO benchmark_users (email, name, is_active) VALUES (?1, ?2, ?3)",
                    rusqlite::params![format!("user{id}@example.com"), format!("User {id}"), id % 2 == 0],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .unwrap();
}

async fn seed_orm(database: &Database) {
    for id in 1..=SEED_ROWS {
        database
            .create::<BenchUser>()
            .set(BenchUserFields::EMAIL, format!("user{id}@example.com"))
            .set(BenchUserFields::NAME, format!("User {id}"))
            .set(BenchUserFields::IS_ACTIVE, id % 2 == 0)
            .execute()
            .await
            .unwrap();
    }
}

async fn orm_operation(database: &Database, operation: Operation, task: usize) {
    let id = SEED_ROWS + task as i64 + 1;
    match operation {
        Operation::Insert => {
            database
                .create::<BenchUser>()
                .set(BenchUserFields::EMAIL, format!("bench{id}@example.com"))
                .set(BenchUserFields::NAME, "Benchmark")
                .set(BenchUserFields::IS_ACTIVE, true)
                .execute()
                .await
                .unwrap();
        }
        Operation::Get => {
            std::hint::black_box(
                database
                    .get::<BenchUser>((task as i64 % SEED_ROWS) + 1)
                    .await
                    .unwrap(),
            );
        }
        Operation::Filter => {
            std::hint::black_box(
                database
                    .query::<BenchUser>()
                    .filter(BenchUserFields::IS_ACTIVE.eq(true))
                    .order_by(BenchUserFields::ID)
                    .limit(20)
                    .all()
                    .await
                    .unwrap(),
            );
        }
        Operation::Update => {
            std::hint::black_box(
                database
                    .update::<BenchUser>((task as i64 % SEED_ROWS) + 1)
                    .set(BenchUserFields::NAME, "Updated")
                    .execute()
                    .await
                    .unwrap(),
            );
        }
        Operation::Count => {
            std::hint::black_box(
                database
                    .query::<BenchUser>()
                    .filter(BenchUserFields::IS_ACTIVE.eq(true))
                    .count()
                    .await
                    .unwrap(),
            );
        }
    }
}

async fn sqlx_operation(pool: &SqlitePool, operation: Operation, task: usize) {
    let id = (task as i64 % SEED_ROWS) + 1;
    match operation {
        Operation::Insert => {
            sqlx::query("INSERT INTO benchmark_users (email, name, is_active) VALUES (?, ?, ?)")
                .bind(format!("bench{task}@example.com"))
                .bind("Benchmark")
                .bind(true)
                .execute(pool)
                .await
                .unwrap();
        }
        Operation::Get => {
            std::hint::black_box(
                sqlx::query("SELECT id, email, name, is_active FROM benchmark_users WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        Operation::Filter => {
            std::hint::black_box(sqlx::query("SELECT id, email, name, is_active FROM benchmark_users WHERE is_active = ? ORDER BY id LIMIT 20").bind(true).fetch_all(pool).await.unwrap());
        }
        Operation::Update => {
            std::hint::black_box(
                sqlx::query("UPDATE benchmark_users SET name = ? WHERE id = ?")
                    .bind("Updated")
                    .bind(id)
                    .execute(pool)
                    .await
                    .unwrap(),
            );
        }
        Operation::Count => {
            std::hint::black_box(
                sqlx::query("SELECT COUNT(*) FROM benchmark_users WHERE is_active = ?")
                    .bind(true)
                    .fetch_one(pool)
                    .await
                    .unwrap()
                    .get::<i64, _>(0),
            );
        }
    }
}

async fn rusqlite_operation(pool: &AsyncRusqlitePool, operation: Operation, task: usize) {
    let connection = pool.connection(task);
    let id = (task as i64 % SEED_ROWS) + 1;
    connection
        .call(move |connection| {
            match operation {
                Operation::Insert => {
                    connection.execute(
                        "INSERT INTO benchmark_users (email, name, is_active) VALUES (?1, ?2, ?3)",
                        rusqlite::params![format!("bench{task}@example.com"), "Benchmark", true],
                    )?;
                }
                Operation::Get => {
                    let _: (i64, String, String, bool) = connection.query_row(
                        "SELECT id, email, name, is_active FROM benchmark_users WHERE id = ?1",
                        [id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )?;
                }
                Operation::Filter => {
                    let mut statement = connection.prepare(
                        "SELECT id FROM benchmark_users WHERE is_active = ?1 ORDER BY id LIMIT 20",
                    )?;
                    let _: Vec<i64> = statement
                        .query_map([true], |row| row.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                }
                Operation::Update => {
                    connection.execute(
                        "UPDATE benchmark_users SET name = ?1 WHERE id = ?2",
                        rusqlite::params!["Updated", id],
                    )?;
                }
                Operation::Count => {
                    let _: i64 = connection.query_row(
                        "SELECT COUNT(*) FROM benchmark_users WHERE is_active = ?1",
                        [true],
                        |row| row.get(0),
                    )?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
}

async fn concurrent(
    tasks: usize,
    action: Arc<
        dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
            + Send
            + Sync,
    >,
) {
    let barrier = Arc::new(Barrier::new(tasks));
    let mut set = JoinSet::new();
    for task in 0..tasks {
        let barrier = Arc::clone(&barrier);
        let action = Arc::clone(&action);
        set.spawn(async move {
            barrier.wait().await;
            action(task).await;
        });
    }
    while set.join_next().await.is_some() {}
}

fn benchmark(c: &mut Criterion) {
    let fixtures = Fixtures::new();
    let runtime = &fixtures.runtime;
    for tasks in [1usize, 10, 100] {
        let mut group = c.benchmark_group(format!("sqlite/concurrent_{tasks}"));
        group.throughput(Throughput::Elements(tasks as u64));
        group.measurement_time(Duration::from_secs(5));
        for operation in [
            Operation::Insert,
            Operation::Get,
            Operation::Filter,
            Operation::Update,
            Operation::Count,
        ] {
            let orm_path = fixtures.path(&format!("orm_{tasks}_{}.sqlite", operation.name()));
            let sqlx_path = fixtures.path(&format!("sqlx_{tasks}_{}.sqlite", operation.name()));
            let rusqlite_path =
                fixtures.path(&format!("rusqlite_{tasks}_{}.sqlite", operation.name()));
            let orm_url = format!("sqlite://{orm_path}?mode=rwc");
            let sqlx_url = format!("sqlite://{sqlx_path}?mode=rwc");
            let orm = runtime.block_on(setup_database(&orm_url));
            runtime.block_on(seed_orm(&orm));
            let sqlx = runtime.block_on(setup_sqlx(&sqlx_url));
            runtime.block_on(seed_sqlx(&sqlx));
            let rusqlite = runtime.block_on(AsyncRusqlitePool::open(&rusqlite_path));
            runtime.block_on(seed_rusqlite(&rusqlite));
            let orm_action: Arc<
                dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            > = Arc::new({
                let orm = orm.clone();
                move |task| {
                    let orm = orm.clone();
                    Box::pin(async move { orm_operation(&orm, operation, task).await })
                }
            });
            let sqlx_action: Arc<
                dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            > = Arc::new({
                let sqlx = sqlx.clone();
                move |task| {
                    let sqlx = sqlx.clone();
                    Box::pin(async move { sqlx_operation(&sqlx, operation, task).await })
                }
            });
            let rusqlite_action: Arc<
                dyn Fn(usize) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            > = Arc::new({
                let rusqlite = rusqlite.clone();
                move |task| {
                    let rusqlite = rusqlite.clone();
                    Box::pin(async move { rusqlite_operation(&rusqlite, operation, task).await })
                }
            });
            group.bench_function(
                BenchmarkId::new(format!("orm/{}", operation.name()), tasks),
                |b| {
                    b.to_async(runtime)
                        .iter(|| async { concurrent(tasks, Arc::clone(&orm_action)).await })
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("sqlx/{}", operation.name()), tasks),
                |b| {
                    b.to_async(runtime)
                        .iter(|| async { concurrent(tasks, Arc::clone(&sqlx_action)).await })
                },
            );
            group.bench_function(
                BenchmarkId::new(format!("tokio-rusqlite/{}", operation.name()), tasks),
                |b| {
                    b.to_async(runtime)
                        .iter(|| async { concurrent(tasks, Arc::clone(&rusqlite_action)).await })
                },
            );
        }
        group.finish();
    }
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
