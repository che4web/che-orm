use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::{Database, MigrationStatus, Result, Schema};
#[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
use crate::{GeneratedMigration, MigrationOptions};

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeSettings {
    pub database: DatabaseSettings,
    #[serde(default)]
    pub migrations: MigrationSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MigrationSettings {
    #[serde(default = "default_migrations_dir")]
    pub dir: PathBuf,
}

pub trait Application {
    fn schema(&self) -> Schema;
    fn settings(&self) -> Result<RuntimeSettings>;
}

impl RuntimeSettings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }
}

impl Default for MigrationSettings {
    fn default() -> Self {
        Self {
            dir: default_migrations_dir(),
        }
    }
}

pub struct Manager<A> {
    app: A,
}

impl<A: Application> Manager<A> {
    pub fn new(app: A) -> Self {
        Self { app }
    }

    pub fn application(&self) -> &A {
        &self.app
    }

    pub fn schema(&self) -> Schema {
        self.app.schema()
    }

    pub fn settings(&self) -> Result<RuntimeSettings> {
        self.app.settings()
    }

    pub async fn connect(&self) -> Result<Database> {
        let settings = self.settings()?;
        Ok(Database::connect(&settings.database.url)
            .await?
            .with_migrations_dir(settings.migrations.dir))
    }

    #[cfg(any(feature = "migration-native", feature = "migration-atlas"))]
    pub fn makemigrations(&self, name: impl Into<String>) -> Result<GeneratedMigration> {
        let settings = self.settings()?;
        #[cfg(feature = "postgres")]
        #[cfg(feature = "migration-native")]
        {
            return Err(crate::Error::UnsafeMigration(
                "native migrations are only supported by SQLite; use manual SQL or Atlas"
                    .to_string(),
            ));
        }
        let options = MigrationOptions::new(&settings.migrations.dir).named(name);
        crate::generate_migrations(&self.schema(), options)
    }

    pub async fn migrate(&self) -> Result<Vec<String>> {
        self.connect().await?.migrate().await
    }

    pub async fn status(&self) -> Result<Vec<MigrationStatus>> {
        self.connect().await?.migration_status().await
    }
}

fn default_migrations_dir() -> PathBuf {
    PathBuf::from("migrations")
}
