use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FilePath(String);

impl FilePath {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.starts_with('/')
            || value.contains('\\')
            || value.contains('\0')
            || value
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(Error::InvalidFilePath(value));
        }
        let path = Path::new(&value);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                        | std::path::Component::CurDir
                )
            })
        {
            return Err(Error::InvalidFilePath(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for FilePath {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<Path> for FilePath {
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl FromStr for FilePath {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

impl std::fmt::Display for FilePath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl serde::Serialize for FilePath {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for FilePath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for FilePath {
    fn encode_by_ref(
        &self,
        arguments: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&self.0, arguments)
    }
}

impl sqlx::Type<sqlx::Sqlite> for FilePath {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for FilePath {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<'r, sqlx::Sqlite>>::decode(value)?;
        Self::new(value).map_err(|error| Box::new(error) as sqlx::error::BoxDynError)
    }
}

pub trait FileStorage {
    fn store(&self, bytes: &[u8], extension: Option<&str>) -> Result<FilePath>;
    fn read(&self, path: &FilePath) -> Result<Vec<u8>>;
    fn delete(&self, path: &FilePath) -> Result<()>;
    fn exists(&self, path: &FilePath) -> Result<bool>;
}

#[derive(Debug, Clone)]
pub struct LocalFileStorage {
    root: PathBuf,
}

impl LocalFileStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn full_path(&self, path: &FilePath) -> PathBuf {
        self.root.join(path.as_str())
    }
}

impl FileStorage for LocalFileStorage {
    fn store(&self, bytes: &[u8], extension: Option<&str>) -> Result<FilePath> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        if let Some(extension) = extension {
            if extension.is_empty()
                || !extension.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                return Err(Error::InvalidFileExtension(extension.to_string()));
            }
        }
        fs::create_dir_all(&self.root)?;
        for _ in 0..10 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let prefix = format!("{:02x}/{:02x}", (nanos >> 8) & 0xff, nanos & 0xff);
            let filename = match extension {
                Some(extension) => format!(
                    "{:x}-{}.{}",
                    nanos,
                    COUNTER.fetch_add(1, Ordering::Relaxed),
                    extension
                ),
                None => format!("{:x}-{}", nanos, COUNTER.fetch_add(1, Ordering::Relaxed)),
            };
            let path = FilePath::new(format!("{prefix}/{filename}"))?;
            let full = self.full_path(&path);
            if full.exists() {
                continue;
            }
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent)?;
            }
            let temporary = full.with_extension("uploading");
            fs::write(&temporary, bytes)?;
            fs::rename(&temporary, &full)?;
            return Ok(path);
        }
        Err(Error::Storage(
            "unable to allocate a unique file path".to_string(),
        ))
    }

    fn read(&self, path: &FilePath) -> Result<Vec<u8>> {
        Ok(fs::read(self.full_path(path))?)
    }
    fn delete(&self, path: &FilePath) -> Result<()> {
        Ok(fs::remove_file(self.full_path(path))?)
    }
    fn exists(&self, path: &FilePath) -> Result<bool> {
        Ok(self.full_path(path).try_exists()?)
    }
}
