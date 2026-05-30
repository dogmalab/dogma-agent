//! # execute_script — Ejecuta scripts ligeros
//!
//! Permite ejecutar código en lenguajes interpretados (bash, python,
//! node) en entornos controlados. Reemplaza las 72 herramientas
//! estáticas del Dogma 1.0 con un único ejecutor polivalente.
//!
//! ## Seguridad
//!
//! Según el `SecurityMode` configurado:
//! * `Confined` — Todos los scripts son bloqueados inmediatamente.
//! * `SemiAutonomous` — Comandos bash/sh peligrosos requieren
//!   autorización humana vía HITL (stdin en CLI, evento en modo JSON).
//! * `Free` — Sin restricciones.

use crate::tools::security::{CommandVerdict, ToolGuardrail};
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;
use tracing::warn;

/// Herramienta `execute_script`.
pub struct ExecuteScriptTool;

const MAX_SCRIPT_LENGTH: usize = 100_000; // 100 KB
const EXECUTION_TIMEOUT_SECS: u64 = 30;

/// Encuentra el binario para un lenguaje dado.
fn resolve_binary(lang: &str) -> Option<&'static str> {
    match lang {
        "bash" | "sh" => Some("bash"),
        "python" | "py" => Some("python3"),
        "node" | "javascript" | "js" => Some("node"),
        _ => None,
    }
}

#[async_trait]
impl Tool for ExecuteScriptTool {
    fn name(&self) -> &'static str {
        "execute_script"
    }

    fn description(&self) -> &'static str {
        "Execute a script in a supported language (bash, python, node). Returns stdout and stderr. Timeout after 30 seconds."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "lang": {
                    "type": "string",
                    "enum": ["bash", "sh", "python", "py", "node", "javascript", "js"],
                    "description": "Scripting language"
                },
                "code": {
                    "type": "string",
                    "description": "Code to execute"
                }
            },
            "required": ["lang", "code"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let lang = args
            .get("lang")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: lang".to_string())?;

        let code = args
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: code".to_string())?;

        if code.len() > MAX_SCRIPT_LENGTH {
            return Err(format!(
                "script too large ({} bytes, max {MAX_SCRIPT_LENGTH})",
                code.len()
            ));
        }

        // ── Inspección de seguridad ──────────────────────────────────
        match ToolGuardrail::inspect_command(lang, code) {
            CommandVerdict::Blocked { reason } => {
                return Err(format!("[security] {reason}"));
            }
            CommandVerdict::RequiresAuthorization { command, reason } => {
                // En SemiAutonomous mode, pedir aprobación humana
                let msg = format!("[security] {reason}");
                let prompt = format!("{lang}: {code}");
                warn!("{msg}: {prompt}");
                ToolGuardrail::request_approval(&command).await?;
            }
            CommandVerdict::Allowed => { /* continuar */ }
        }

        let binary = resolve_binary(lang).ok_or_else(|| format!("unsupported language: {lang}"))?;

        // Ejecutar el script como código inline
        let output = tokio::time::timeout(
            Duration::from_secs(EXECUTION_TIMEOUT_SECS),
            run_script(binary, code),
        )
        .await
        .map_err(|_| format!("script execution timed out after {EXECUTION_TIMEOUT_SECS}s"))?
        .map_err(|e| format!("script execution failed: {e}"))?;

        let mut result = String::new();

        if !output.stdout.is_empty() {
            result.push_str(&output.stdout);
        }

        if !output.stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!("[stderr]\n{}", output.stderr));
        }

        if result.is_empty() {
            result = "script completed (no output)".to_string();
        }

        // Truncar si es muy largo
        if result.len() > 50_000 {
            warn!("Script output truncated ({} bytes)", result.len());
            result.truncate(50_000);
            result.push_str("\n... [truncated]");
        }

        Ok(result)
    }
}

/// Resultado de la ejecución de un script.
struct ScriptOutput {
    stdout: String,
    stderr: String,
}

/// Ejecuta un script usando el binario especificado.
async fn run_script(binary: &str, code: &str) -> std::result::Result<ScriptOutput, std::io::Error> {
    let output = tokio::process::Command::new(binary)
        .arg("-c")
        .arg(code)
        .output()
        .await?;

    Ok(ScriptOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_missing_args() {
        let tool = ExecuteScriptTool;
        assert!(tool.call(&json!({})).await.is_err());
        assert!(tool.call(&json!({"lang": "bash"})).await.is_err());
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        let tool = ExecuteScriptTool;
        let args = json!({"lang": "ruby", "code": "puts 'hi'"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported language"));
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = ExecuteScriptTool;
        let args = json!({"lang": "bash", "code": "echo hello_dogma"});
        let result = tool.call(&args).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello_dogma"), "output was: {output}");
    }
}
