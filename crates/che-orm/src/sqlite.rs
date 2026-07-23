use std::{
    fs,
    path::{Path, PathBuf},
};

use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

use crate::{Model, Result, create_table_sql};

#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new().connect(url).await?;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn create_table<M: Model>(&self) -> Result<()> {
        self.apply_sql(&create_table_sql::<M>()).await
    }

    pub async fn apply_sql(&self, sql: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
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
            let already_applied: Option<i64> =
                sqlx::query_scalar("SELECT id FROM _che_orm_migrations WHERE name = ?1")
                    .bind(&name)
                    .fetch_optional(&self.pool)
                    .await?;

            if already_applied.is_some() {
                continue;
            }

            let sql = fs::read_to_string(&path)?;
            let mut tx = self.pool.begin().await?;
            for statement in executable_statements(&sql) {
                sqlx::query(&statement).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO _che_orm_migrations (name) VALUES (?1)")
                .bind(&name)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;

            applied.push(name);
        }

        Ok(applied)
    }

    async fn ensure_migrations_table(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS _che_orm_migrations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
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

fn executable_statements(sql: &str) -> Vec<String> {
    sql.split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .filter(|statement| {
            statement
                .lines()
                .map(str::trim)
                .any(|line| !line.is_empty() && !line.starts_with("--"))
        })
        .map(str::to_string)
        .collect()
}
