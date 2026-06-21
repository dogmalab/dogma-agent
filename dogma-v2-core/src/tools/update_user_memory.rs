//! # update_user_memory — Tool para gestionar la memoria del usuario
//!
//! Permite al agente leer, escribir, eliminar y buscar en la memoria
//! persistente del usuario (key-value store en dogma-vdb).

use crate::state::user_memory::UserMemory;
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use std::sync::Arc;

/// Tool que permite al agente gestionar la memoria del usuario.
pub struct UpdateUserMemoryTool {
    user_memory: Arc<RwLock<UserMemory>>,
}

impl UpdateUserMemoryTool {
    pub fn new(user_memory: Arc<RwLock<UserMemory>>) -> Self {
        Self { user_memory }
    }
}

#[async_trait]
impl Tool for UpdateUserMemoryTool {
    fn name(&self) -> &'static str {
        "update_user_memory"
    }

    fn description(&self) -> &'static str {
        "Read, write, or search the user's persistent memory. \
         Store preferences, system info, habits, and knowledge \
         that persists across sessions."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["set", "get", "remove", "list", "search"],
                    "description": "Action to perform"
                },
                "key": {
                    "type": "string",
                    "description": "Key name (for set/get/remove)"
                },
                "value": {
                    "type": "string",
                    "description": "Value to store (for set)"
                },
                "category": {
                    "type": "string",
                    "enum": ["system", "preference", "knowledge"],
                    "description": "Category for the entry (for set)",
                    "default": "preference"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (for search)"
                }
            },
            "required": ["action"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: action".to_string())?;

        match action {
            "set" => {
                let key = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "set requires 'key'".to_string())?;
                let value = args
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "set requires 'value'".to_string())?;
                let category = args
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("preference");

                let mut mem = self.user_memory.write();
                mem.set(key, value, category)
                    .map_err(|e| format!("failed to set: {e}"))?;

                Ok(format!("Stored '{key}' = '{value}' (category: {category})"))
            }
            "get" => {
                let key = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "get requires 'key'".to_string())?;

                let mem = self.user_memory.read();
                match mem.get(key) {
                    Some(value) => Ok(value),
                    None => Ok(format!("Key '{key}' not found")),
                }
            }
            "remove" => {
                let key = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "remove requires 'key'".to_string())?;

                let mut mem = self.user_memory.write();
                let removed = mem
                    .remove(key)
                    .map_err(|e| format!("failed to remove: {e}"))?;

                if removed {
                    Ok(format!("Removed '{key}'"))
                } else {
                    Ok(format!("Key '{key}' not found"))
                }
            }
            "list" => {
                let mem = self.user_memory.read();
                let entries = mem.entries();
                if entries.is_empty() {
                    return Ok("No entries in user memory".to_string());
                }

                let mut output = String::from("User memory entries:\n");
                for (key, value, category) in &entries {
                    output.push_str(&format!("  - {key}: {value} [{category}]\n"));
                }
                Ok(output)
            }
            "search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "search requires 'query'".to_string())?;

                let mem = self.user_memory.read();
                let results = mem.search(query);

                if results.is_empty() {
                    return Ok(format!("No results for '{query}'"));
                }

                let mut output = format!("Search results for '{query}':\n");
                for (key, value) in &results {
                    output.push_str(&format!("  - {key}: {value}\n"));
                }
                Ok(output)
            }
            _ => Err(format!("unknown action: {action}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool() -> (tempfile::TempDir, UpdateUserMemoryTool) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("user_memory.vdb");
        let mem = UserMemory::open(&path).unwrap();
        let tool = UpdateUserMemoryTool::new(Arc::new(RwLock::new(mem)));
        (dir, tool)
    }

    #[tokio::test]
    async fn test_set_and_get() {
        let (_dir, tool) = make_tool();
        let result = tool
            .call(&serde_json::json!({
                "action": "set",
                "key": "temp_dir",
                "value": "/tmp/work",
                "category": "system"
            }))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Stored"));

        let result = tool
            .call(&serde_json::json!({
                "action": "get",
                "key": "temp_dir"
            }))
            .await;
        assert_eq!(result.unwrap(), "/tmp/work");
    }

    #[tokio::test]
    async fn test_remove() {
        let (_dir, tool) = make_tool();
        tool.call(&serde_json::json!({"action": "set", "key": "k", "value": "v"}))
            .await
            .unwrap();

        let result = tool
            .call(&serde_json::json!({"action": "remove", "key": "k"}))
            .await;
        assert!(result.unwrap().contains("Removed"));
    }

    #[tokio::test]
    async fn test_list() {
        let (_dir, tool) = make_tool();
        tool.call(&serde_json::json!({"action": "set", "key": "a", "value": "1"}))
            .await
            .unwrap();

        let result = tool.call(&serde_json::json!({"action": "list"})).await;
        assert!(result.unwrap().contains("a: 1"));
    }

    #[tokio::test]
    async fn test_search() {
        let (_dir, tool) = make_tool();
        tool.call(&serde_json::json!({"action": "set", "key": "temp_dir", "value": "/tmp/work"}))
            .await
            .unwrap();

        let result = tool
            .call(&serde_json::json!({"action": "search", "query": "tmp"}))
            .await;
        assert!(result.unwrap().contains("temp_dir"));
    }

    #[tokio::test]
    async fn test_missing_action() {
        let (_dir, tool) = make_tool();
        let result = tool.call(&serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_unknown_action() {
        let (_dir, tool) = make_tool();
        let result = tool
            .call(&serde_json::json!({"action": "unknown"}))
            .await;
        assert!(result.is_err());
    }
}
