//! # AgentEvent — Eventos de telemetría para consumidores externos
//!
//! Define el contrato de eventos que el runtime emite a través de un
//! canal `tokio::sync::mpsc`. La UI en línea consume estos eventos
//! para actualizar sus líneas dinámicas en la terminal sin modificar
//! el historial de chat.
//!
//! ## Consumidores
//!
//! * `InlineUI` (CLI) — renderiza barras de progreso, spinners y
//!   estado cognitivo en la terminal.
//! * Modo JSON — serializa eventos como NDJSON para integración con
//!   herramientas externas.

use serde::{Deserialize, Serialize};

/// Evento de telemetría para el ciclo de vida de agentes y subagentes.
///
/// Estos eventos fluyen desde el `SubAgentManager` y el `RuntimeLoop`
/// hacia la UI u otros consumidores a través de un canal async.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentEvent {
    /// Un subagente ha sido spawnado con una meta específica.
    SubAgentSpawned {
        goal_id: String,
        description: String,
        depth: usize,
    },
    /// El subagente cambió de etapa en su ciclo RSI.
    StageChanged {
        goal_id: String,
        stage: String,
        fuel_pct: Option<f64>,
    },
    /// El estado de una meta fue evaluado (completada o fallida).
    GoalEvaluated {
        goal_id: String,
        description: String,
        completed: bool,
        criteria_count: usize,
    },
    /// Una herramienta fue ejecutada por el subagente.
    ToolExecuted {
        goal_id: String,
        tool_name: String,
        duration_ms: u64,
    },
    /// El subagente terminó su ejecución.
    SubAgentTerminated {
        goal_id: String,
        success: bool,
        result_summary: String,
    },
    /// Actualización periódica del estado cognitivo del agente.
    StatusUpdate {
        /// Porcentaje de ventana de contexto usado (0.0 - 1.0).
        context_used: f32,
        /// Total de tokens consumidos en la sesión.
        total_tokens: u64,
        /// Modelo activo.
        model: String,
    },
    /// Chunk de reasoning/thinking del LLM (DeepSeek `reasoning_content`).
    ThinkingChunk { content: String },
    /// Chunk del contenido de la respuesta final del LLM.
    ContentChunk { content: String },
}

impl AgentEvent {
    /// Crea un evento de spawn para un subagente.
    #[must_use]
    pub fn spawned(goal_id: String, description: String, depth: usize) -> Self {
        Self::SubAgentSpawned {
            goal_id,
            description,
            depth,
        }
    }

    /// Crea un evento de cambio de etapa.
    #[must_use]
    pub fn stage_changed(goal_id: String, stage: String, fuel_pct: Option<f64>) -> Self {
        Self::StageChanged {
            goal_id,
            stage,
            fuel_pct,
        }
    }

    /// Crea un evento de evaluación de meta.
    #[must_use]
    pub fn goal_evaluated(
        goal_id: String,
        description: String,
        completed: bool,
        criteria_count: usize,
    ) -> Self {
        Self::GoalEvaluated {
            goal_id,
            description,
            completed,
            criteria_count,
        }
    }

    /// Crea un evento de herramienta ejecutada.
    #[must_use]
    pub fn tool_executed(goal_id: String, tool_name: String, duration_ms: u64) -> Self {
        Self::ToolExecuted {
            goal_id,
            tool_name,
            duration_ms,
        }
    }

    /// Crea un evento de terminación de subagente.
    #[must_use]
    pub fn terminated(goal_id: String, success: bool, result_summary: String) -> Self {
        Self::SubAgentTerminated {
            goal_id,
            success,
            result_summary,
        }
    }

    /// Crea un evento de actualización de estado.
    #[must_use]
    pub fn status(context_used: f32, total_tokens: u64, model: String) -> Self {
        Self::StatusUpdate {
            context_used,
            total_tokens,
            model,
        }
    }

    /// Crea un evento de chunk de thinking/reasoning.
    #[must_use]
    pub fn thinking_chunk(content: String) -> Self {
        Self::ThinkingChunk { content }
    }

    /// Crea un evento de chunk de contenido.
    #[must_use]
    pub fn content_chunk(content: String) -> Self {
        Self::ContentChunk { content }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_event_spawned() {
        let e = AgentEvent::spawned("goal_1".into(), "test task".into(), 1);
        assert!(matches!(e, AgentEvent::SubAgentSpawned { .. }));
        if let AgentEvent::SubAgentSpawned {
            ref goal_id,
            ref description,
            depth,
        } = e
        {
            assert_eq!(goal_id, "goal_1");
            assert_eq!(description, "test task");
            assert_eq!(depth, 1);
        }
    }

    #[test]
    fn test_agent_event_goal_evaluated() {
        let e = AgentEvent::goal_evaluated("goal_x".into(), "fix bug".into(), true, 3);
        assert!(matches!(
            e,
            AgentEvent::GoalEvaluated {
                completed: true,
                ..
            }
        ));
    }

    #[test]
    fn test_agent_event_serialization_roundtrip() {
        let e = AgentEvent::status(0.5, 12000, "gpt-4".into());
        let json = serde_json::to_value(&e).expect("serialize");
        // Verificar estructura del enum externamente tagged
        assert!(json.get("StatusUpdate").is_some(), "debe ser StatusUpdate");
        assert_eq!(json["StatusUpdate"]["total_tokens"], 12000);

        let deserialized: AgentEvent = serde_json::from_value(json).expect("deserialize");
        assert_eq!(e, deserialized);
    }

    #[test]
    fn test_agent_event_all_variants_serialize() {
        let events = vec![
            AgentEvent::spawned("g1".into(), "task1".into(), 0),
            AgentEvent::stage_changed("g1".into(), "planning".into(), Some(0.85)),
            AgentEvent::goal_evaluated("g1".into(), "task1".into(), true, 2),
            AgentEvent::tool_executed("g1".into(), "read_file".into(), 42),
            AgentEvent::terminated("g1".into(), true, "done".into()),
            AgentEvent::status(0.3, 5000, "claude".into()),
        ];
        for e in &events {
            let json = serde_json::to_value(e).expect("serialize");
            let back: AgentEvent = serde_json::from_value(json).expect("deserialize");
            assert_eq!(*e, back, "roundtrip failed for {e:?}");
        }
    }
}
