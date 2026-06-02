//! # SubAgentManager — Pipeline de delegación de subagentes efímeros
//!
//! Orquesta la creación y ejecución de subagentes aislados, cada uno
//! con su propia sesión dentro del SessionManager compartido.
//!
//! ## Pipeline
//!
//! ```text
//! dispatch(args)
//!   │
//!   ├── 1. Validar profundidad (current_depth < max_spawn_depth)
//!   ├── 2. Validar rol (Leaf no puede delegar)
//!   ├── 3. Crear sesión aislada en SessionManager
//!   ├── 4. Inyectar contexto + task_objective como prompt
//!   ├── 5. Ejecutar ciclo RSI del padre con session_id del hijo
//!   └── 6. Capturar resultado y devolverlo al padre
//! ```
//!
//! ## Aislamiento
//!
//! * Cada subagente tiene su propio session_id (no comparte el
//!   contexto del padre).
//! * El `SessionManager` compartido persiste cada subagente como
//!   una sesión independiente dentro de la misma colección.
//! * Los toolsets filtrados limitan qué herramientas puede usar el
//!   subagente (ej: solo `file` y `terminal`).
//! * La profundidad máxima (`max_spawn_depth`) evita recursión.

use std::sync::Arc;

use dogma_v2_common::error::Error;
use dogma_v2_common::Result;
use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

use crate::models::delegation::{AgentGoal, AgentRole, DelegateTaskArgs, SubAgentConfig};
use crate::runtime::loop_handle::RuntimeLoop;
use crate::state::session::SessionManager;
use crate::tools::ToolRegistry;

/// Manager que orquesta la ejecución de subagentes efímeros.
///
/// Cada subagente se modela como un `AgentGoal` que se registra al
/// despachar la tarea y se marca completado al finalizar.
///
/// Se construye con una referencia al `RuntimeLoop` padre. Cada
/// llamada a `dispatch` crea una sesión aislada y ejecuta el ciclo
/// RSI dentro de esa sesión.
pub struct SubAgentManager {
    runtime: Arc<RuntimeLoop>,
    #[allow(dead_code)]
    tools: Arc<RwLock<ToolRegistry>>,
    session: Arc<RwLock<SessionManager>>,
    config: SubAgentConfig,
    /// Registro de todas las metas creadas y su estado actual.
    goals: RwLock<Vec<AgentGoal>>,
}

impl std::fmt::Debug for SubAgentManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubAgentManager")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl SubAgentManager {
    /// Crea un nuevo `SubAgentManager` a partir del `RuntimeLoop` padre.
    ///
    /// # Parámetros
    ///
    /// * `runtime` — RuntimeLoop del agente padre (Arc compartido).
    /// * `config` — Configuración de delegación (profundidad, rol, etc.).
    pub fn new(runtime: Arc<RuntimeLoop>, config: SubAgentConfig) -> Self {
        let tools = runtime.tool_registry();
        let session = runtime.session_handle();
        Self {
            runtime,
            tools,
            session,
            config,
            goals: RwLock::new(Vec::new()),
        }
    }

    // ── Goal lifecycle ──────────────────────────────────────────────

    /// Crea y registra un `AgentGoal` a partir de los argumentos de
    /// delegación. Emite un evento de tracing con el goal creado.
    ///
    /// Devuelve el `id` del goal registrado.
    fn create_goal(&self, args: &DelegateTaskArgs) -> String {
        let goal = AgentGoal::new(args.task_objective.clone(), &args.context);
        let goal_id = goal.id.clone();
        let criteria_count = goal.criteria_count();

        {
            let mut goals = self.goals.write();
            goals.push(goal);
        }

        info!(
            goal_id = &goal_id,
            criteria = criteria_count,
            "AgentGoal created"
        );
        goal_id
    }

    /// Marca un goal como completado por su `id`. Emite un evento de
    /// tracing. No falla si el id no existe (ya fue completado o no se
    /// registró).
    fn complete_goal(&self, goal_id: &str) {
        let mut goals = self.goals.write();
        if let Some(goal) = goals.iter_mut().find(|g| g.id == goal_id) {
            goal.complete();
            info!(
                goal_id = goal_id,
                description = &goal.description,
                "AgentGoal completed"
            );
        } else {
            warn!(goal_id = goal_id, "AgentGoal not found for completion");
        }
    }

    /// Marca un goal como fallido (no completado) emitiendo un evento.
    fn fail_goal(&self, goal_id: &str, reason: &str) {
        let mut goals = self.goals.write();
        if let Some(goal) = goals.iter_mut().find(|g| g.id == goal_id) {
            error!(
                goal_id = goal_id,
                description = &goal.description,
                reason = reason,
                "AgentGoal failed"
            );
        }
    }

    /// Devuelve una copia de todas las metas registradas.
    #[must_use]
    pub fn get_goals(&self) -> Vec<AgentGoal> {
        self.goals.read().clone()
    }

    /// Devuelve el número de metas registradas.
    #[must_use]
    pub fn goal_count(&self) -> usize {
        self.goals.read().len()
    }

    /// Despacha una tarea a un subagente efímero.
    ///
    /// El subagente recibe el `task_objective` como prompt raíz y el
    /// `context` como contexto adicional inyectado. Ejecuta su propio
    /// ciclo RSI (a través del RuntimeLoop padre, pero con un
    /// session_id independiente) y devuelve el resultado como String.
    ///
    /// # Errors
    ///
    /// * `Error::Validation` — Profundidad máxima excedida.
    /// * `Error::Validation` — Rol Leaf intenta delegar.
    /// * `Error::Execution` — Fallo en la ejecución del subagente.
    pub async fn dispatch(&self, args: DelegateTaskArgs) -> Result<String> {
        // ── 1. Validar profundidad ──────────────────────────────────
        if self.config.is_exhausted() {
            warn!(
                "Delegation rejected: max depth {} reached at depth {}",
                self.config.max_spawn_depth, self.config.current_depth
            );
            return Err(Error::Validation(format!(
                "Max spawn depth ({}) reached at current depth {}. \
                 Cannot create more sub-agents.",
                self.config.max_spawn_depth, self.config.current_depth
            )));
        }

        // ── 2. Validar rol ─────────────────────────────────────────
        let child_role = args.role.unwrap_or(AgentRole::Leaf);
        if !self.config.role.can_delegate() && self.config.current_depth > 0 {
            warn!(
                "Delegation rejected: Leaf agent at depth {} cannot delegate",
                self.config.current_depth
            );
            return Err(Error::Validation(format!(
                "Agent with role {:?} at depth {} cannot delegate tasks. \
                 Only Orchestrator agents can spawn sub-agents.",
                self.config.role, self.config.current_depth
            )));
        }

        let child_depth = self.config.current_depth + 1;
        debug!(
            "Dispatching sub-agent (depth={child_depth}, role={child_role:?}, \
             objective_len={}, context_len={})",
            args.task_objective.len(),
            args.context.len(),
        );

        // ── 3. Registrar AgentGoal ──────────────────────────────────
        let goal_id = self.create_goal(&args);
        debug!(goal_id = &goal_id, "AgentGoal registered for sub-agent");

        // ── 4. Crear sesión aislada ────────────────────────────────
        let session_id = format!(
            "sub_{}_depth_{}",
            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("x"),
            child_depth,
        );

        {
            let mut session = self.session.write();
            session
                .create_session(&session_id)
                .map_err(|e| Error::Execution(format!("Failed to create sub-session: {e}")))?;
        }

        info!("Sub-agent session created: {session_id}");

        // ── 4. Construir prompt compuesto ──────────────────────────
        let prompt = if args.context.is_empty() {
            args.task_objective
        } else {
            format!(
                "{}\n\n--- Context ---\n{}",
                args.task_objective, args.context
            )
        };

        // ── 5. Configurar herramientas filtradas ────────────────────
        // Nota: en esta versión inicial, el subagente usa el mismo
        // registro de herramientas que el padre (con delegate_task
        // filtrado para Leaf). Una mejora futura implementará
        // filtrado por toolsets a nivel de RuntimeLoop.
        self.apply_tools_filter(&child_role, &args.toolsets);

        // ── 7. Ejecutar ciclo RSI ──────────────────────────────────
        info!("Running sub-agent: {session_id}");
        match self.runtime.run(&prompt, &session_id).await {
            Ok(result) => {
                debug!("Sub-agent {session_id} completed successfully");
                self.complete_goal(&goal_id);
                self.restore_tools();
                Ok(result)
            }
            Err(e) => {
                error!("Sub-agent {session_id} failed: {e}");
                self.fail_goal(&goal_id, &e.to_string());
                self.restore_tools();
                Err(Error::Execution(format!(
                    "Sub-agent execution failed (session={session_id}): {e}"
                )))
            }
        }
    }

    /// Aplica filtro de herramientas según rol y toolsets.
    ///
    /// En la implementación actual esto es un no-op que registra la
    /// intención. La aplicación real requiere que el `ToolRegistry`
    /// soporte enable/disable por herramienta, lo cual se implementará
    /// en una iteración posterior.
    fn apply_tools_filter(&self, child_role: &AgentRole, toolsets: &Option<Vec<String>>) {
        if *child_role == AgentRole::Leaf {
            debug!("Leaf sub-agent: delegate_task would be filtered out");
        }
        if let Some(ts) = toolsets {
            debug!("Toolsets filter requested: {ts:?} (not yet applied at ToolRegistry level)");
        }
    }

    /// Restaura las herramientas después de la ejecución del subagente.
    fn restore_tools(&self) {
        debug!("Tool filter restored (no-op in current implementation)");
    }

    /// Devuelve una referencia a la configuración actual.
    #[must_use]
    pub fn config(&self) -> &SubAgentConfig {
        &self.config
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::loop_handle::LoopConfig;
    use crate::runtime::provider::LLMProvider;
    use crate::runtime::provider::{LLMResponse, Message};
    use crate::tools::create_survival_tools;
    use tempfile::tempdir;

    /// Provider mock para tests de subagentes.
    struct MockProvider {
        config: crate::runtime::provider::ProviderConfig,
    }

    impl MockProvider {
        fn new() -> Self {
            Self {
                config: crate::runtime::provider::ProviderConfig {
                    model: "mock".into(),
                    base_url: "http://mock".into(),
                    api_key: None,
                    max_tokens: 100,
                    ..crate::runtime::provider::ProviderConfig::default()
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl LLMProvider for MockProvider {
        fn config(&self) -> &crate::runtime::provider::ProviderConfig {
            &self.config
        }

        async fn chat(
            &self,
            _messages: &[Message],
            _tools: &[serde_json::Value],
        ) -> std::result::Result<LLMResponse, dogma_v2_common::error::Error> {
            Ok(LLMResponse {
                content: "mock response from sub-agent".into(),
                tool_calls: vec![],
                usage: crate::runtime::provider::TokenUsage::default(),
                extra_fields: vec![],
            })
        }
    }

    /// Construye un RuntimeLoop mock para tests.
    /// Devuelve también el `TempDir` que debe mantenerse vivo mientras
    /// el runtime esté en uso.
    fn make_mock_runtime() -> (Arc<RuntimeLoop>, tempfile::TempDir) {
        let provider: Arc<dyn LLMProvider> = Arc::new(MockProvider::new());
        let tools = create_survival_tools();
        let config = LoopConfig::default();
        let dir = tempdir().expect("temp dir for session");
        let session = SessionManager::open(dir.path()).expect("test session");

        (
            Arc::new(RuntimeLoop::new(provider, tools, session, config)),
            dir,
        )
    }

    #[tokio::test]
    async fn test_sub_agent_exhausted_depth() {
        let (runtime, _dir) = make_mock_runtime();
        let config = SubAgentConfig {
            current_depth: 2,
            max_spawn_depth: 2,
            ..SubAgentConfig::default()
        };

        let manager = SubAgentManager::new(runtime, config);

        let args = DelegateTaskArgs {
            task_objective: "do something".into(),
            context: String::new(),
            role: None,
            toolsets: None,
        };

        let result = manager.dispatch(args).await;
        assert!(result.is_err(), "Should reject at max depth");
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Max spawn depth"),
            "Error: {err}"
        );
    }

    #[test]
    fn test_sub_agent_config_access() {
        let (runtime, _dir) = make_mock_runtime();
        let config = SubAgentConfig {
            current_depth: 1,
            ..SubAgentConfig::default()
        };

        let manager = SubAgentManager::new(runtime, config);
        assert_eq!(manager.config().current_depth, 1);
        assert_eq!(manager.config().max_spawn_depth, 2);
    }

    // ── Goal lifecycle tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_sub_agent_goal_created_on_dispatch() {
        let (runtime, _dir) = make_mock_runtime();
        let manager = SubAgentManager::new(runtime, SubAgentConfig::default());

        let args = DelegateTaskArgs {
            task_objective: "Refactor the parser module".into(),
            context: "- [ ] Run tests\n- [ ] Update docs".into(),
            role: None,
            toolsets: None,
        };

        let result = manager.dispatch(args).await;
        assert!(result.is_ok(), "dispatch should succeed: {:?}", result);

        // Goal should have been created and completed
        assert_eq!(manager.goal_count(), 1);
        let goals = manager.get_goals();
        assert!(goals[0].is_completed(), "goal should be completed");
        assert_eq!(goals[0].description, "Refactor the parser module");
        assert_eq!(goals[0].criteria_count(), 2);
    }

    #[tokio::test]
    async fn test_sub_agent_goal_no_goals_on_rejected() {
        let (runtime, _dir) = make_mock_runtime();
        let config = SubAgentConfig {
            current_depth: 2,
            max_spawn_depth: 2,
            ..SubAgentConfig::default()
        };

        let manager = SubAgentManager::new(runtime, config);

        let args = DelegateTaskArgs {
            task_objective: "do something".into(),
            context: String::new(),
            role: None,
            toolsets: None,
        };

        let result = manager.dispatch(args).await;
        assert!(result.is_err(), "Should reject at max depth");

        // No goal should have been created since validation failed early
        assert_eq!(manager.goal_count(), 0);
    }

    #[tokio::test]
    async fn test_sub_agent_multiple_goals_accumulated() {
        let (runtime, _dir) = make_mock_runtime();
        let manager = SubAgentManager::new(runtime, SubAgentConfig::default());

        for i in 0..3 {
            let args = DelegateTaskArgs {
                task_objective: format!("Task {i}"),
                context: String::new(),
                role: None,
                toolsets: None,
            };
            let _ = manager.dispatch(args).await;
        }

        assert_eq!(manager.goal_count(), 3);
        let goals = manager.get_goals();
        for (i, goal) in goals.iter().enumerate() {
            assert!(goal.is_completed(), "Goal {i} should be completed");
            assert_eq!(goal.description, format!("Task {i}"));
        }
    }
}
