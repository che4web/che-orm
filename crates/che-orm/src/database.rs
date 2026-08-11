#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
use std::fs;
#[cfg(feature = "sqlite")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(feature = "migration-atlas")]
use std::process::Command;

#[cfg(feature = "postgres")]
use crate::PostgresBackend;
#[cfg(feature = "postgres")]
use crate::PostgresQueryBuilder;
#[cfg(feature = "sqlite")]
use crate::QueryBuilder;
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
use crate::Schema;
#[cfg(feature = "sqlite")]
use crate::SqliteModel;
#[cfg(feature = "sqlite")]
use crate::UpdateBuilder;
#[cfg(feature = "postgres")]
use crate::postgres::PostgresModelManager;
use crate::{DatabaseValue, Result};
#[cfg(feature = "postgres")]
use crate::{Error, PostgresModel};
#[cfg(feature = "sqlite")]
use crate::{SqliteBackend, manager::ModelManager};
#[cfg(feature = "migration-native")]
use crate::{diff_schemas, sqlite_migration_sql, validate_migration};
#[cfg(feature = "migration-atlas")]
use crate::{postgres_schema_sql, sqlite_schema_sql};

#[derive(Debug, Clone)]
pub enum Database {
    #[cfg(feature = "sqlite")]
    Sqlite {
        backend: SqliteBackend,
        migrations_dir: PathBuf,
    },
    #[cfg(feature = "postgres")]
    Postgres {
        backend: PostgresBackend,
        migrations_dir: PathBuf,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    pub name: String,
    pub applied: bool,
    pub checksum: Option<String>,
    pub checksum_mismatch: bool,
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
#[derive(Debug, Clone)]
pub struct MigrationOptions {
    migrations_dir: PathBuf,
    name: String,
}

#[cfg(feature = "migration-atlas")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationDialect {
    Sqlite,
    Postgres,
}

#[cfg(feature = "migration-atlas")]
#[derive(Debug, Clone)]
pub struct AtlasOptions {
    pub binary: String,
    pub dev_url: String,
    pub dialect: MigrationDialect,
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedMigration {
    pub path: Option<PathBuf>,
    pub changes: usize,
}

pub struct DatabaseCreateBuilder<'db, M> {
    database: &'db Database,
    values: Vec<(String, DatabaseValue)>,
    _model: std::marker::PhantomData<M>,
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
impl MigrationOptions {
    pub fn new(migrations_dir: impl Into<PathBuf>) -> Self {
        Self {
            migrations_dir: migrations_dir.into(),
            name: "auto".to_string(),
        }
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }
}

#[cfg(feature = "migration-atlas")]
impl AtlasOptions {
    pub fn new(binary: impl Into<String>, dev_url: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            dev_url: dev_url.into(),
            dialect: MigrationDialect::Sqlite,
        }
    }

    pub fn from_env() -> Self {
        Self {
            binary: std::env::var("CHE_ORM_ATLAS_BIN").unwrap_or_else(|_| "atlas".to_string()),
            dev_url: std::env::var("CHE_ORM_ATLAS_DEV_URL")
                .unwrap_or_else(|_| "sqlite://file?mode=memory".to_string()),
            dialect: MigrationDialect::Sqlite,
        }
    }

    pub fn with_dialect(mut self, dialect: MigrationDialect) -> Self {
        self.dialect = dialect;
        self
    }
}

impl Database {
    #[cfg(feature = "sqlite")]
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::Sqlite {
            backend: SqliteBackend::connect(url).await?,
            migrations_dir: PathBuf::from("migrations"),
        })
    }

    #[cfg(feature = "postgres")]
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::Postgres {
            backend: PostgresBackend::connect(url).await?,
            migrations_dir: PathBuf::from("migrations"),
        })
    }

    pub fn with_migrations_dir(mut self, path: impl Into<PathBuf>) -> Self {
        match &mut self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite { migrations_dir, .. } => {
                *migrations_dir = path.into();
            }
            #[cfg(feature = "postgres")]
            Self::Postgres { migrations_dir, .. } => {
                *migrations_dir = path.into();
            }
        }
        self
    }

    #[cfg(feature = "sqlite")]
    pub fn as_sqlite(&self) -> &SqliteBackend {
        match self {
            Self::Sqlite { backend, .. } => backend,
        }
    }

    #[cfg(feature = "postgres")]
    pub fn as_postgres(&self) -> &PostgresBackend {
        match self {
            Self::Postgres { backend, .. } => backend,
        }
    }

    pub async fn migrate(&self) -> Result<Vec<String>> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite {
                backend,
                migrations_dir,
            } => backend.apply_migrations_dir(migrations_dir).await,
            #[cfg(feature = "postgres")]
            Self::Postgres {
                backend,
                migrations_dir,
            } => backend.migrate(migrations_dir).await,
        }
    }

    pub async fn migration_status(&self) -> Result<Vec<MigrationStatus>> {
        match self {
            #[cfg(feature = "sqlite")]
            Self::Sqlite {
                backend,
                migrations_dir,
            } => backend.migration_status(migrations_dir).await,
            #[cfg(feature = "postgres")]
            Self::Postgres {
                backend,
                migrations_dir,
            } => backend.migration_status(migrations_dir).await,
        }
    }

    #[cfg(feature = "sqlite")]
    pub fn signals(&self) -> &crate::Signals {
        self.as_sqlite().signals()
    }

    #[cfg(feature = "sqlite")]
    pub fn pool(&self) -> &sqlx::SqlitePool {
        self.as_sqlite().pool()
    }

    #[cfg(feature = "sqlite")]
    pub async fn apply_sql(&self, sql: &str) -> Result<()> {
        self.as_sqlite().apply_sql(sql).await
    }

    #[cfg(feature = "sqlite")]
    pub async fn apply_migrations_dir(&self, migrations_dir: &Path) -> Result<Vec<String>> {
        self.as_sqlite().apply_migrations_dir(migrations_dir).await
    }

    #[cfg(feature = "sqlite")]
    pub fn create<M>(&self) -> DatabaseCreateBuilder<'_, M>
    where
        M: SqliteModel,
    {
        DatabaseCreateBuilder {
            database: self,
            values: Vec::new(),
            _model: std::marker::PhantomData,
        }
    }

    #[cfg(feature = "postgres")]
    pub fn create<M>(&self) -> DatabaseCreateBuilder<'_, M>
    where
        M: PostgresModel,
    {
        DatabaseCreateBuilder {
            database: self,
            values: Vec::new(),
            _model: std::marker::PhantomData,
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn create_table<M>(&self) -> Result<()>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => backend.create_table::<M>().await,
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn get<M>(&self, id: M::Id) -> Result<M>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).get(id).await,
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn get<M>(&self, id: M::Id) -> Result<M>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => PostgresModelManager::<M>::new(backend)
                .get(id.into())
                .await?
                .ok_or(Error::NotFound),
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn all<M>(&self) -> Result<Vec<M>>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).all().await,
        }
    }

    #[cfg(feature = "sqlite")]
    pub fn query<M>(&self) -> QueryBuilder<'_, M>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => QueryBuilder::<M>::new(backend),
        }
    }

    #[cfg(feature = "postgres")]
    pub fn query<M>(&self) -> PostgresQueryBuilder<'_, M>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => PostgresQueryBuilder::<M>::new(backend),
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn all<M>(&self) -> Result<Vec<M>>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => PostgresModelManager::<M>::new(backend).all().await,
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn update<M>(&self, id: M::Id, data: M::Update) -> Result<M>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).update(id, data).await,
        }
    }

    #[cfg(feature = "sqlite")]
    pub fn update_fields<M>(&self, id: M::Id) -> UpdateBuilder<'_, M>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).update_fields(id),
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn update<M>(&self, id: M::Id, data: M::Update) -> Result<M>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => PostgresModelManager::<M>::new(backend)
                .update(id.into(), data)
                .await?
                .ok_or(Error::NotFound),
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn save<M>(&self, model: &M) -> Result<M>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).save(model).await,
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn save<M>(&self, model: &M) -> Result<M>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => PostgresModelManager::<M>::new(backend)
                .save(model)
                .await?
                .ok_or(Error::NotFound),
        }
    }

    #[cfg(feature = "sqlite")]
    pub async fn delete<M>(&self, id: M::Id) -> Result<()>
    where
        M: SqliteModel,
    {
        match self {
            Self::Sqlite { backend, .. } => ModelManager::<M>::new(backend).delete(id).await,
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn delete<M>(&self, id: M::Id) -> Result<()>
    where
        M: PostgresModel,
    {
        match self {
            Self::Postgres { backend, .. } => {
                PostgresModelManager::<M>::new(backend)
                    .delete(id.into())
                    .await?;
                Ok(())
            }
        }
    }

    #[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
    pub fn makemigrations(
        &self,
        schema: &Schema,
        options: MigrationOptions,
    ) -> Result<GeneratedMigration> {
        generate_migrations(schema, options)
    }
}

#[cfg(feature = "sqlite")]
impl<'db, M> DatabaseCreateBuilder<'db, M>
where
    M: SqliteModel,
{
    pub fn set<V>(mut self, field: &str, value: V) -> Self
    where
        V: Into<DatabaseValue>,
    {
        self.values.push((field.to_string(), value.into()));
        self
    }

    pub fn set_null(mut self, field: &str) -> Self {
        self.values.push((field.to_string(), DatabaseValue::Null));
        self
    }

    pub async fn execute(self) -> Result<M> {
        match self.database {
            #[cfg(feature = "sqlite")]
            Database::Sqlite { backend, .. } => {
                let mut builder = ModelManager::<M>::new(backend).create();
                for (field, value) in self.values {
                    builder = builder.set(&field, value);
                }
                builder.execute().await
            }
        }
    }
}

#[cfg(feature = "postgres")]
impl<'db, M> DatabaseCreateBuilder<'db, M>
where
    M: PostgresModel,
{
    pub fn set<V>(mut self, field: &str, value: V) -> Self
    where
        V: Into<DatabaseValue>,
    {
        self.values.push((field.to_string(), value.into()));
        self
    }

    pub fn set_null(mut self, field: &str) -> Self {
        self.values.push((field.to_string(), DatabaseValue::Null));
        self
    }

    pub async fn execute(self) -> Result<M> {
        match self.database {
            Database::Postgres { backend, .. } => {
                PostgresModelManager::<M>::new(backend)
                    .create_values(self.values)
                    .await
            }
        }
    }
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
pub fn generate_migrations(
    schema: &Schema,
    options: MigrationOptions,
) -> Result<GeneratedMigration> {
    fs::create_dir_all(&options.migrations_dir)?;
    let snapshot_path = options.migrations_dir.join("schema.json");
    #[cfg(feature = "migration-native")]
    let generated = generate_native(schema, &options.migrations_dir, &options.name)?;
    #[cfg(feature = "migration-atlas")]
    let generated = generate_atlas(
        schema,
        &options.migrations_dir,
        &options.name,
        &AtlasOptions::from_env().with_dialect(compiled_dialect()),
    )?;
    if generated.path.is_some() {
        schema.save(snapshot_path)?;
    }
    Ok(generated)
}

#[cfg(all(feature = "sqlite", feature = "migration-atlas"))]
fn compiled_dialect() -> MigrationDialect {
    MigrationDialect::Sqlite
}

#[cfg(all(feature = "postgres", feature = "migration-atlas"))]
fn compiled_dialect() -> MigrationDialect {
    MigrationDialect::Postgres
}

#[cfg(feature = "migration-native")]
fn generate_native(
    schema: &Schema,
    migrations_dir: &Path,
    name: &str,
) -> Result<GeneratedMigration> {
    let old_schema = Schema::load_or_empty(migrations_dir.join("schema.json"))?;
    let migration = diff_schemas(&old_schema, schema);
    validate_migration(&migration)?;
    let changes = migration.changes.len();
    if changes == 0 {
        return Ok(GeneratedMigration {
            path: None,
            changes,
        });
    }

    let migration_path = migrations_dir.join(format!(
        "{}_{}.sql",
        next_migration_version(migrations_dir)?,
        slugify(name)
    ));
    fs::write(
        &migration_path,
        format!("{}\n", sqlite_migration_sql(&migration)),
    )?;
    Ok(GeneratedMigration {
        path: Some(migration_path),
        changes,
    })
}

#[cfg(feature = "migration-atlas")]
fn generate_atlas(
    schema: &Schema,
    migrations_dir: &Path,
    name: &str,
    atlas: &AtlasOptions,
) -> Result<GeneratedMigration> {
    let before = sql_files(migrations_dir)?;
    let desired_path = std::env::temp_dir().join(format!(
        "che_orm_atlas_schema_{}_{}.sql",
        std::process::id(),
        next_migration_version(migrations_dir)?
    ));
    let desired_schema = match atlas.dialect {
        MigrationDialect::Sqlite => sqlite_schema_sql(schema),
        MigrationDialect::Postgres => postgres_schema_sql(schema),
    };
    fs::write(&desired_path, format!("{desired_schema}\n"))?;

    let dir = fs::canonicalize(migrations_dir)?;
    let desired = fs::canonicalize(&desired_path)?;
    let output = Command::new(&atlas.binary)
        .args([
            "migrate",
            "diff",
            &slugify(name),
            "--dir",
            &format!("file://{}", dir.display()),
            "--to",
            &format!("file://{}", desired.display()),
            "--dev-url",
            &atlas.dev_url,
        ])
        .output()
        .map_err(|source| Error::AtlasUnavailable {
            binary: atlas.binary.clone(),
            source,
        });
    let _ = fs::remove_file(&desired_path);
    let output = output?;
    if !output.status.success() {
        return Err(Error::AtlasFailed {
            details: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    let mut created = sql_files(migrations_dir)?
        .into_iter()
        .filter(|path| !before.contains(path))
        .collect::<Vec<_>>();
    if created.len() > 1 {
        return Err(Error::AtlasFailed {
            details: "Atlas created more than one SQL migration".to_string(),
        });
    }
    let Some(path) = created.pop() else {
        return Ok(GeneratedMigration {
            path: None,
            changes: 0,
        });
    };
    if !is_sqlx_migration_file(&path) {
        return Err(Error::AtlasFailed {
            details: format!(
                "Atlas created an invalid SQLx migration name: {}",
                path.display()
            ),
        });
    }
    Ok(GeneratedMigration {
        path: Some(path),
        changes: 1,
    })
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
fn sql_files(migrations_dir: &Path) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(migrations_dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect())
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
fn next_migration_version(migrations_dir: &Path) -> Result<u64> {
    let max = sql_files(migrations_dir)?
        .iter()
        .filter_map(|path| {
            path.file_stem()?
                .to_str()?
                .split('_')
                .next()?
                .parse::<u64>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    let now = chrono::Utc::now()
        .format("%Y%m%d%H%M%S")
        .to_string()
        .parse::<u64>()
        .unwrap();
    Ok(now.max(max.saturating_add(1)))
}

#[cfg(feature = "migration-atlas")]
fn is_sqlx_migration_file(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let Some((version, description)) = stem.split_once('_') else {
        return false;
    };
    !version.is_empty()
        && version.chars().all(|character| character.is_ascii_digit())
        && !description.is_empty()
}

#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
fn slugify(value: &str) -> String {
    let mut slug = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_string()
}
