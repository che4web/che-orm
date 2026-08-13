use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;
use sqlx::{SqliteConnection, SqlitePool, migrate::Migrator, sqlite::SqlitePoolOptions};

use crate::{
    Error, MigrationStatus, Model, Result, Signals, create_table_sql,
    migration::SQLITE_FK_REBUILD_DIRECTIVE,
};

static MIGRATION_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
    signals: Signals,
}

impl SqliteBackend {
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with_max_connections(url, 10).await
    }

    pub async fn connect_with_max_connections(url: &str, max_connections: u32) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections.max(1))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("PRAGMA busy_timeout = 30000")
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("PRAGMA journal_mode = WAL")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        Ok(Self {
            pool,
            signals: Signals::new(),
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn signals(&self) -> &Signals {
        &self.signals
    }

    pub async fn create_table<M: Model>(&self) -> Result<()> {
        self.apply_sql(&create_table_sql::<M>()).await
    }

    pub async fn apply_sql(&self, sql: &str) -> Result<()> {
        if requires_fk_safe_rebuild(sql) {
            self.apply_fk_safe_sql(sql).await?;
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        run_preflight(sql, &mut *tx).await?;
        for statement in executable_statements(sql) {
            sqlx::query(&statement).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn apply_migrations_dir(
        &self,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<String>> {
        // SQLx's SQLite migration table bootstrap is not process-local safe.
        // Serialize application instances in this process before invoking it.
        let _lock = MIGRATION_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let applied_before: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        migrator.run(&self.pool).await?;
        Ok(migrator
            .iter()
            .filter(|migration| !applied_before.contains(&migration.version))
            .map(|migration| migration.description.to_string())
            .collect())
    }

    pub async fn apply_migrations_dir_with_namespace(
        &self,
        namespace: &str,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<String>> {
        // SQLx identifies migrations only by their numeric version. Namespace versions so
        // independent applications can each begin their migrations at 0001.
        let _lock = MIGRATION_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let namespace_hash = namespace_hash(namespace);
        self.register_namespace(namespace, namespace_hash).await?;
        let mut migrator = Migrator::new(migrations_dir.as_ref()).await?;
        migrator.set_ignore_missing(true);
        let original_versions = migrator
            .iter()
            .map(|migration| {
                Ok((
                    namespaced_migration_version(namespace, migration.version)?,
                    migration.description.to_string(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        for migration in migrator.migrations.to_mut() {
            migration.version = namespaced_migration_version(namespace, migration.version)?;
        }
        self.validate_namespace_versions(&migrator, namespace_hash)
            .await?;
        let applied_before: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        migrator.run(&self.pool).await?;
        Ok(original_versions
            .into_iter()
            .filter(|(version, _)| !applied_before.contains(version))
            .map(|(_, description)| description)
            .collect())
    }

    pub async fn migration_status_with_namespace(
        &self,
        namespace: &str,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<MigrationStatus>> {
        let _lock = MIGRATION_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let namespace_hash = namespace_hash(namespace);
        self.register_namespace(namespace, namespace_hash).await?;
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let applied: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        migrator
            .iter()
            .map(|migration| {
                let version =
                    namespaced_migration_version_from_hash(namespace_hash, migration.version)?;
                let stored = applied.iter().find(|row| row.0 == version);
                Ok(MigrationStatus {
                    name: migration.description.to_string(),
                    applied: stored.is_some_and(|row| row.1),
                    checksum: stored
                        .map(|row| checksum_hex(&row.2))
                        .or_else(|| Some(checksum_hex(migration.checksum.as_ref()))),
                    checksum_mismatch: stored
                        .is_some_and(|row| row.2.as_slice() != migration.checksum.as_ref()),
                })
            })
            .collect()
    }

    pub async fn migration_status(
        &self,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<MigrationStatus>> {
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let applied: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await
        .unwrap_or_default();
        Ok(migrator
            .iter()
            .map(|migration| {
                let stored = applied.iter().find(|row| row.0 == migration.version);
                let checksum = stored
                    .map(|row| checksum_hex(&row.2))
                    .or_else(|| Some(checksum_hex(migration.checksum.as_ref())));
                MigrationStatus {
                    name: migration.description.to_string(),
                    applied: stored.is_some_and(|row| row.1),
                    checksum,
                    checksum_mismatch: stored
                        .is_some_and(|row| row.2.as_slice() != migration.checksum.as_ref()),
                }
            })
            .collect())
    }

    async fn apply_fk_safe_sql(&self, sql: &str) -> Result<()> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await?;

        let result = async {
            sqlx::query("BEGIN EXCLUSIVE")
                .execute(&mut *connection)
                .await?;
            run_preflight(sql, &mut *connection).await?;
            for statement in executable_statements(sql) {
                sqlx::query(&statement).execute(&mut *connection).await?;
            }
            let violations: Vec<(String, i64, String, i64)> =
                sqlx::query_as("PRAGMA foreign_key_check")
                    .fetch_all(&mut *connection)
                    .await?;
            if !violations.is_empty() {
                return Err(Error::ForeignKeyCheckFailed(
                    violations
                        .iter()
                        .map(|(table, rowid, parent, fk)| {
                            format!("{table} row {rowid} references {parent} (fk {fk})")
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                ));
            }
            sqlx::query("COMMIT").execute(&mut *connection).await?;
            Ok(())
        }
        .await;

        if result.is_err() {
            let _ = sqlx::query("ROLLBACK").execute(&mut *connection).await;
        }
        let restore = sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await;
        if restore.is_err() {
            let _ = connection.close().await;
        }
        match (result, restore) {
            (Err(error), Err(restore)) => Err(Error::ForeignKeyRestoreFailed {
                original: error.to_string(),
                restore,
            }),
            (Err(error), Ok(_)) => Err(error),
            (Ok(_), Err(error)) => Err(Error::ForeignKeyEnforcementRestoreFailed(error)),
            (Ok(result), Ok(_)) => Ok(result),
        }
    }

    async fn register_namespace(&self, namespace: &str, hash: u32) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _che_orm_migration_namespaces (
                hash INTEGER PRIMARY KEY,
                namespace TEXT NOT NULL UNIQUE
            )",
        )
        .execute(&self.pool)
        .await?;
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT namespace FROM _che_orm_migration_namespaces WHERE hash = ?1",
        )
        .bind(i64::from(hash))
        .fetch_optional(&self.pool)
        .await?;
        match existing {
            Some(existing) if existing != namespace => Err(Error::UnsafeMigration(format!(
                "migration namespace hash collision between {existing:?} and {namespace:?}"
            ))),
            Some(_) => Ok(()),
            None => {
                sqlx::query(
                    "INSERT INTO _che_orm_migration_namespaces (hash, namespace) VALUES (?1, ?2)",
                )
                .bind(i64::from(hash))
                .bind(namespace)
                .execute(&self.pool)
                .await?;
                Ok(())
            }
        }
    }

    async fn validate_namespace_versions(&self, migrator: &Migrator, hash: u32) -> Result<()> {
        let applied: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_all(&self.pool)
                .await
                .unwrap_or_default();
        let expected = migrator
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>();
        if let Some(version) = applied
            .into_iter()
            .find(|version| ((*version >> 32) as u32) == hash && !expected.contains(version))
        {
            return Err(Error::UnsafeMigration(format!(
                "applied migration version {version} is missing from its namespace directory"
            )));
        }
        Ok(())
    }
}

fn namespaced_migration_version(namespace: &str, version: i64) -> Result<i64> {
    namespaced_migration_version_from_hash(namespace_hash(namespace), version)
}

fn namespaced_migration_version_from_hash(hash: u32, version: i64) -> Result<i64> {
    if !(0..=u32::MAX as i64).contains(&version) {
        return Err(Error::UnsafeMigration(format!(
            "migration version is outside the namespaced range: {version}"
        )));
    }

    Ok((i64::from(hash) << 32) | version)
}

fn namespace_hash(namespace: &str) -> u32 {
    namespace.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    }) & 0x7fff_ffff
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum PreflightRule {
    #[serde(rename = "unique")]
    Unique { table: String, columns: Vec<String> },
    #[serde(rename = "foreign_key")]
    ForeignKey {
        table: String,
        column: String,
        target_table: String,
    },
    #[serde(rename = "choices")]
    Choices {
        table: String,
        column: String,
        values: Vec<String>,
    },
    #[serde(rename = "max_length")]
    MaxLength {
        table: String,
        column: String,
        max_length: u32,
    },
}

async fn run_preflight(sql: &str, connection: &mut SqliteConnection) -> Result<()> {
    for line in sql.lines() {
        let Some(payload) = line.strip_prefix("-- che-orm: preflight ") else {
            continue;
        };
        let rule: PreflightRule = serde_json::from_str(payload).map_err(|error| {
            Error::UnsafeMigration(format!("invalid preflight directive: {error}"))
        })?;
        match rule {
            PreflightRule::Unique { table, columns } => {
                if columns.is_empty() {
                    return Err(Error::UnsafeMigration(
                        "unique preflight requires at least one column".to_string(),
                    ));
                }
                let columns = columns
                    .iter()
                    .map(|column| quote_identifier(column))
                    .collect::<Vec<_>>()
                    .join(", ");
                let non_null = columns
                    .split(", ")
                    .map(|column| format!("{column} IS NOT NULL"))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                let sql = format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} WHERE {non_null} GROUP BY {columns} HAVING COUNT(*) > 1)",
                    table = quote_identifier(&table),
                    columns = columns,
                    non_null = non_null,
                );
                let violated: i64 = sqlx::query_scalar(&sql).fetch_one(&mut *connection).await?;
                if violated != 0 {
                    return Err(Error::MigrationPreflightFailed {
                        rule: "unique".to_string(),
                        details: format!("duplicate values found in {table} ({columns})"),
                    });
                }
            }
            PreflightRule::ForeignKey {
                table,
                column,
                target_table,
            } => {
                let sql = format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} AS source LEFT JOIN {target_table} AS target ON target.\"id\" = source.{column} WHERE source.{column} IS NOT NULL AND target.\"id\" IS NULL)",
                    table = quote_identifier(&table),
                    target_table = quote_identifier(&target_table),
                    column = quote_identifier(&column),
                );
                let violated: i64 = sqlx::query_scalar(&sql).fetch_one(&mut *connection).await?;
                if violated != 0 {
                    return Err(Error::MigrationPreflightFailed {
                        rule: "foreign_key".to_string(),
                        details: format!("orphaned values found in {table}.{column}"),
                    });
                }
            }
            PreflightRule::Choices {
                table,
                column,
                values,
            } => {
                let placeholders = (1..=values.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = if values.is_empty() {
                    format!(
                        "SELECT EXISTS (SELECT 1 FROM {table} WHERE {column} IS NOT NULL)",
                        table = quote_identifier(&table),
                        column = quote_identifier(&column)
                    )
                } else {
                    format!(
                        "SELECT EXISTS (SELECT 1 FROM {table} WHERE {column} IS NOT NULL AND {column} NOT IN ({placeholders}))",
                        table = quote_identifier(&table),
                        column = quote_identifier(&column)
                    )
                };
                let mut query = sqlx::query_scalar(&sql);
                for value in &values {
                    query = query.bind(value);
                }
                let violated: i64 = query.fetch_one(&mut *connection).await?;
                if violated != 0 {
                    return Err(Error::MigrationPreflightFailed {
                        rule: "choices".to_string(),
                        details: format!(
                            "values outside the allowed set found in {table}.{column}"
                        ),
                    });
                }
            }
            PreflightRule::MaxLength {
                table,
                column,
                max_length,
            } => {
                let sql = format!(
                    "SELECT EXISTS (SELECT 1 FROM {table} WHERE {column} IS NOT NULL AND length({column}) > ?1)",
                    table = quote_identifier(&table),
                    column = quote_identifier(&column),
                );
                let violated: i64 = sqlx::query_scalar(&sql)
                    .bind(max_length)
                    .fetch_one(&mut *connection)
                    .await?;
                if violated != 0 {
                    return Err(Error::MigrationPreflightFailed {
                        rule: "max_length".to_string(),
                        details: format!(
                            "values exceeding {max_length} characters found in {table}.{column}"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

fn requires_fk_safe_rebuild(sql: &str) -> bool {
    sql.lines()
        .map(str::trim)
        .any(|line| line == SQLITE_FK_REBUILD_DIRECTIVE)
}

fn executable_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut single_quote = false;
    let mut double_quote = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        if line_comment {
            current.push(ch);
            if ch == '\n' {
                line_comment = false;
            }
            continue;
        }
        if block_comment {
            current.push(ch);
            if ch == '*' && chars.peek() == Some(&'/') {
                current.push(chars.next().unwrap());
                block_comment = false;
            }
            continue;
        }
        if !single_quote && !double_quote && ch == '-' && chars.peek() == Some(&'-') {
            current.push(ch);
            current.push(chars.next().unwrap());
            line_comment = true;
            continue;
        }
        if !single_quote && !double_quote && ch == '/' && chars.peek() == Some(&'*') {
            current.push(ch);
            current.push(chars.next().unwrap());
            block_comment = true;
            continue;
        }
        if ch == '\'' && !double_quote {
            if single_quote && chars.peek() == Some(&'\'') {
                current.push(ch);
                current.push(chars.next().unwrap());
                continue;
            }
            single_quote = !single_quote;
        } else if ch == '"' && !single_quote {
            if double_quote && chars.peek() == Some(&'"') {
                current.push(ch);
                current.push(chars.next().unwrap());
                continue;
            }
            double_quote = !double_quote;
        }
        if ch == ';' && !single_quote && !double_quote {
            let trimmed = current.trim();
            let upper = trimmed.to_ascii_uppercase();
            if upper.contains("CREATE TRIGGER")
                && upper.contains("BEGIN")
                && !ends_with_end(trimmed)
            {
                current.push(ch);
            } else {
                push_statement(&mut statements, &mut current);
            }
        } else {
            current.push(ch);
        }
    }
    push_statement(&mut statements, &mut current);
    statements
}

fn ends_with_end(statement: &str) -> bool {
    let mut end = statement.len();
    loop {
        let trimmed = statement[..end].trim_end();
        if let Some(comment_start) = trimmed.rfind("/*")
            && trimmed[comment_start + 2..]
                .find("*/")
                .is_some_and(|offset| comment_start + 2 + offset + 2 == trimmed.len())
        {
            end = comment_start;
            continue;
        }
        let line_start = trimmed.rfind('\n').map_or(0, |position| position + 1);
        if let Some(comment_start) = trimmed[line_start..].find("--") {
            end = line_start + comment_start;
            continue;
        }
        return trimmed
            .rsplit_once(|character: char| character.is_whitespace())
            .map_or(trimmed.eq_ignore_ascii_case("END"), |(_, word)| {
                word.eq_ignore_ascii_case("END")
            });
    }
}

fn push_statement(statements: &mut Vec<String>, current: &mut String) {
    let statement = current.trim();
    if !statement.is_empty()
        && statement
            .lines()
            .map(str::trim)
            .any(|line| !line.is_empty() && !line.starts_with("--") && !line.starts_with("/*"))
    {
        statements.push(statement.to_string());
    }
    current.clear();
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn checksum_hex(checksum: &[u8]) -> String {
    checksum.iter().map(|byte| format!("{byte:02x}")).collect()
}
