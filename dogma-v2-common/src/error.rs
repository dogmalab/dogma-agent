//! # Sistema unificado de errores
//!
//! Clasifica cada fallo en una de tres categorías estrictas para que el
//! loop de RSI pueda tomar decisiones precisas:
//!
//! * **Infrastructure** — Errores de red, caídas de API, rate limits.
//!   Son recuperables; el runtime reintenta con backoff.
//! * **Execution** — Errores lógicos controlados (tool no encontrada,
//!   formato inválido, etc.). El loop puede auto-curarlos.
//! * **Fatal** — Fallos críticos de I/O de disco o corrupción de estado
//!   que deben abortar el runtime inmediatamente.

use std::path::PathBuf;
use thiserror::Error;

/// Error unificado y categorizado del sistema Dogma 2.0.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    // ── Infrastructure ──────────────────────────────────────────────

    #[error("network error: {detail}")]
    Network {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    #[error("rate limit exceeded (retry after {retry_after_secs}s)")]
    RateLimited { retry_after_secs: u64 },

    #[error("API error ({status_code}): {detail}")]
    Api {
        status_code: u16,
        detail: String,
    },

    // ── Execution ───────────────────────────────────────────────────

    #[error("execution error: {0}")]
    Execution(String),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    // ── Fatal ───────────────────────────────────────────────────────

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("storage corrupted: {0}")]
    StorageCorrupted(String),

    #[error("internal invariant violated: {0}")]
    Internal(String),
}

/// Alias de resultado con el error unificado de dogma-v2-common.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Devuelve `true` si el error es de categoría **Infrastructure**.
    #[must_use]
    pub fn is_infrastructure(&self) -> bool {
        matches!(self, Self::Network { .. } | Self::RateLimited { .. } | Self::Api { .. })
    }

    /// Devuelve `true` si el error es de categoría **Execution**.
    #[must_use]
    pub fn is_execution(&self) -> bool {
        matches!(self, Self::Execution(_) | Self::ToolNotFound(_) | Self::Validation(_) | Self::Serialization(_))
    }

    /// Devuelve `true` si el error es de categoría **Fatal**.
    #[must_use]
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Io { .. } | Self::StorageCorrupted(_) | Self::Internal(_))
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            path: PathBuf::new(),
            source,
        }
    }
}
