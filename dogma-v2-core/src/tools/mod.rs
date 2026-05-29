//! # Tools — Las 3 herramientas de supervivencia
//!
//! Este módulo registra únicamente las herramientas esenciales:
//!
//! 1. `read_file(path)` — Lee el contenido de un archivo.
//! 2. `write_file(path, content)` — Escribe contenido en un archivo.
//! 3. `execute_script(lang, code)` — Ejecuta scripts ligeros.
//!
//! Cada herramienta implementa el trait `Tool` y se registra en el
//! `ToolRegistry` del runtime.

mod execute_script;
mod read_file;
mod write_file;

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

pub use execute_script::ExecuteScriptTool;
pub use read_file::ReadFileTool;
pub use write_file::WriteFileTool;

/// Resultado de la ejecución de una herramienta.
pub type ToolResult = std::result::Result<String, String>;

/// Trait que toda herramienta debe implementar.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Nombre único de la herramienta (ej: `read_file`).
    fn name(&self) -> &'static str;

    /// Descripción breve para el sistema de herramientas del LLM.
    fn description(&self) -> &'static str;

    /// Esquema de los parámetros en formato JSON Schema.
    fn parameters(&self) -> serde_json::Value;

    /// Ejecuta la herramienta con los argumentos dados (JSON).
    ///
    /// # Errors
    ///
    /// Devuelve `Err(String)` con un mensaje descriptivo si la
    /// ejecución falla.
    async fn call(&self, args: &serde_json::Value) -> ToolResult;
}

/// Registro threadsafe de herramientas disponibles.
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Crea un registro vacío.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Registra una herramienta en el mapa.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name();
        debug!("Registering tool: {name}");
        self.tools.insert(name, Arc::from(tool));
    }

    /// Devuelve una referencia clonada a una herramienta por nombre.
    pub fn get_tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Ejecuta una herramienta por nombre con argumentos JSON.
    ///
    /// # Errors
    ///
    /// Devuelve `Err(String)` si la herramienta no existe o falla.
    pub async fn execute(&self, name: &str, args_json: &str) -> ToolResult {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("tool not found: {name}"))?;

        let args: serde_json::Value = serde_json::from_str(args_json)
            .map_err(|e| format!("invalid arguments for {name}: {e}"))?;

        tool.call(&args).await
    }

    /// Devuelve la especificación de todas las herramientas para
    /// inyectar en el prompt del sistema.
    pub fn tool_specs(&self) -> Vec<serde_json::Value> {
        self.tools
            .values()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name(),
                        "description": tool.description(),
                        "parameters": tool.parameters(),
                    }
                })
            })
            .collect()
    }

    /// Número de herramientas registradas.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Devuelve `true` si no hay herramientas registradas.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Crea un `ToolRegistry` con las 3 herramientas de supervivencia.
pub fn create_survival_tools() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadFileTool));
    registry.register(Box::new(WriteFileTool));
    registry.register(Box::new(ExecuteScriptTool));
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_empty() {
        let registry = ToolRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[tokio::test]
    async fn test_registry_survival_tools() {
        let registry = create_survival_tools();
        assert_eq!(registry.len(), 3);
        assert!(registry.tools.contains_key("read_file"));
        assert!(registry.tools.contains_key("write_file"));
        assert!(registry.tools.contains_key("execute_script"));
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let registry = ToolRegistry::new();
        let result = registry.execute("nonexistent", "{}").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }
}
