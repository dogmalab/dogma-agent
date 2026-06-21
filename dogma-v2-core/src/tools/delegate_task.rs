//! # DelegateTaskTool — Herramienta de delegación a subagentes efímeros
//!
//! Expone `delegate_task` como una herramienta que el LLM del agente
//! padre puede invocar para delegar trabajo a un subagente aislado.
//!
//! ## Flujo
//!
//! ```text
//! LLM llama: delegate_task({task_objective, context, role?, toolsets?})
//!   │
//!   ├── 1. Deserializar DelegateTaskArgs
//!   ├── 2. Validar argumentos (non-empty task_objective)
//!   ├── 3. SubAgentManager::dispatch(args)
//!   └── 4. Devolver resultado o error al LLM
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use crate::models::delegation::DelegateTaskArgs;
use crate::runtime::sub_agent::SubAgentManager;
use crate::tools::{Tool, ToolResult};

/// Herramienta que permite al LLM delegar tareas a subagentes
/// efímeros con contexto aislado.
///
/// El subagente recibe un objetivo (`task_objective`) y contexto
/// adicional (`context`), y ejecuta su propio ciclo RSI en una
/// sesión independiente.
pub struct DelegateTaskTool {
    manager: Arc<SubAgentManager>,
}

impl std::fmt::Debug for DelegateTaskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegateTaskTool").finish_non_exhaustive()
    }
}

impl DelegateTaskTool {
    /// Crea una nueva herramienta de delegación.
    ///
    /// # Parámetros
    ///
    /// * `manager` — SubAgentManager del agente raíz (Arc compartido).
    #[must_use]
    pub fn new(manager: Arc<SubAgentManager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &'static str {
        "delegate_task"
    }

    fn description(&self) -> &'static str {
        "Delegates a task to a sub-agent with isolated context. \
         The sub-agent runs its own reasoning loop with the given \
         objective and context, using restricted tools. \
         Use this for complex subproblems that benefit from \
         focused, independent execution."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["task_objective", "context"],
            "properties": {
                "task_objective": {
                    "type": "string",
                    "description": "The main objective for the sub-agent. \
                                    This is the root instruction/prompt."
                },
                "context": {
                    "type": "string",
                    "description": "Additional context: checklists, \
                                    specifications, skill fragments, or \
                                    any information the sub-agent needs."
                },
                "role": {
                    "type": "string",
                    "enum": ["leaf", "orchestrator"],
                    "description": "Role of the sub-agent. 'leaf' (default) \
                                    cannot delegate further. 'orchestrator' \
                                    can spawn its own sub-agents."
                },
                "toolsets": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Optional filter of tool groups. \
                                    Example: ['file', 'terminal']"
                },
                "skills": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Optional skills to install in the sub-agent's \
                                    context. Each string is a skill_id from skills.sh. \
                                    Example: ['format-json', 'search-code']"
                }
            }
        })
    }

    async fn call(&self, args: &serde_json::Value) -> ToolResult {
        // ── 1. Deserializar ────────────────────────────────────────
        let delegate_args: DelegateTaskArgs = serde_json::from_value(args.clone())
            .map_err(|e| format!("Invalid delegate_task arguments: {e}"))?;

        // ── 2. Validar ─────────────────────────────────────────────
        if delegate_args.task_objective.trim().is_empty() {
            return Err("task_objective must not be empty".to_string());
        }

        debug!(
            "delegate_task called: objective_len={}, context_len={}, role={:?}, toolsets={:?}",
            delegate_args.task_objective.len(),
            delegate_args.context.len(),
            delegate_args.role,
            delegate_args.toolsets,
        );

        // ── 3. Despachar ───────────────────────────────────────────
        match self.manager.dispatch(delegate_args).await {
            Ok(result) => Ok(result),
            Err(e) => Err(format!("Sub-agent delegation failed: {e}")),
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::delegation::SubAgentConfig;
    use crate::runtime::loop_handle::{LoopConfig, RuntimeLoop};
    use crate::runtime::provider::LLMProvider;
    use crate::runtime::provider::{LLMResponse, Message};
    use crate::state::session::SessionManager;
    use crate::tools::create_survival_tools;
    use tempfile::tempdir;

    /// Provider mock para tests.
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
                content: "sub-agent result".into(),
                tool_calls: vec![],
                usage: crate::runtime::provider::TokenUsage::default(),
                extra_fields: vec![],
            })
        }
    }

    fn make_tool() -> DelegateTaskTool {
        let provider: Arc<dyn LLMProvider> = Arc::new(MockProvider::new());
        let tools = create_survival_tools();
        let config = LoopConfig::default();
        let dir = tempdir().expect("temp dir");
        let session = SessionManager::open(dir.path()).expect("session");

        let runtime = Arc::new(RuntimeLoop::new(provider, tools, session, config, None));
        let manager = Arc::new(SubAgentManager::new(runtime, SubAgentConfig::default()));
        DelegateTaskTool::new(manager)
    }

    #[tokio::test]
    async fn test_delegate_tool_name() {
        let tool = make_tool();
        assert_eq!(tool.name(), "delegate_task");
    }

    #[tokio::test]
    async fn test_delegate_tool_description_not_empty() {
        let tool = make_tool();
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn test_delegate_tool_parameters_schema() {
        let tool = make_tool();
        let params = tool.parameters();
        assert_eq!(params["type"], "object");

        let props = params["properties"].as_object().expect("properties");
        assert!(
            props.contains_key("task_objective"),
            "missing task_objective"
        );
        assert!(props.contains_key("context"), "missing context");
        assert!(props.contains_key("role"), "missing role");
        assert!(props.contains_key("toolsets"), "missing toolsets");

        // Required fields
        let required = params["required"].as_array().expect("required");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_names.contains(&"task_objective"));
        assert!(required_names.contains(&"context"));
    }

    #[tokio::test]
    async fn test_delegate_tool_rejects_empty_objective() {
        let tool = make_tool();
        let args = serde_json::json!({
            "task_objective": "",
            "context": "some context"
        });
        let result = tool.call(&args).await;
        assert!(result.is_err(), "Should reject empty objective");
        assert!(
            result.unwrap_err().contains("must not be empty"),
            "Error message should mention empty objective"
        );
    }

    #[tokio::test]
    async fn test_delegate_tool_rejects_missing_fields() {
        let tool = make_tool();
        let args = serde_json::json!({});
        let result = tool.call(&args).await;
        assert!(result.is_err(), "Should reject missing fields");
    }
}
