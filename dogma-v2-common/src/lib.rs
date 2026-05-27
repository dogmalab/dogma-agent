//! # dogma-v2-common — Tipos fundacionales, config y protocolo NDJSON
//!
//! Este crate define los tipos compartidos por todos los demás crates del
//! workspace `dogma-agent`. Incluye:
//!
//! * El enum unificado de errores con categorización estricta.
//! * El protocolo de eventos NDJSON para comunicación IPC / SSE.
//! * Tipos auxiliares (severidad, marcas de tiempo).

pub mod error;
pub mod event;

pub use error::{Error, Result};
pub use event::{Event, EventSeverity};
