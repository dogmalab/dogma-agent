//! # Config — Carga de configuración del agente
//!
//! Busca la configuración en este orden de precedencia:
//!
//! 1. Archivo `keys.toml` en el directorio actual (o ruta especificada).
//! 2. Variables de entorno: `DOGMA_BASE_URL`, `DOGMA_MODEL`, `DOGMA_API_KEY`.
//!
//! ## Formato de `keys.toml`
//!
//! ```toml
//! [provider]
//! name = "deepseek-v4-flash"
//! base_url = "https://opencode.ai/zen/go/v1"
//! model = "deepseek-v4-flash"
//! api_key = "sk-..."
//!
//! [runtime]
//! max_tool_iterations = 50
//! ```

use std::path::{Path, PathBuf};

use dogma_v2_core::runtime::provider::ProviderConfig;
use serde::Deserialize;

/// Configuración completa del agente (provider + runtime).
#[derive(Debug, Clone)]
pub struct DogmaConfig {
    pub provider: ProviderConfig,
    pub max_tool_iterations: u32,
}

impl Default for DogmaConfig {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            max_tool_iterations: 50,
        }
    }
}

/// Estructura que refleja el archivo keys.toml.
#[derive(Debug, Deserialize)]
struct KeysToml {
    provider: ProviderSection,
    #[serde(default)]
    runtime: RuntimeSection,
}

/// Sección `[provider]` del keys.toml.
#[derive(Debug, Deserialize)]
struct ProviderSection {
    #[allow(dead_code)]
    name: Option<String>,
    base_url: String,
    model: String,
    api_key: String,
}

/// Sección `[runtime]` del keys.toml (opcional).
#[derive(Debug, Deserialize, Default)]
struct RuntimeSection {
    max_tool_iterations: Option<u32>,
}

/// Obtiene el directorio home del usuario.
/// En tests, se puede override con `DOGMA_HOME`.
fn dirs() -> Option<PathBuf> {
    #[cfg(test)]
    if let Ok(custom) = std::env::var("DOGMA_HOME") {
        return Some(PathBuf::from(custom));
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Carga la configuración completa del agente.
///
/// Orden de precedencia:
/// 1. `keys.toml` en la ruta indicada (o `./keys.toml` por defecto).
/// 2. Variables de entorno: `DOGMA_BASE_URL`, `DOGMA_MODEL`, `DOGMA_API_KEY`.
///
/// La sección `[runtime]` es opcional — si no está, se usan defaults.
pub fn load_config(keys_path: Option<&Path>) -> Result<DogmaConfig, String> {
    // ── Intento 1: keys.toml (ruta explícita o directorio actual) ───
    let path = keys_path.unwrap_or_else(|| Path::new("keys.toml"));
    if path.exists() {
        if let Some(config) = load_from_toml_file(path) {
            return Ok(config);
        }
    }

    // ── Intento 2: ~/.dogma/keys.toml ───────────────────────────────
    let home_keys = dirs()
        .map(|h| h.join(".dogma").join("keys.toml"))
        .filter(|p| p.exists());

    if let Some(home_path) = home_keys {
        if let Some(config) = load_from_toml_file(&home_path) {
            return Ok(config);
        }
    }

    // ── Intento 3: variables de entorno ──────────────────────────────
    let base_url = std::env::var("DOGMA_BASE_URL").ok();
    let model = std::env::var("DOGMA_MODEL").ok();
    let api_key = std::env::var("DOGMA_API_KEY").ok();

    if let (Some(url), Some(mdl), Some(key)) = (&base_url, &model, &api_key) {
        return Ok(DogmaConfig {
            provider: ProviderConfig {
                base_url: url.clone(),
                model: mdl.clone(),
                api_key: Some(key.clone()),
                ..Default::default()
            },
            max_tool_iterations: 50,
        });
    }

    // Si hay configuración parcial de env, reportar qué falta
    let mut missing = Vec::new();
    if base_url.is_none() {
        missing.push("DOGMA_BASE_URL");
    }
    if model.is_none() {
        missing.push("DOGMA_MODEL");
    }
    if api_key.is_none() {
        missing.push("DOGMA_API_KEY");
    }

    if missing.len() < 3 {
        return Err(format!(
            "Incomplete environment configuration. Missing: {}",
            missing.join(", ")
        ));
    }

    Err("No provider configuration found.\n\
         Create a keys.toml file with:\n\
         \n  [provider]\n  base_url = \"...\"\n  model = \"...\"\n  api_key = \"...\"\n\
         \n  [runtime]\n  max_tool_iterations = 50\n\
         \nOr set environment variables:\n  DOGMA_BASE_URL\n  DOGMA_MODEL\n  DOGMA_API_KEY"
        .to_string())
}

/// Intenta cargar config desde un archivo TOML.
fn load_from_toml_file(path: &Path) -> Option<DogmaConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    let toml_config: KeysToml = toml::from_str(&content).ok()?;

    Some(DogmaConfig {
        provider: ProviderConfig {
            base_url: toml_config.provider.base_url,
            model: toml_config.provider.model,
            api_key: Some(toml_config.provider.api_key),
            ..Default::default()
        },
        max_tool_iterations: toml_config.runtime.max_tool_iterations.unwrap_or(50),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::LazyLock;
    use std::sync::Mutex;

    /// Mutex global para serializar tests que modifican env vars.
    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn with_env_vars<R>(kvs: &[(&str, &str)], f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let all_keys: Vec<String> = std::env::vars()
            .filter(|(k, _)| k.starts_with("DOGMA_"))
            .map(|(k, _)| k)
            .collect();
        let saved: Vec<(String, Option<String>)> = all_keys
            .iter()
            .map(|k| (k.clone(), std::env::var(k).ok()))
            .collect();

        for (k, _) in &saved {
            unsafe {
                std::env::remove_var(k);
            }
        }

        for &(k, v) in kvs {
            unsafe {
                std::env::set_var(k, v);
            }
        }

        let result = f();

        for (k, original) in &saved {
            match original {
                Some(val) => unsafe {
                    std::env::set_var(k, &val);
                },
                None => unsafe {
                    std::env::remove_var(k);
                },
            }
        }

        result
    }

    fn write_keys_toml(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("keys.toml");
        let mut f = std::fs::File::create(&path).expect("create keys.toml");
        write!(f, "{content}").expect("write keys.toml");
        path
    }

    #[test]
    fn test_load_from_toml() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let content = r#"
[provider]
name = "big-pickle"
base_url = "https://opencode.ai/zen/v1"
model = "@ai-sdk/openai-compatible"
api_key = "sk-test-key-for-toml"

[runtime]
max_tool_iterations = 30
"#;
        let path = write_keys_toml(&dir, content);

        let config = load_config(Some(&path)).expect("load from toml");
        assert_eq!(config.provider.base_url, "https://opencode.ai/zen/v1");
        assert_eq!(config.provider.model, "@ai-sdk/openai-compatible");
        assert_eq!(config.provider.api_key, Some("sk-test-key-for-toml".into()));
        assert_eq!(config.max_tool_iterations, 30);
    }

    #[test]
    fn test_load_from_toml_without_runtime() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let content = r#"
[provider]
base_url = "https://test.test/v1"
model = "test-model"
api_key = "sk-test"
"#;
        let path = write_keys_toml(&dir, content);

        let config = load_config(Some(&path)).expect("load from toml");
        assert_eq!(config.max_tool_iterations, 50); // default
    }

    #[test]
    fn test_load_from_env() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nonexistent = dir.path().join("nonexistent.toml");

        let result = with_env_vars(
            &[
                ("DOGMA_BASE_URL", "https://env-test.test/v1"),
                ("DOGMA_MODEL", "env-model"),
                ("DOGMA_API_KEY", "sk-env-key"),
                ("DOGMA_HOME", dir.path().to_str().unwrap()),
            ],
            || load_config(Some(&nonexistent)),
        );

        let config = result.expect("load from env");
        assert_eq!(config.provider.base_url, "https://env-test.test/v1");
        assert_eq!(config.provider.model, "env-model");
        assert_eq!(config.provider.api_key, Some("sk-env-key".into()));
        assert_eq!(config.max_tool_iterations, 50); // default
    }

    #[test]
    fn test_load_fails_when_none_found() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nonexistent = dir.path().join("no-file-here.toml");

        let old_base = std::env::var("DOGMA_BASE_URL").ok();
        let old_model = std::env::var("DOGMA_MODEL").ok();
        let old_key = std::env::var("DOGMA_API_KEY").ok();
        let old_home = std::env::var("DOGMA_HOME").ok();

        if old_base.is_some() {
            unsafe {
                std::env::remove_var("DOGMA_BASE_URL");
            }
        }
        if old_model.is_some() {
            unsafe {
                std::env::remove_var("DOGMA_MODEL");
            }
        }
        if old_key.is_some() {
            unsafe {
                std::env::remove_var("DOGMA_API_KEY");
            }
        }
        unsafe {
            std::env::set_var("DOGMA_HOME", dir.path());
        }

        let err = load_config(Some(&nonexistent)).expect_err("should fail");

        if let Some(v) = old_base {
            unsafe {
                std::env::set_var("DOGMA_BASE_URL", &v);
            }
        }
        if let Some(v) = old_model {
            unsafe {
                std::env::set_var("DOGMA_MODEL", &v);
            }
        }
        if let Some(v) = old_key {
            unsafe {
                std::env::set_var("DOGMA_API_KEY", &v);
            }
        }
        match old_home {
            Some(v) => unsafe {
                std::env::set_var("DOGMA_HOME", &v);
            },
            None => unsafe {
                std::env::remove_var("DOGMA_HOME");
            },
        }

        assert!(err.contains("No provider configuration found"));
    }

    #[test]
    fn test_load_fails_with_partial_env() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nonexistent = dir.path().join("nonexistent.toml");

        let result = with_env_vars(
            &[
                ("DOGMA_BASE_URL", "https://partial.test/v1"),
                ("DOGMA_HOME", dir.path().to_str().unwrap()),
            ],
            || load_config(Some(&nonexistent)),
        );

        let err = result.expect_err("should fail with partial env");
        assert!(err.contains("Missing"));
        assert!(err.contains("DOGMA_MODEL"));
        assert!(err.contains("DOGMA_API_KEY"));
    }

    #[test]
    fn test_toml_file_takes_precedence_over_env() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let content = r#"
[provider]
base_url = "https://toml-wins.test/v1"
model = "toml-model"
api_key = "sk-toml-key"

[runtime]
max_tool_iterations = 42
"#;
        let path = write_keys_toml(&dir, content);

        let config = with_env_vars(
            &[
                ("DOGMA_BASE_URL", "https://env-wrong.test/v1"),
                ("DOGMA_MODEL", "env-model"),
                ("DOGMA_API_KEY", "sk-env-key"),
                ("DOGMA_HOME", dir.path().to_str().unwrap()),
            ],
            || load_config(Some(&path)),
        );

        let config = config.expect("load from toml");
        assert_eq!(config.provider.base_url, "https://toml-wins.test/v1");
        assert_eq!(config.provider.model, "toml-model");
        assert_eq!(config.provider.api_key, Some("sk-toml-key".into()));
        assert_eq!(config.max_tool_iterations, 42);
    }
}
