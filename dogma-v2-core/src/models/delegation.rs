//! # Delegation — Tipos para el sistema de subagentes efímeros
//!
//! Define las estructuras de datos que gobiernan la jerarquía de
//! delegación, los roles de los subagentes, y los argumentos que el
//! agente padre pasa al invocar `delegate_task`.
//!
//! ## Arquitectura
//!
//! ```text
//! Agent (padre)
//!   │  delegate_task({task_objective, context, role?, toolsets?})
//!   │
//!   └── SubAgentManager
//!         ├── Verifica profundidad (current_depth < max_spawn_depth)
//!         ├── Crea contexto aislado (sesión + tools filtrados)
//!         ├── Ejecuta ciclo RSI con su propio RuntimeLoop
//!         └── Devuelve resultado al padre
//! ```
//!
//! ## Seguridad
//!
//! * `max_spawn_depth` evita recursión infinita de subagentes (default: 2).
//! * `AgentRole::Leaf` impide que un subagente delegue a su vez.
//! * `toolsets` permite restringir qué herramientas usa cada subagente.

use serde::{Deserialize, Serialize};

/// Rol de un subagente dentro de la jerarquía de delegación.
///
/// Controla si el subagente puede o no crear sus propios subagentes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Nodo hoja. No puede invocar `delegate_task` bajo ninguna
    /// circunstancia. Es el rol por defecto.
    #[default]
    Leaf,
    /// Orquestador. Puede invocar `delegate_task` para subdividir y
    /// encadenar trabajo jerárquicamente.
    Orchestrator,
}

impl AgentRole {
    /// Devuelve `true` si el rol permite delegar.
    #[must_use]
    pub fn can_delegate(&self) -> bool {
        matches!(self, Self::Orchestrator)
    }
}

// ---------------------------------------------------------------------------
// SubAgentConfig — Configuración de delegación
// ---------------------------------------------------------------------------

/// Configuración global del sistema de delegación.
///
/// Se inyecta en cada `SubAgentManager` y se hereda (con profundidad
/// incrementada) a los subagentes hijos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentConfig {
    /// Rol del subagente actual.
    #[serde(default)]
    pub role: AgentRole,
    /// Filtro de grupos de herramientas autorizados.
    /// `None` = todas las herramientas disponibles.
    /// `Some(["file", "terminal"])` = solo las herramientas de esos
    /// grupos/tipos.
    pub toolsets: Option<Vec<String>>,
    /// Profundidad de delegación actual. Empieza en 0 para el agente
    /// raíz y se incrementa con cada nivel de delegación.
    #[serde(default)]
    pub current_depth: usize,
    /// Límite estricto para evitar recursión infinita (default: 2).
    /// Un valor de 2 permite: raíz → subagente → subagente.
    #[serde(default = "default_max_spawn_depth")]
    pub max_spawn_depth: usize,
    /// Máximo de iteraciones de tool calls por subagente (default: 5).
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
}

const fn default_max_spawn_depth() -> usize {
    2
}

const fn default_max_iterations() -> u32 {
    5
}

impl Default for SubAgentConfig {
    fn default() -> Self {
        Self {
            role: AgentRole::default(),
            toolsets: None,
            current_depth: 0,
            max_spawn_depth: default_max_spawn_depth(),
            max_iterations: default_max_iterations(),
        }
    }
}

impl SubAgentConfig {
    /// Crea una configuración para un subagente hijo, incrementando la
    /// profundidad y heredando el límite del padre.
    ///
    /// El hijo hereda `max_spawn_depth` y `max_iterations` del padre,
    /// pero su `current_depth` es `parent.current_depth + 1`.
    #[must_use]
    pub fn child_config(&self, role: AgentRole, toolsets: Option<Vec<String>>) -> Self {
        Self {
            role,
            toolsets,
            current_depth: self.current_depth + 1,
            max_spawn_depth: self.max_spawn_depth,
            max_iterations: self.max_iterations,
        }
    }

    /// Devuelve `true` si la profundidad actual permite seguir
    /// delegando.
    #[must_use]
    pub fn can_spawn(&self) -> bool {
        self.current_depth < self.max_spawn_depth
    }

    /// Devuelve `true` si la profundidad actual ha excedido el límite.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.current_depth >= self.max_spawn_depth
    }
}

/// Estructura de argumentos que el LLM del agente padre pasa al
/// invocar la herramienta `delegate_task`.
///
/// Corresponde al esquema JSON de la tool declaration que el LLM
/// recibe en el prompt del sistema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateTaskArgs {
    /// Objetivo principal del subagente. Es el prompt que se le pasa
    /// como instrucción raíz.
    pub task_objective: String,
    /// Contexto adicional: checklists, especificaciones, fragmentos de
    /// skills, o cualquier información que el subagente necesite.
    pub context: String,
    /// Rol opcional del subagente. Si se omite, se usa `Leaf` por
    /// defecto.
    #[serde(default)]
    pub role: Option<AgentRole>,
    /// Filtro opcional de toolsets. Si se omite, hereda los del padre.
    pub toolsets: Option<Vec<String>>,
    /// Skills opcionales para instalar en el contexto del subagente.
    /// Cada string es un skill_id válido en skills.sh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// AgentGoal — Meta estructurada para subagentes
// ---------------------------------------------------------------------------

/// Meta estructurada que un subagente debe cumplir.
///
/// Cada tarea delegada se modela como un `AgentGoal`. El subagente usa
/// esta meta como ancla determinista para medir progreso y autoevaluar
/// el éxito de su síntesis antes de cerrar su bucle RSI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentGoal {
    /// Identificador único para rastreo asíncrono (ej: "goal_<uuid_prefix>").
    pub id: String,
    /// Descripción de lo que el subagente debe lograr.
    /// Proviene del `task_objective` del `DelegateTaskArgs`.
    pub description: String,
    /// `true` cuando el subagente ha autoverificado el cumplimiento.
    #[serde(default)]
    pub completed: bool,
    /// Criterios de aceptación o restricciones adicionales inyectadas
    /// por el orquestador (vacío si no se especificaron).
    #[serde(default)]
    pub criteria: Vec<String>,
}

impl AgentGoal {
    /// Crea una nueva meta sin completar a partir de un objetivo.
    ///
    /// El `id` se genera automáticamente con un prefijo UUID corto.
    /// `criteria` se parsea de `context` si está presente.
    #[must_use]
    pub fn new(description: String, context: &str) -> Self {
        let id = format!(
            "goal_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("x")
        );

        // Extraer criterios del context: líneas que empiezan con "- [ ]"
        // o que contienen "CRITERIO:" o "ACCEPTANCE:".
        let criteria: Vec<String> = context
            .lines()
            .filter(|line| {
                let lower = line.trim().to_lowercase();
                lower.starts_with("- [ ]")
                    || lower.starts_with("* [ ]")
                    || lower.contains("criterio:")
                    || lower.contains("acceptance:")
            })
            .map(|line| line.trim().to_string())
            .collect();

        Self {
            id,
            description,
            completed: false,
            criteria,
        }
    }

    /// Marca la meta como completada.
    pub fn complete(&mut self) {
        self.completed = true;
    }

    /// Devuelve `true` si la meta está cumplida.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        self.completed
    }

    /// Devuelve el número de criterios de aceptación.
    #[must_use]
    pub fn criteria_count(&self) -> usize {
        self.criteria.len()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_role_default() {
        assert_eq!(AgentRole::default(), AgentRole::Leaf);
    }

    #[test]
    fn test_agent_role_can_delegate() {
        assert!(!AgentRole::Leaf.can_delegate());
        assert!(AgentRole::Orchestrator.can_delegate());
    }

    #[test]
    fn test_sub_agent_config_default() {
        let cfg = SubAgentConfig::default();
        assert_eq!(cfg.role, AgentRole::Leaf);
        assert_eq!(cfg.current_depth, 0);
        assert_eq!(cfg.max_spawn_depth, 2);
        assert_eq!(cfg.max_iterations, 5);
        assert!(cfg.toolsets.is_none());
    }

    #[test]
    fn test_sub_agent_config_can_spawn() {
        let cfg = SubAgentConfig::default();
        assert!(cfg.can_spawn());
        assert!(!cfg.is_exhausted());
    }

    #[test]
    fn test_sub_agent_config_exhausted() {
        let cfg = SubAgentConfig {
            current_depth: 2,
            ..SubAgentConfig::default()
        };
        assert!(!cfg.can_spawn());
        assert!(cfg.is_exhausted());
    }

    #[test]
    fn test_sub_agent_config_exhausted_at_max() {
        // max_spawn_depth = 2 significa que depth 0 y 1 pueden spawnear,
        // depth 2 ya no puede.
        let cfg = SubAgentConfig {
            current_depth: 2,
            max_spawn_depth: 2,
            ..SubAgentConfig::default()
        };
        assert!(!cfg.can_spawn());
        assert!(cfg.is_exhausted());
    }

    #[test]
    fn test_sub_agent_config_child() {
        let parent = SubAgentConfig::default();
        let child = parent.child_config(AgentRole::Leaf, Some(vec!["file".into()]));
        assert_eq!(child.current_depth, 1);
        assert_eq!(child.max_spawn_depth, 2);
        assert_eq!(child.role, AgentRole::Leaf);
        assert_eq!(child.toolsets, Some(vec!["file".to_string()]));
    }

    #[test]
    fn test_delegate_task_args_serialization() {
        let args = DelegateTaskArgs {
            task_objective: "Refactor module X".into(),
            context: "Checklist:\n1. Run tests\n2. Update docs".into(),
            role: Some(AgentRole::Orchestrator),
            toolsets: Some(vec!["file".into(), "terminal".into()]),
            skills: None,
        };

        let json = serde_json::to_value(&args).expect("serialize");
        assert_eq!(json["task_objective"], "Refactor module X");
        assert_eq!(json["role"], "orchestrator");

        // round-trip
        let deserialized: DelegateTaskArgs = serde_json::from_value(json).expect("deserialize");
        assert_eq!(deserialized.task_objective, args.task_objective);
        assert_eq!(deserialized.role, args.role);
        assert_eq!(deserialized.toolsets, args.toolsets);
    }

    #[test]
    fn test_delegate_task_args_default_role() {
        let args = DelegateTaskArgs {
            task_objective: "test".into(),
            context: String::new(),
            role: None,
            toolsets: None,
            skills: None,
        };
        assert!(args.role.is_none());
    }

    // ── AgentGoal tests ─────────────────────────────────────────────

    #[test]
    fn test_agent_goal_creation() {
        let goal = AgentGoal::new("Refactor module X".into(), "");
        assert!(!goal.is_completed());
        assert_eq!(goal.description, "Refactor module X");
        assert!(goal.id.starts_with("goal_"));
        assert!(goal.criteria.is_empty());
    }

    #[test]
    fn test_agent_goal_with_criteria_from_context() {
        let context = "\
- [ ] All tests pass
- [ ] No warnings clippy
* [ ] Documentation updated
Some other text
ACCEPTANCE: zero regressions";
        let goal = AgentGoal::new("Refactor module X".into(), context);
        assert_eq!(goal.criteria_count(), 4);
        assert!(goal.criteria[0].contains("- [ ]"));
    }

    #[test]
    fn test_agent_goal_complete() {
        let mut goal = AgentGoal::new("test".into(), "");
        assert!(!goal.is_completed());
        goal.complete();
        assert!(goal.is_completed());
    }

    #[test]
    fn test_agent_goal_serialization_roundtrip() {
        let mut goal = AgentGoal::new("Build feature Y".into(), "criterio: performance");
        goal.complete();

        let json = serde_json::to_value(&goal).expect("serialize");
        assert_eq!(json["description"], "Build feature Y");
        assert_eq!(json["completed"], true);
        assert!(json["criteria"][0].as_str().unwrap().contains("criterio:"));

        let deserialized: AgentGoal = serde_json::from_value(json).expect("deserialize");
        assert_eq!(deserialized.description, goal.description);
        assert!(deserialized.is_completed());
        assert_eq!(deserialized.criteria_count(), 1);
    }
}
