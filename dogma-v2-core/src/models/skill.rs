//! # Skill — Modelos de habilidades dinámicas descargables desde skills.sh
//!
//! Define las estructuras de datos para representar una habilidad dinámica
//! que el agente puede adquirir e indexar bajo demanda.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Identificador único de un Skill en la base de datos de grafos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SkillId(pub String);

impl std::fmt::Display for SkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for SkillId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for SkillId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Representa una habilidad dinámica que el agente puede adquirir e
/// indexar bajo demanda.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicSkill {
    pub id: SkillId,
    pub name: String,
    pub description: String,
    /// Ejemplos de uso que disparan este skill de forma semántica en
    /// dogma-vdb.
    #[serde(default)]
    pub trigger_examples: Vec<String>,
    /// Parámetros en formato JSON Schema que el LLM debe proveer para
    /// ejecutar el skill.
    #[serde(default = "default_input_schema")]
    pub input_schema: Value,
    /// El payload real que contiene el código o la directiva del sistema.
    pub payload: SkillPayload,
}

fn default_input_schema() -> Value {
    serde_json::json!({})
}

/// Payload de un skill: código ejecutable o extensión del system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SkillPayload {
    /// Un script ejecutable (ej: Python o Bash) que correrá de forma
    /// confinada.
    ExecutableScript { interpreter: String, code: String },
    /// Parches o extensiones del system prompt para alterar el
    /// comportamiento cognitivo.
    SystemInstructionExtension { system_prompt_patch: String },
}

impl DynamicSkill {
    /// Crea un nuevo DynamicSkill con los campos mínimos requeridos.
    #[must_use]
    pub fn new(
        id: impl Into<SkillId>,
        name: impl Into<String>,
        description: impl Into<String>,
        payload: SkillPayload,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            trigger_examples: Vec::new(),
            input_schema: default_input_schema(),
            payload,
        }
    }

    /// Añade ejemplos de disparo semántico.
    #[must_use]
    pub fn with_triggers(mut self, triggers: Vec<impl Into<String>>) -> Self {
        self.trigger_examples = triggers.into_iter().map(|t| t.into()).collect();
        self
    }

    /// Añade el esquema de entrada.
    #[must_use]
    pub fn with_input_schema(mut self, schema: Value) -> Self {
        self.input_schema = schema;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_creation() {
        let skill = DynamicSkill::new(
            "format-json",
            "Format JSON",
            "Formatea bloques JSON de forma estética",
            SkillPayload::ExecutableScript {
                interpreter: "python3".to_string(),
                code: "print('JSON limpio')".to_string(),
            },
        );
        assert_eq!(skill.id.to_string(), "format-json");
        assert_eq!(skill.name, "Format JSON");
        assert!(skill.trigger_examples.is_empty());
    }

    #[test]
    fn test_skill_with_triggers() {
        let skill = DynamicSkill::new(
            "search-code",
            "Search Code",
            "Busca código en el repositorio",
            SkillPayload::SystemInstructionExtension {
                system_prompt_patch: "Search files with ripgrep.".to_string(),
            },
        )
        .with_triggers(vec!["buscar", "search"]);
        assert_eq!(skill.trigger_examples.len(), 2);
    }

    #[test]
    fn test_skill_id_from_str() {
        let id: SkillId = "my-skill".into();
        assert_eq!(id.0, "my-skill");
    }

    #[test]
    fn test_skill_serialization_roundtrip() {
        let skill = DynamicSkill::new(
            "test-skill",
            "Test",
            "A test skill",
            SkillPayload::ExecutableScript {
                interpreter: "bash".to_string(),
                code: "echo hello".to_string(),
            },
        );
        let json = serde_json::to_string(&skill).expect("serialize");
        let deserialized: DynamicSkill = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(skill.id, deserialized.id);
        assert_eq!(skill.name, deserialized.name);
    }
}
