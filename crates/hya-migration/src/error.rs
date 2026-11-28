//! Error types for `hya-migration`.

use std::path::PathBuf;

/// Failures produced during inspection, migration, backup, or rollback.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// Filesystem I/O failure with the associated path and context.
    #[error("I/O error at `{path}` ({context}): {source}")]
    Io {
        /// Affected filesystem path.
        path: PathBuf,
        /// Short description of the attempted operation.
        context: &'static str,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// SQLite / `sqlx` database failure.
    #[error("SQLite error ({context}): {source}")]
    Sqlx {
        /// Short description of the attempted database operation.
        context: String,
        /// Underlying `sqlx` error.
        #[source]
        source: sqlx::Error,
    },
    /// Error returned by v2 `hya-store` while initializing or verifying a database.
    #[error("hya-store error ({context}): {source}")]
    Store {
        /// Short description of the attempted store operation.
        context: String,
        /// Underlying `hya_store::StoreError`.
        #[source]
        source: hya_store::StoreError,
    },
    /// JSON parsing or serialization error.
    #[error("JSON error ({context}): {source}")]
    Json {
        /// Short description of the JSON operation.
        context: String,
        /// Underlying `serde_json` error.
        #[source]
        source: serde_json::Error,
    },
    /// YAML parsing or serialization error.
    #[error("YAML error ({context}): {source}")]
    Yaml {
        /// Short description of the YAML operation.
        context: String,
        /// Underlying `serde_norway` error.
        #[source]
        source: serde_norway::Error,
    },
    /// TOML parsing error.
    #[error("TOML parse error at `{path}`: {source}")]
    TomlDe {
        /// Path of the TOML file.
        path: PathBuf,
        /// Underlying `toml::de::Error`.
        #[source]
        source: toml::de::Error,
    },
    /// TOML serialization error.
    #[error("TOML serialize error at `{path}`: {source}")]
    TomlSer {
        /// Path of the TOML file.
        path: PathBuf,
        /// Underlying `toml::ser::Error`.
        #[source]
        source: toml::ser::Error,
    },
    /// Backup manifest is missing, malformed, or fails SHA-256 verification.
    #[error("backup verification failed: {0}")]
    BackupVerification(String),
    /// Database integrity check or schema validation failed.
    #[error("database integrity check failed for `{path}`: {detail}")]
    DatabaseIntegrity {
        /// Database file path.
        path: PathBuf,
        /// Detailed integrity error.
        detail: String,
    },
    /// Unsupported or malformed v1 data encountered during migration.
    #[error("invalid v1 artifact `{path}`: {detail}")]
    InvalidArtifact {
        /// Artifact path.
        path: PathBuf,
        /// Human-readable reason.
        detail: String,
    },
}

/// Convenience result alias for `hya-migration`.
pub type Result<T> = std::result::Result<T, MigrationError>;

impl MigrationError {
    /// Construct an [`MigrationError::Io`] error.
    #[must_use]
    pub fn io(path: impl Into<PathBuf>, context: &'static str, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            context,
            source,
        }
    }
}
