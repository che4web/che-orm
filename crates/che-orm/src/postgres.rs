use std::path::Path;

use sqlx::{
    PgPool,
    migrate::{Migrate, Migrator},
    postgres::PgPoolOptions,
};

use crate::{DatabaseValue, FieldInfo, FieldType, MigrationStatus, PostgresModel, Result};

#[derive(Debug, Clone)]
pub struct PostgresBackend {
    pool: PgPool,
}

pub struct PostgresModelManager<'db, M> {
    db: &'db PostgresBackend,
    _model: std::marker::PhantomData<M>,
}

impl<'db, M: PostgresModel> PostgresModelManager<'db, M> {
    pub fn new(db: &'db PostgresBackend) -> Self {
        Self {
            db,
            _model: std::marker::PhantomData,
        }
    }

    pub async fn all(&self) -> Result<Vec<M>> {
        let sql = format!("SELECT * FROM {}", M::table_name());
        let rows = sqlx::query(&sql).fetch_all(self.db.pool()).await?;
        rows.iter()
            .map(|row| M::from_postgres_row(row).map_err(Into::into))
            .collect()
    }

    pub async fn filter_eq(&self, field: &str, value: DatabaseValue) -> Result<Vec<M>> {
        let field_info = field_info::<M>(field)?;
        let sql = format!("SELECT * FROM {} WHERE {field} = $1", M::table_name());
        let rows = bind_value(sqlx::query(&sql), value, field_info)
            .fetch_all(self.db.pool())
            .await?;
        rows.iter()
            .map(|row| M::from_postgres_row(row).map_err(Into::into))
            .collect()
    }

    pub async fn get(&self, id: i64) -> Result<Option<M>> {
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "SELECT * FROM {} WHERE {} = $1 LIMIT 1",
            M::table_name(),
            primary_key.db_name
        );
        sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?
            .map(|row| M::from_postgres_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn create(&self, model: &M) -> Result<M> {
        self.create_values(
            M::save_values(model)
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect(),
        )
        .await
    }

    pub async fn create_values(&self, raw_values: Vec<(String, DatabaseValue)>) -> Result<M> {
        let mut values: Vec<(&'static FieldInfo, DatabaseValue)> =
            Vec::with_capacity(raw_values.len());
        for (name, value) in raw_values {
            let field = field_info::<M>(&name)?;
            if field.primary_key || field.auto || field.auto_now || field.auto_now_add {
                return Err(crate::Error::ReadonlyField(name));
            }
            values.push((field, value));
        }
        if values.is_empty() {
            let sql = format!("INSERT INTO {} DEFAULT VALUES RETURNING *", M::table_name());
            return Ok(M::from_postgres_row(
                &sqlx::query(&sql).fetch_one(self.db.pool()).await?,
            )?);
        }
        let columns = values
            .iter()
            .map(|(field, _)| field.db_name)
            .collect::<Vec<_>>()
            .join(", ");
        let placeholders = (1..=values.len())
            .map(|index| format!("${index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT INTO {} ({columns}) VALUES ({placeholders}) RETURNING *",
            M::table_name()
        );
        let mut query = sqlx::query(&sql);
        for (field, value) in values {
            query = bind_value(query, value, field);
        }
        Ok(M::from_postgres_row(
            &query.fetch_one(self.db.pool()).await?,
        )?)
    }

    pub async fn update(&self, id: i64, data: M::Update) -> Result<Option<M>> {
        let values = M::update_values(data);
        self.update_values(id, values).await
    }

    pub async fn save(&self, model: &M) -> Result<Option<M>> {
        self.update_values(model.id().into(), M::save_values(model))
            .await
    }

    async fn update_values(
        &self,
        id: i64,
        values: Vec<(&'static str, DatabaseValue)>,
    ) -> Result<Option<M>> {
        if values.is_empty() {
            return Err(crate::Error::EmptyUpdate);
        }
        let assignments = values
            .iter()
            .enumerate()
            .map(|(index, (name, _))| format!("{name} = ${}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "UPDATE {} SET {assignments} WHERE {} = ${} RETURNING *",
            M::table_name(),
            primary_key.db_name,
            values.len() + 1
        );
        let mut query = sqlx::query(&sql);
        for (name, value) in values {
            query = bind_value(query, value, field_info::<M>(name)?);
        }
        let row = query.bind(id).fetch_optional(self.db.pool()).await?;
        row.map(|row| M::from_postgres_row(&row).map_err(Into::into))
            .transpose()
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        let primary_key = primary_key::<M>()?;
        let sql = format!(
            "DELETE FROM {} WHERE {} = $1",
            M::table_name(),
            primary_key.db_name
        );
        Ok(sqlx::query(&sql)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected()
            != 0)
    }
}

fn field_info<M: PostgresModel>(name: &str) -> Result<&'static FieldInfo> {
    M::fields()
        .iter()
        .find(|field| field.db_name == name || field.rust_name == name)
        .ok_or_else(|| crate::Error::UnknownField(name.to_string()))
}

fn primary_key<M: PostgresModel>() -> Result<&'static FieldInfo> {
    M::primary_key().ok_or(crate::Error::MissingPrimaryKey)
}

fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: DatabaseValue,
    field: &FieldInfo,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        DatabaseValue::I64(value) => query.bind(value),
        DatabaseValue::String(value) => query.bind(value),
        DatabaseValue::Bool(value) => query.bind(value),
        DatabaseValue::F64(value) => query.bind(value),
        DatabaseValue::DateTime(value) => query.bind(value),
        DatabaseValue::Json(value) => query.bind(sqlx::types::Json(value)),
        DatabaseValue::Null => match field.ty {
            FieldType::Integer => query.bind(Option::<i64>::None),
            FieldType::Text | FieldType::Choice | FieldType::FilePath => {
                query.bind(Option::<String>::None)
            }
            FieldType::Boolean => query.bind(Option::<bool>::None),
            FieldType::Real => query.bind(Option::<f64>::None),
            FieldType::DateTime => query.bind(Option::<chrono::NaiveDateTime>::None),
            FieldType::Json => query.bind(Option::<sqlx::types::Json<serde_json::Value>>::None),
        },
    }
}

impl PostgresBackend {
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self {
            pool: PgPoolOptions::new().connect(url).await?,
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self, migrations_dir: impl AsRef<Path>) -> Result<Vec<String>> {
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

    pub async fn migration_status(
        &self,
        migrations_dir: impl AsRef<Path>,
    ) -> Result<Vec<MigrationStatus>> {
        let migrator = Migrator::new(migrations_dir.as_ref()).await?;
        let mut connection = self.pool.acquire().await?;
        connection.ensure_migrations_table().await?;
        let applied: Vec<(i64, bool, Vec<u8>)> = sqlx::query_as(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(migrator
            .iter()
            .map(|migration| {
                let stored = applied.iter().find(|row| row.0 == migration.version);
                MigrationStatus {
                    name: migration.description.to_string(),
                    applied: stored.is_some_and(|row| row.1),
                    checksum: stored
                        .map(|row| checksum_hex(&row.2))
                        .or_else(|| Some(checksum_hex(migration.checksum.as_ref()))),
                    checksum_mismatch: stored
                        .is_some_and(|row| row.2.as_slice() != migration.checksum.as_ref()),
                }
            })
            .collect())
    }
}

fn checksum_hex(checksum: &[u8]) -> String {
    checksum.iter().map(|byte| format!("{byte:02x}")).collect()
}
