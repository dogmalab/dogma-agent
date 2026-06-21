//! # execute_script — Ejecuta scripts ligeros
//!
//! Permite ejecutar código en lenguajes interpretados (bash, python,
//! node) y módulos WebAssembly (wasm) en entornos controlados.
//!
//! ## Sandbox WASI
//!
//! Según el `SandboxMode` configurado en `SecurityConfig`:
//! * `Disabled` (default) — Ejecución nativa tradicional.
//! * `Enabled` — Scripts bash/python nativos, módulos `.wasm`
//!   ejecutados dentro del sandbox virtualizado.
//! * `WasmOnly` — Solo módulos WASM, bloquea bash/python/node.

use crate::runtime::wasm_sandbox::{SandboxLimits, SandboxOutput, WasmSandbox};
use crate::tools::security::{CommandVerdict, SandboxMode, ToolGuardrail};
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

/// Herramienta `execute_script`.
pub struct ExecuteScriptTool;

const MAX_SCRIPT_LENGTH: usize = 100_000; // 100 KB
const EXECUTION_TIMEOUT_SECS: u64 = 30;

/// Encuentra el binario para un lenguaje dado.
/// Devuelve `None` para `wasm` (se maneja aparte).
fn resolve_binary(lang: &str) -> Option<&'static str> {
    match lang {
        "bash" | "sh" => Some("bash"),
        "python" | "py" => Some("python3"),
        "node" | "javascript" | "js" => Some("node"),
        "wasm" => None, // se ejecuta via WasmSandbox
        _ => None,
    }
}

/// Determina si el modo sandbox permite la ejecución de un lenguaje dado.
fn sandbox_check(sandbox_mode: SandboxMode, lang: &str) -> Result<(), String> {
    match sandbox_mode {
        SandboxMode::Disabled => Ok(()),
        SandboxMode::Enabled => {
            // Solo wasm usa sandbox; bash/python/node van nativos
            Ok(())
        }
        SandboxMode::WasmOnly => {
            if lang != "wasm" {
                return Err(format!(
                    "[security] SandboxMode::WasmOnly: execution of '{lang}' scripts is blocked. \
                     Only pre-compiled .wasm modules are allowed."
                ));
            }
            Ok(())
        }
    }
}

#[async_trait]
impl Tool for ExecuteScriptTool {
    fn name(&self) -> &'static str {
        "execute_script"
    }

    fn description(&self) -> &'static str {
        "Execute a script in a supported language (bash, python, node, wasm). \
         Returns stdout and stderr. Timeout after 30 seconds."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "lang": {
                    "type": "string",
                    "enum": ["bash", "sh", "python", "py", "node", "javascript", "js", "wasm"],
                    "description": "Scripting language. Use 'wasm' for base64-encoded WebAssembly modules."
                },
                "code": {
                    "type": "string",
                    "description": "Code to execute. For 'wasm' language, use base64-encoded WASM binary."
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
                let msg = format!("[security] {reason}");
                let prompt = format!("{lang}: {code}");
                warn!("{msg}: {prompt}");
                ToolGuardrail::request_approval(&command).await?;
            }
            CommandVerdict::Allowed => { /* continuar */ }
        }

        // ── Chequeo de sandbox mode ─────────────────────────────────
        let config = ToolGuardrail::config();
        sandbox_check(config.sandbox_mode, lang)?;

        // ── Ejecución según lenguaje ─────────────────────────────────
        let output = match lang {
            "wasm" => run_wasm_in_sandbox(code, &config).await?,
            _ => run_script_native(lang, code).await?,
        };

        Ok(output)
    }
}

// ── Helpers de ejecución ────────────────────────────────────────────────

/// Resultado de la ejecución de un script.
struct ScriptOutput {
    stdout: String,
    stderr: String,
}

/// Ejecuta un módulo WASM dentro del sandbox virtualizado.
///
/// El `code` debe ser base64-encoded WASM binary.
async fn run_wasm_in_sandbox(
    code: &str,
    config: &crate::tools::security::SecurityConfig,
) -> Result<String, String> {
    use base64::Engine;

    // Decodificar base64 → bytes WASM
    let wasm_bytes = base64::engine::general_purpose::STANDARD
        .decode(code)
        .map_err(|e| format!("wasm: invalid base64 payload: {e}"))?;

    // Límites del sandbox (desde config o defaults)
    let mut limits = config.sandbox_limits.clone().unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        SandboxLimits {
            max_fuel: 1_000_000,
            allowed_workspace: cwd,
            temp_dir: Some(std::env::temp_dir()),
        }
    });

    // Asegurar temp_dir para capturar stdout/stderr
    if limits.temp_dir.is_none() {
        limits.temp_dir = Some(std::env::temp_dir());
    }

    // Compilar y ejecutar dentro de la micro-VM
    let sandbox =
        WasmSandbox::new(&wasm_bytes).map_err(|e| format!("wasm: compilation failed: {e}"))?;

    debug!(
        "WASM sandbox: running module ({} bytes, fuel={})",
        wasm_bytes.len(),
        limits.max_fuel
    );

    let output: SandboxOutput = tokio::time::timeout(
        Duration::from_secs(EXECUTION_TIMEOUT_SECS),
        sandbox.run_captured(&limits, &[]),
    )
    .await
    .map_err(|_| format!("wasm: execution timed out after {EXECUTION_TIMEOUT_SECS}s"))?
    .map_err(|e| format!("wasm: execution failed: {e}"))?;

    let fuel_used = limits.max_fuel.saturating_sub(output.fuel_remaining);

    debug!(
        "WASM sandbox: completed (stdout={}, stderr={}, fuel_used={})",
        output.stdout.len(),
        output.stderr.len(),
        fuel_used,
    );

    // Formatear salida usando Display del SandboxOutput
    let result = output.to_string();

    if result.is_empty() {
        return Ok("wasm: executed successfully (no output)".to_string());
    }

    Ok(result)
}

/// Ejecuta un script usando el binario nativo del sistema.
async fn run_script_native(lang: &str, code: &str) -> Result<String, String> {
    let binary = resolve_binary(lang).ok_or_else(|| format!("unsupported language: {lang}"))?;

    let output = tokio::time::timeout(
        Duration::from_secs(EXECUTION_TIMEOUT_SECS),
        run_script_native_inner(binary, code),
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

/// Ejecución real del subproceso nativo.
async fn run_script_native_inner(
    binary: &str,
    code: &str,
) -> std::result::Result<ScriptOutput, std::io::Error> {
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

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::security::SecurityConfig;
    use base64::Engine;
    use serde_json::json;

    fn setup_free() {
        ToolGuardrail::set_config(SecurityConfig {
            mode: crate::tools::security::SecurityMode::Free,
            allowed_dirs: vec![],
            sandbox_mode: SandboxMode::Disabled,
            sandbox_limits: None,
        });
    }

    fn setup_wasm_only() {
        ToolGuardrail::set_config(SecurityConfig {
            mode: crate::tools::security::SecurityMode::Free,
            allowed_dirs: vec![],
            sandbox_mode: SandboxMode::WasmOnly,
            sandbox_limits: None,
        });
    }

    #[tokio::test]
    async fn test_missing_args() {
        let tool = ExecuteScriptTool;
        assert!(tool.call(&json!({})).await.is_err());
        assert!(tool.call(&json!({"lang": "bash"})).await.is_err());
    }

    #[tokio::test]
    async fn test_unsupported_language() {
        setup_free();
        let tool = ExecuteScriptTool;
        let args = json!({"lang": "ruby", "code": "puts 'hi'"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unsupported language"));
    }

    #[tokio::test]
    async fn test_bash_echo() {
        setup_free();
        let tool = ExecuteScriptTool;
        let args = json!({"lang": "bash", "code": "echo hello_dogma"});
        let result = tool.call(&args).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("hello_dogma"), "output was: {output}");
    }

    #[tokio::test]
    async fn test_wasm_only_blocks_bash() {
        setup_wasm_only();
        let tool = ExecuteScriptTool;
        let args = json!({"lang": "bash", "code": "echo blocked"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("WasmOnly"), "error: {err}");
        assert!(err.contains("blocked"), "error: {err}");
    }

    #[tokio::test]
    async fn test_wasm_only_allows_wasm_if_valid_base64() {
        setup_wasm_only();
        let tool = ExecuteScriptTool;

        // Módulo WASM vacío (mínimo válido compilado desde wat)
        let wat = r#"(module (memory (export "memory") 1) (func (export "_start")))"#;
        let wasm = wat::parse_str(wat).expect("wat parse");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wasm);

        let args = json!({"lang": "wasm", "code": b64});
        let result = tool.call(&args).await;

        // Debe ejecutar sin error
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn test_wasm_only_rejects_invalid_base64() {
        setup_wasm_only();
        let tool = ExecuteScriptTool;

        let args = json!({"lang": "wasm", "code": "not-valid-base64!!!"});
        let result = tool.call(&args).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("base64"),
            "should mention base64 error"
        );
    }

    #[tokio::test]
    async fn test_wasm_sandbox_hello() {
        setup_free();
        let tool = ExecuteScriptTool;

        // Módulo WASI que escribe "hello from wasm\n" a stdout
        let wat = r#"
(module
    (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
    (export "_start" (func $_start))
    (func $_start
        i32.const 1
        i32.const 64
        i32.const 1
        i32.const 80
        call $fd_write
        drop
    )
    (data (i32.const 8) "hello from wasm\n")
    (data (i32.const 64) "\08\00\00\00\11\00\00\00")
)
"#;
        let wasm = wat::parse_str(wat).expect("wat parse");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&wasm);

        let args = json!({"lang": "wasm", "code": b64});
        let result = tool.call(&args).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let output = result.unwrap();
        assert!(output.contains("hello from wasm"), "output: {output}");
    }
}
