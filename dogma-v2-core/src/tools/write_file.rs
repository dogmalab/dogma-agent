//! # write_file — Escribe contenido en un archivo
//!
//! Crea o sobrescribe un archivo con el contenido proporcionado.
//! Límite de 1 MB para prevenir abusos. Valida el path contra los
//! directorios permitidos según el modo de seguridad configurado.

use crate::tools::security::ToolGuardrail;
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

/// Herramienta `write_file`.
pub struct WriteFileTool;

const MAX_WRITE_SIZE: usize = 1_048_576; // 1 MB

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write content to a file. Creates parent directories if needed. Maximum 1 MB."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: path".to_string())?;

        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: content".to_string())?;

        if content.len() > MAX_WRITE_SIZE {
            return Err(format!(
                "content too large ({} bytes, max {MAX_WRITE_SIZE})",
                content.len()
            ));
        }

        // Validar path contra los guardrails de seguridad
        let validated_path = ToolGuardrail::validate_path(path)?;

        // Create parent directories
        if let Some(parent) = validated_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create directory {parent:?}: {e}"))?;
        }

        std::fs::write(&validated_path, content)
            .map_err(|e| format!("cannot write {path}: {e}"))?;

        Ok(format!(
            "successfully wrote {} bytes to {path}",
            content.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_missing_args() {
        let tool = WriteFileTool;
        assert!(tool.call(&json!({})).await.is_err());
        assert!(tool.call(&json!({"path": "/tmp/test.txt"})).await.is_err());
    }
}
