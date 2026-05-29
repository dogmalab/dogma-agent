//! # read_file — Lee el contenido de un archivo
//!
//! Lee un archivo del sistema de archivos local y devuelve su contenido
//! como texto. Límite de 1 MB para evitar saturar el contexto del LLM.

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

/// Herramienta `read_file`.
pub struct ReadFileTool;

const MAX_READ_SIZE: u64 = 1_048_576; // 1 MB

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read the contents of a file from the local filesystem. Maximum 1 MB."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: path".to_string())?;

        let metadata = std::fs::metadata(path).map_err(|e| format!("cannot access {path}: {e}"))?;

        if metadata.len() > MAX_READ_SIZE {
            return Err(format!(
                "file too large ({} bytes, max {MAX_READ_SIZE})",
                metadata.len()
            ));
        }

        if metadata.is_dir() {
            return Err(format!("{path} is a directory, not a file"));
        }

        let content =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;

        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_missing_path() {
        let tool = ReadFileTool;
        let args = json!({});
        let result = tool.call(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument"));
    }

    #[tokio::test]
    async fn test_nonexistent_file() {
        let tool = ReadFileTool;
        let args = json!({"path": "/tmp/nonexistent_file_xxxx123"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
    }
}
