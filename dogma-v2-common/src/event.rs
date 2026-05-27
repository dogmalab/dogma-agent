//! # Protocolo de eventos NDJSON
//!
//! Cada evento se serializa como una línea JSON separada por `\n`,
//! permitiendo su consumo tanto por la CLI (flag `--json`) como por
//! Server-Sent Events (SSE) en la futura interfaz web.
//!
//! ## Formato
//!
//! ```json
//! {"type":"message","timestamp":"2026-05-25T20:00:00Z","severity":"info","content":"...","session_id":"..."}
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Nivel de severidad de un evento NDJSON.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSeverity {
    /// Información general (progreso, diagnóstico).
    Info,
    /// Advertencia (recuperable, no crítica).
    Warning,
    /// Error fatal (el runtime abortará).
    Fatal,
    /// Traza de depuración (solo cuando `RUST_LOG=debug`).
    Debug,
    /// Éxito de una operación.
    Success,
}

impl EventSeverity {
    /// Devuelve `true` si la severidad es igual o superior a `warning`.
    #[must_use]
    pub fn is_at_least_warning(&self) -> bool {
        matches!(self, Self::Warning | Self::Fatal)
    }
}

impl std::fmt::Display for EventSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "info"),
            Self::Warning => write!(f, "warning"),
            Self::Fatal => write!(f, "fatal"),
            Self::Debug => write!(f, "debug"),
            Self::Success => write!(f, "success"),
        }
    }
}

/// Tipo de evento dentro del flujo NDJSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Mensaje del asistente (respuesta de la IA).
    Message,
    /// Ejecución de una herramienta.
    ToolCall,
    /// Resultado de una herramienta.
    ToolResult,
    /// Progreso de planificación.
    PlanProgress,
    /// Error ocurrido durante la ejecución.
    Error,
    /// Señal de finalización.
    Done,
    /// Estado interno del runtime.
    System,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message => write!(f, "message"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::ToolResult => write!(f, "tool_result"),
            Self::PlanProgress => write!(f, "plan_progress"),
            Self::Error => write!(f, "error"),
            Self::Done => write!(f, "done"),
            Self::System => write!(f, "system"),
        }
    }
}

/// Un evento del protocolo NDJSON.
///
/// Cada evento representa una unidad atómica de información que viaja
/// desde el runtime hacia el exterior (CLI, UI, logs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Tipo semántico del evento.
    #[serde(rename = "type")]
    pub event_type: EventType,
    /// Marca de tiempo ISO 8601 (UTC).
    pub timestamp: DateTime<Utc>,
    /// Severidad del evento.
    pub severity: EventSeverity,
    /// Contenido textual del evento.
    pub content: String,
    /// ID de sesión asociada (opcional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Metadatos adicionales (tool name, exit code, etc.).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub metadata: std::collections::HashMap<String, String>,
}

impl Event {
    /// Crea un nuevo evento con la hora actual.
    pub fn new(
        event_type: EventType,
        severity: EventSeverity,
        content: impl Into<String>,
    ) -> Self {
        Self {
            event_type,
            timestamp: Utc::now(),
            severity,
            content: content.into(),
            session_id: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// Asigna un ID de sesión al evento.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Añade una entrada de metadato al evento.
    #[must_use]
    pub fn with_metadata(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Serializa el evento a una línea NDJSON (sin `\n` final).
    ///
    /// Devuelve `String` vacía si la serialización falla (no debería
    /// ocurrir con tipos simples como estos).
    pub fn to_ndjson(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Serializa el evento y añade un `\n` final para escribir en un
    /// stream.
    pub fn to_ndjson_line(&self) -> String {
        let mut line = self.to_ndjson();
        line.push('\n');
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_roundtrip() {
        let event = Event::new(EventType::Message, EventSeverity::Info, "hello world")
            .with_session_id("session-1");
        let json = event.to_ndjson();
        let deserialized: Event = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(deserialized.event_type, EventType::Message);
        assert_eq!(deserialized.content, "hello world");
        assert_eq!(deserialized.session_id, Some("session-1".into()));
    }

    #[test]
    fn test_severity_display() {
        assert_eq!(EventSeverity::Info.to_string(), "info");
        assert_eq!(EventSeverity::Fatal.to_string(), "fatal");
    }

    #[test]
    fn test_error_categorization() {
        let infra = crate::error::Error::Network {
            detail: "timeout".into(),
            source: None,
        };
        assert!(infra.is_infrastructure());
        assert!(!infra.is_execution());
        assert!(!infra.is_fatal());
    }
}
