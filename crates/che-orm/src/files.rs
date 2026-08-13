use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use cap_std::{ambient_authority, fs::Dir};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// A validated, relative path stored in a file model field.
///
/// Absolute paths, traversal segments, backslashes, and empty paths are
/// rejected, allowing storage implementations to safely join it to their root.
pub struct FilePath(String);

impl FilePath {
    /// Validates and constructs a relative storage path.
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

    /// Borrows the validated relative path.
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Returns the validated path as an owned string.
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

#[cfg(feature = "sqlite")]
impl<'q> sqlx::Encode<'q, sqlx::Sqlite> for FilePath {
    fn encode_by_ref(
        &self,
        arguments: &mut Vec<sqlx::sqlite::SqliteArgumentValue<'q>>,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, sqlx::Sqlite>>::encode_by_ref(&self.0, arguments)
    }
}

#[cfg(feature = "sqlite")]
impl sqlx::Type<sqlx::Sqlite> for FilePath {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <String as sqlx::Type<sqlx::Sqlite>>::type_info()
    }
    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

#[cfg(feature = "sqlite")]
impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for FilePath {
    fn decode(
        value: sqlx::sqlite::SqliteValueRef<'r>,
    ) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<'r, sqlx::Sqlite>>::decode(value)?;
        Self::new(value).map_err(|error| Box::new(error) as sqlx::error::BoxDynError)
    }
}

#[cfg(feature = "postgres")]
impl<'q> sqlx::Encode<'q, sqlx::Postgres> for FilePath {
    fn encode_by_ref(
        &self,
        arguments: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <String as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(&self.0, arguments)
    }
}

#[cfg(feature = "postgres")]
impl sqlx::Type<sqlx::Postgres> for FilePath {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <String as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <String as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

#[cfg(feature = "postgres")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for FilePath {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> std::result::Result<Self, sqlx::error::BoxDynError> {
        let value = <String as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)?;
        Self::new(value).map_err(|error| Box::new(error) as sqlx::error::BoxDynError)
    }
}

/// Storage operations keyed by validated [`FilePath`] values.
pub trait FileStorage {
    fn store(&self, bytes: &[u8], extension: Option<&str>) -> Result<FilePath>;
    fn read(&self, path: &FilePath) -> Result<Vec<u8>>;
    fn delete(&self, path: &FilePath) -> Result<()>;
    fn exists(&self, path: &FilePath) -> Result<bool>;
}

#[derive(Debug, Clone)]
/// Filesystem-backed [`FileStorage`] rooted at one local directory.
pub struct LocalFileStorage {
    root: PathBuf,
}

impl LocalFileStorage {
    /// Creates storage rooted at `root`; the directory is created on first store.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    /// Returns the root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn directory(&self) -> Result<Dir> {
        Dir::open_ambient_dir(&self.root, ambient_authority()).map_err(Into::into)
    }

    fn create_directory(&self) -> Result<Dir> {
        fs::create_dir_all(&self.root)?;
        self.directory()
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
        let root = self.create_directory()?;
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
            let parent = Path::new(path.as_str())
                .parent()
                .expect("file path has a parent");
            root.create_dir_all(parent)?;
            let mut options = cap_std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            match root.open_with(path.as_str(), &options) {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(bytes)?;
                    file.sync_all()?;
                    return Ok(path);
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Storage(
            "unable to allocate a unique file path".to_string(),
        ))
    }

    fn read(&self, path: &FilePath) -> Result<Vec<u8>> {
        Ok(self.directory()?.read(path.as_str())?)
    }
    fn delete(&self, path: &FilePath) -> Result<()> {
        Ok(self.directory()?.remove_file(path.as_str())?)
    }
    fn exists(&self, path: &FilePath) -> Result<bool> {
        let root = match self.directory() {
            Ok(root) => root,
            Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        match root.metadata(path.as_str()) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
