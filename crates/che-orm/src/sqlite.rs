use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection, SqlitePool, sqlite::SqlitePoolOptions};

use crate::{
    Error, Model, Result, Signals, create_table_sql, migration::SQLITE_FK_REBUILD_DIRECTIVE,
};

#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
    signals: Signals,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    pub name: String,
    pub applied: bool,
    pub checksum: Option<String>,
    pub checksum_mismatch: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SafeMigrationResult {
    Applied,
    AlreadyApplied,
}

impl SqliteBackend {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(connection)
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
            self.apply_fk_safe_sql(sql, None).await?;
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
        self.apply_migrations_dir_inner(None, migrations_dir.as_ref())
            .await
    }

    pub async fn apply_migrations_dir_with_namespace(
        &self,
        namespace: &str,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<String>> {
        self.apply_migrations_dir_inner(Some(namespace), migrations_dir.as_ref())
            .await
    }

    pub async fn migration_status(
        &self,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<MigrationStatus>> {
        self.ensure_migrations_table().await?;
        let applied: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, checksum FROM _che_orm_migrations ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        let mut files = migration_files(migrations_dir.as_ref())?;
        files.sort();
        let mut statuses = Vec::new();
        for path in files {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let checksum = migration_checksum(&fs::read(&path)?);
            let stored = applied.iter().find(|(stored_name, _)| stored_name == &name);
            let checksum_mismatch = stored
                .and_then(|(_, stored_checksum)| stored_checksum.as_deref())
                .is_some_and(|stored_checksum| stored_checksum != checksum);
            statuses.push(MigrationStatus {
                name,
                applied: stored.is_some(),
                checksum: stored
                    .and_then(|(_, stored_checksum)| stored_checksum.clone())
                    .or(Some(checksum)),
                checksum_mismatch,
            });
        }
        Ok(statuses)
    }

    async fn apply_migrations_dir_inner(
        &self,
        namespace: Option<&str>,
        migrations_dir: &Path,
    ) -> Result<Vec<String>> {
        self.ensure_migrations_table().await?;

        let mut files = migration_files(migrations_dir)?;
        files.sort();

        let mut applied = Vec::new();
        for path in files {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let name = match namespace {
                Some(namespace) => format!("{namespace}/{file_name}"),
                None => file_name,
            };
            let sql = fs::read(&path)?;
            let checksum = migration_checksum(&sql);
            let existing: Option<(i64, Option<String>)> =
                sqlx::query_as("SELECT id, checksum FROM _che_orm_migrations WHERE name = ?1")
                    .bind(&name)
                    .fetch_optional(&self.pool)
                    .await?;

            if let Some((id, stored_checksum)) = existing {
                if let Some(stored_checksum) = stored_checksum {
                    if stored_checksum != checksum {
                        return Err(Error::MigrationChecksumMismatch {
                            name,
                            expected: stored_checksum,
                            actual: checksum,
                        });
                    }
                } else {
                    sqlx::query("UPDATE _che_orm_migrations SET checksum = ?1 WHERE id = ?2")
                        .bind(&checksum)
                        .bind(id)
                        .execute(&self.pool)
                        .await?;
                }
                continue;
            }

            let sql = std::str::from_utf8(&sql)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if requires_fk_safe_rebuild(sql) {
                if self
                    .apply_fk_safe_sql(sql, Some((&name, &checksum)))
                    .await?
                    == SafeMigrationResult::AlreadyApplied
                {
                    continue;
                }
            } else {
                let mut tx = self.pool.begin().await?;
                run_preflight(sql, &mut *tx).await?;
                for statement in executable_statements(sql) {
                    sqlx::query(&statement).execute(&mut *tx).await?;
                }
                sqlx::query("INSERT INTO _che_orm_migrations (name, checksum) VALUES (?1, ?2)")
                    .bind(&name)
                    .bind(&checksum)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }

            applied.push(name);
        }

        Ok(applied)
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _che_orm_migrations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                checksum TEXT,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .execute(&self.pool)
        .await?;
        let columns = sqlx::query("PRAGMA table_info(_che_orm_migrations)")
            .fetch_all(&self.pool)
            .await?;
        if !columns
            .iter()
            .any(|column| column.get::<String, _>("name") == "checksum")
        {
            sqlx::query("ALTER TABLE _che_orm_migrations ADD COLUMN checksum TEXT")
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn apply_fk_safe_sql(
        &self,
        sql: &str,
        migration: Option<(&str, &str)>,
    ) -> Result<SafeMigrationResult> {
        let mut connection = self.pool.acquire().await?;
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await?;

        let result = async {
            sqlx::query("BEGIN EXCLUSIVE")
                .execute(&mut *connection)
                .await?;
            if let Some((name, checksum)) = migration {
                let existing: Option<(i64, Option<String>)> =
                    sqlx::query_as("SELECT id, checksum FROM _che_orm_migrations WHERE name = ?1")
                        .bind(name)
                        .fetch_optional(&mut *connection)
                        .await?;
                if let Some((id, stored_checksum)) = existing {
                    if let Some(stored_checksum) = stored_checksum {
                        if stored_checksum != checksum {
                            return Err(Error::MigrationChecksumMismatch {
                                name: name.to_string(),
                                expected: stored_checksum,
                                actual: checksum.to_string(),
                            });
                        }
                    } else {
                        sqlx::query("UPDATE _che_orm_migrations SET checksum = ?1 WHERE id = ?2")
                            .bind(checksum)
                            .bind(id)
                            .execute(&mut *connection)
                            .await?;
                    }
                    sqlx::query("COMMIT").execute(&mut *connection).await?;
                    return Ok(SafeMigrationResult::AlreadyApplied);
                }
            }
            run_preflight(sql, &mut *connection).await?;
            for statement in executable_statements(sql) {
                sqlx::query(&statement).execute(&mut *connection).await?;
            }
            if let Some((name, checksum)) = migration {
                sqlx::query("INSERT INTO _che_orm_migrations (name, checksum) VALUES (?1, ?2)")
                    .bind(name)
                    .bind(checksum)
                    .execute(&mut *connection)
                    .await?;
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
            Ok(SafeMigrationResult::Applied)
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
}

fn migration_files(migrations_dir: &Path) -> Result<Vec<PathBuf>> {
    if !migrations_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(migrations_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "sql") {
            files.push(path);
        }
    }
    Ok(files)
}

fn migration_checksum(sql: &[u8]) -> String {
    let digest = Sha256::digest(sql);
    format!("{digest:x}")
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
