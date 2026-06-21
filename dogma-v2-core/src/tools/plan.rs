//! # plan — Tool de planificación para tareas complejas
//!
//! Permite al LLM crear planes estructurados con pasos secuenciales.
//! Los planes se persisten en dogma-vdb y se pueden consultar después.

use std::sync::Arc;

use crate::models::plan::Plan;
use crate::state::session::SessionManager;
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use tracing::debug;

/// Session ID fijo para planes globales.
/// Los planes se almacenan con este ID para distinguirlos de mensajes.
const PLANS_SESSION_ID: &str = "__plans__";

/// Tool `plan` — crea planes estructurados para tareas complejas.
pub struct PlanTool {
    session: Arc<RwLock<SessionManager>>,
}

impl PlanTool {
    /// Crea una nueva instancia de PlanTool.
    #[must_use]
    pub fn new(session: Arc<RwLock<SessionManager>>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }

    fn description(&self) -> &'static str {
        "Create a structured plan for a complex task. Breaks a goal into \
         sequential steps. Use this at the start of complex tasks to organize \
         your approach. Each step can then be executed using other tools \
         (read_file, delegate_task, execute_script, etc.). Returns the plan \
         with step numbers and IDs."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "required": ["task", "steps"],
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The main objective or task description"
                },
                "steps": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Ordered list of steps to accomplish the task"
                }
            }
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: task".to_string())?;

        let steps_raw = args
            .get("steps")
            .and_then(Value::as_array)
            .ok_or_else(|| "missing required argument: steps".to_string())?;

        let steps: Vec<String> = steps_raw
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        if steps.is_empty() {
            return Err("steps array must not be empty".to_string());
        }

        // Crear plan
        let plan = Plan::new(task, &steps);
        let display = plan.format_display();
        debug!("Plan created: {} with {} steps", plan.id, plan.steps.len());

        // Persistir a dogma-vdb
        {
            let mut session = self.session.write();
            session
                .save_plan(PLANS_SESSION_ID, &plan)
                .map_err(|e| format!("failed to save plan: {e}"))?;
        }

        Ok(display)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::session::SessionManager;
    use tempfile::TempDir;

    /// Keeps TempDir alive for the duration of the test.
    struct TestPlanTool {
        _dir: TempDir,
        tool: PlanTool,
    }

    fn test_plan_tool() -> TestPlanTool {
        let dir = tempfile::tempdir().expect("temp dir");
        let session = SessionManager::open(dir.path()).expect("session manager");
        TestPlanTool {
            _dir: dir,
            tool: PlanTool::new(Arc::new(RwLock::new(session))),
        }
    }

    #[test]
    fn test_plan_tool_name() {
        let t = test_plan_tool();
        assert_eq!(t.tool.name(), "plan");
    }

    #[test]
    fn test_plan_tool_description_not_empty() {
        let t = test_plan_tool();
        assert!(!t.tool.description().is_empty());
    }

    #[test]
    fn test_plan_tool_parameters_schema() {
        let t = test_plan_tool();
        let params = t.tool.parameters();
        assert_eq!(params["type"], "object");

        let props = params["properties"].as_object().expect("properties");
        assert!(props.contains_key("task"));
        assert!(props.contains_key("steps"));

        let required = params["required"].as_array().expect("required");
        let required_names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(required_names.contains(&"task"));
        assert!(required_names.contains(&"steps"));
    }

    #[tokio::test]
    async fn test_plan_tool_call() {
        let t = test_plan_tool();
        let args = serde_json::json!({
            "task": "Fix the bug",
            "steps": ["Read code", "Write fix", "Run tests"]
        });
        let result = t.tool.call(&args).await;
        assert!(
            result.is_ok(),
            "plan tool should succeed: {:?}",
            result.err()
        );
        let output = result.unwrap();
        assert!(output.contains("Plan: Fix the bug"));
        assert!(output.contains("☐ 1. Read code"));
        assert!(output.contains("☐ 2. Write fix"));
    }

    #[tokio::test]
    async fn test_plan_tool_missing_task() {
        let t = test_plan_tool();
        let args = serde_json::json!({ "steps": ["A"] });
        let result = t.tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_plan_tool_missing_steps() {
        let t = test_plan_tool();
        let args = serde_json::json!({ "task": "Do it" });
        let result = t.tool.call(&args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_plan_tool_empty_steps() {
        let t = test_plan_tool();
        let args = serde_json::json!({ "task": "Do it", "steps": [] });
        let result = t.tool.call(&args).await;
        assert!(result.is_err());
    }
}
