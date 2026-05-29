//! # Config — Carga de configuración del proveedor LLM
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
//! name = "big-pickle"
//! base_url = "https://opencode.ai/zen/v1"
//! model = "@ai-sdk/openai-compatible"
//! api_key = "sk-..."
//! ```

use std::path::Path;

use dogma_v2_core::runtime::provider::ProviderConfig;
use serde::Deserialize;

/// Estructura que refleja el archivo keys.toml.
#[derive(Debug, Deserialize)]
struct KeysToml {
    provider: ProviderSection,
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

/// Carga la configuración del proveedor LLM.
///
/// Orden de precedencia:
/// 1. `keys.toml` en la ruta indicada (o `./keys.toml` por defecto).
/// 2. Variables de entorno: `DOGMA_BASE_URL`, `DOGMA_MODEL`, `DOGMA_API_KEY`.
///
/// # Errors
///
/// Devuelve un mensaje descriptivo si no se encuentra configuración en
/// ninguna fuente, o si la encontrada está incompleta.
pub fn load_provider_config(keys_path: Option<&Path>) -> Result<ProviderConfig, String> {
    // ── Intento 1: keys.toml ─────────────────────────────────────────
    let path = keys_path.unwrap_or_else(|| Path::new("keys.toml"));
    if path.exists() {
        match std::fs::read_to_string(path) {
            Ok(content) => match toml::from_str::<KeysToml>(&content) {
                Ok(toml_config) => {
                    return Ok(ProviderConfig {
                        base_url: toml_config.provider.base_url,
                        model: toml_config.provider.model,
                        api_key: Some(toml_config.provider.api_key),
                        ..Default::default()
                    });
                }
                Err(e) => {
                    tracing::warn!("config: failed to parse {}: {e}", path.display());
                }
            },
            Err(e) => {
                tracing::warn!("config: failed to read {}: {e}", path.display());
            }
        }
    }

    // ── Intento 2: variables de entorno ──────────────────────────────
    let base_url = std::env::var("DOGMA_BASE_URL").ok();
    let model = std::env::var("DOGMA_MODEL").ok();
    let api_key = std::env::var("DOGMA_API_KEY").ok();

    if let (Some(url), Some(mdl), Some(key)) = (&base_url, &model, &api_key) {
        return Ok(ProviderConfig {
            base_url: url.clone(),
            model: mdl.clone(),
            api_key: Some(key.clone()),
            ..Default::default()
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
         \nOr set environment variables:\n  DOGMA_BASE_URL\n  DOGMA_MODEL\n  DOGMA_API_KEY"
        .to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // ── Helper: ejecuta un closure con vars de entorno aisladas ──────
    //     Previene race conditions con otros tests que modifican env.
    fn with_env_vars<R>(kvs: &[(&str, &str)], f: impl FnOnce() -> R) -> R {
        // Salvar valores originales
        let originals: Vec<(&str, Option<String>)> = kvs
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();

        // Aplicar nuevos valores
        for &(k, v) in kvs {
            unsafe {
                std::env::set_var(k, v);
            }
        }

        let result = f();

        // Restaurar valores originales
        for (k, original) in &originals {
            match original {
                Some(val) => unsafe {
                    std::env::set_var(k, val);
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
"#;
        let path = write_keys_toml(&dir, content);

        let config = load_provider_config(Some(&path)).expect("load from toml");
        assert_eq!(config.base_url, "https://opencode.ai/zen/v1");
        assert_eq!(config.model, "@ai-sdk/openai-compatible");
        assert_eq!(config.api_key, Some("sk-test-key-for-toml".into()));
        assert!((config.temperature - 0.7).abs() < f32::EPSILON);
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
            ],
            || load_provider_config(Some(&nonexistent)),
        );

        let config = result.expect("load from env");
        assert_eq!(config.base_url, "https://env-test.test/v1");
        assert_eq!(config.model, "env-model");
        assert_eq!(config.api_key, Some("sk-env-key".into()));
    }

    #[test]
    fn test_load_fails_when_none_found() {
        // Este test debe ejecutarse sin vars DOGMA_* en el entorno.
        // El helper with_env_vars NO debe llamarse aquí ya que no
        // queremos setear ninguna var. Simplemente nos aseguramos
        // de que las vars existentes no interfieran limpiándolas
        // y restaurándolas.
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nonexistent = dir.path().join("no-file-here.toml");

        // Si el entorno exterior tiene DOGMA_* (de otro test en
        // paralelo), las limpiamos temporalmente
        let old_base = std::env::var("DOGMA_BASE_URL").ok();
        let old_model = std::env::var("DOGMA_MODEL").ok();
        let old_key = std::env::var("DOGMA_API_KEY").ok();

        // Limpiar (con unsafe en edition 2024)
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

        let err = load_provider_config(Some(&nonexistent)).expect_err("should fail");

        // Restaurar
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

        assert!(err.contains("No provider configuration found"));
    }

    #[test]
    fn test_load_fails_with_partial_env() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        let nonexistent = dir.path().join("nonexistent.toml");

        let result = with_env_vars(&[("DOGMA_BASE_URL", "https://partial.test/v1")], || {
            load_provider_config(Some(&nonexistent))
        });

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
"#;
        let path = write_keys_toml(&dir, content);

        let config = with_env_vars(
            &[
                ("DOGMA_BASE_URL", "https://env-wrong.test/v1"),
                ("DOGMA_MODEL", "env-model"),
                ("DOGMA_API_KEY", "sk-env-key"),
            ],
            || load_provider_config(Some(&path)),
        );

        let config = config.expect("load from toml");
        assert_eq!(config.base_url, "https://toml-wins.test/v1");
        assert_eq!(config.model, "toml-model");
        assert_eq!(config.api_key, Some("sk-toml-key".into()));
    }
}
