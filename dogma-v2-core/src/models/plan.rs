//! # Plan — Modelo de planificación para tareas complejas
//!
//! Representa un plan estructurado con pasos secuenciales que el LLM
//! puede crear usando el tool `plan` y luego ejecutar paso a paso
//! usando los otros tools disponibles.

use serde::{Deserialize, Serialize};

/// Estado de un paso del plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepStatus::Pending => write!(f, "pending"),
            StepStatus::InProgress => write!(f, "in_progress"),
            StepStatus::Completed => write!(f, "completed"),
            StepStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Un paso individual dentro de un plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub step_number: u32,
    pub description: String,
    pub status: StepStatus,
}

/// Un plan estructurado creado por el LLM.
///
/// Los planes se persisten en dogma-vdb como nodos con
/// `node_type: "Plan"` y `node_type: "PlanStep"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub task: String,
    pub steps: Vec<PlanStep>,
    pub created_at: String,
}

impl Plan {
    /// Crea un nuevo plan con un ID auto-generado y pasos pendientes.
    #[must_use]
    pub fn new(task: &str, step_descriptions: &[String]) -> Self {
        let id = format!(
            "plan_{}",
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("x")
        );

        let steps: Vec<PlanStep> = step_descriptions
            .iter()
            .enumerate()
            .map(|(i, desc)| PlanStep {
                id: format!("{id}_s{}", i + 1),
                step_number: (i + 1) as u32,
                description: desc.clone(),
                status: StepStatus::Pending,
            })
            .collect();

        Self {
            id,
            task: task.to_string(),
            steps,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Formatea el plan como texto legible para el LLM.
    #[must_use]
    pub fn format_display(&self) -> String {
        let mut output = format!("Plan: {}\n─────────────────────────────────\n", self.task);
        for step in &self.steps {
            let check = match step.status {
                StepStatus::Pending => "☐",
                StepStatus::InProgress => "◉",
                StepStatus::Completed => "☑",
                StepStatus::Failed => "✗",
            };
            output.push_str(&format!(
                "{check} {}. {}\n",
                step.step_number, step.description
            ));
        }
        output.push_str(&format!(
            "─────────────────────────────────\nPlan ID: {}",
            self.id
        ));
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_creation() {
        let steps = vec![
            "Read the code".to_string(),
            "Write the fix".to_string(),
            "Run tests".to_string(),
        ];
        let plan = Plan::new("Fix the bug", &steps);

        assert!(plan.id.starts_with("plan_"));
        assert_eq!(plan.task, "Fix the bug");
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].step_number, 1);
        assert_eq!(plan.steps[0].status, StepStatus::Pending);
        assert!(plan.steps[0].id.starts_with(&plan.id));
    }

    #[test]
    fn test_plan_format_display() {
        let steps = vec!["Step one".to_string(), "Step two".to_string()];
        let plan = Plan::new("Test task", &steps);
        let display = plan.format_display();

        assert!(display.contains("Plan: Test task"));
        assert!(display.contains("☐ 1. Step one"));
        assert!(display.contains("☐ 2. Step two"));
        assert!(display.contains("Plan ID:"));
    }

    #[test]
    fn test_plan_empty_steps() {
        let plan = Plan::new("Empty plan", &[]);
        assert_eq!(plan.steps.len(), 0);
    }

    #[test]
    fn test_step_status_display() {
        assert_eq!(StepStatus::Pending.to_string(), "pending");
        assert_eq!(StepStatus::Completed.to_string(), "completed");
    }

    #[test]
    fn test_plan_serialization_roundtrip() {
        let steps = vec!["A".to_string(), "B".to_string()];
        let plan = Plan::new("Test", &steps);
        let json = serde_json::to_string(&plan).expect("serialize");
        let loaded: Plan = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(plan.id, loaded.id);
        assert_eq!(plan.task, loaded.task);
        assert_eq!(plan.steps.len(), loaded.steps.len());
    }
}
