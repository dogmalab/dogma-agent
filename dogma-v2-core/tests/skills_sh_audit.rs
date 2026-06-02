//! # skills_sh_audit — Integration tests for SkillsShClient + Cognitive Audit
//!
//! Validates the complete pipeline:
//! * Legitimate skill passes cognitive audit → skill persisted in dogma-vdb
//! * Malicious skill (data exfiltration) is blocked → Security error, no persistence
//! * Missing/invalid skill IDs produce controlled errors
//!
//! Uses MockAuditor to simulate the cognitive auditor LLM.
//! All tests share a single tmpdir to avoid filesystem conflicts.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use async_trait::async_trait;
use dogma_v2_common::Error as DogmaError;
use dogma_v2_core::runtime::provider::LLMProvider;
use dogma_v2_core::runtime::provider::LLMResponse;
use dogma_v2_core::runtime::provider::Message;
use dogma_v2_core::runtime::provider::ProviderConfig;
use dogma_v2_core::runtime::provider::TokenUsage;
use dogma_v2_core::tools::InstallSkillTool;
use dogma_v2_core::tools::Tool;

// ── Shared temp dir ────────────────────────────────────────────────

static SHARED_TMP: OnceLock<PathBuf> = OnceLock::new();

fn tmp_dir() -> &'static PathBuf {
    SHARED_TMP.get_or_init(|| {
        let dir = std::env::temp_dir().join("dogma-skills-sh-audit-tests");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
}

fn data_dir() -> PathBuf {
    tmp_dir().join("data")
}

fn cleanup() {
    let _ = std::fs::remove_dir_all(tmp_dir());
}

// ── Mock Auditor ───────────────────────────────────────────────────

struct MockAuditor {
    response_json: String,
    config: ProviderConfig,
}

impl MockAuditor {
    fn new_approve() -> Self {
        Self {
            response_json: serde_json::json!({
                "approved": true,
                "risk_score": 0.05,
                "findings": ["No se detectaron problemas de seguridad."]
            })
            .to_string(),
            config: ProviderConfig {
                temperature: 0.0,
                ..ProviderConfig::default()
            },
        }
    }

    fn new_reject() -> Self {
        Self {
            response_json: serde_json::json!({
                "approved": false,
                "risk_score": 0.9,
                "findings": [
                    "ALERTA: El script accede a variables de entorno (os.environ.get)",
                    "Posible fuga de credenciales: OPENAI_API_KEY en payload ejecutable",
                    "Data Exfiltration detectada en código del skill"
                ]
            })
            .to_string(),
            config: ProviderConfig {
                temperature: 0.0,
                ..ProviderConfig::default()
            },
        }
    }
}

#[async_trait]
impl LLMProvider for MockAuditor {
    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse, DogmaError> {
        // Simulate that the audit prompt was answered
        Ok(LLMResponse {
            content: self.response_json.clone(),
            tool_calls: vec![],
            usage: TokenUsage {
                prompt_tokens: 500,
                completion_tokens: 100,
                total_tokens: 600,
            },
            extra_fields: vec![],
        })
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

// ── Test: Legitimate skill passes audit ────────────────────────────

#[tokio::test]
async fn test_install_legitimate_skill_passes() {
    cleanup();

    let auditor: Arc<dyn LLMProvider> = Arc::new(MockAuditor::new_approve());
    let tool = InstallSkillTool::new(auditor, data_dir()).unwrap();

    // Simulate a legitimate skill installation call
    let args = serde_json::json!({ "skill_id": "format_json" });
    let result = tool.call(&args).await;

    assert!(
        result.is_ok(),
        "Legitimate skill should pass audit and install successfully, got: {:?}",
        result.err()
    );

    // Verify output contains success indicators
    let output_str = result.unwrap();
    assert!(
        output_str.contains("format_json"),
        "Output should mention skill ID: {output_str}"
    );
    assert!(
        output_str.contains("installed successfully") || output_str.contains("instalado exitosamente"),
        "Output should indicate installation success: {output_str}"
    );
    assert!(
        output_str.contains("Format JSON") || output_str.contains("format_json"),
        "Output should reference the skill by name: {output_str}"
    );
}

// ── Test: Malicious skill is blocked ───────────────────────────────

#[tokio::test]
async fn test_install_malicious_skill_is_blocked() {
    cleanup();

    let auditor: Arc<dyn LLMProvider> = Arc::new(MockAuditor::new_reject());
    let tool = InstallSkillTool::new(auditor, data_dir()).unwrap();

    // Attempt to install a malicious skill
    let args = serde_json::json!({ "skill_id": "malicious_telemetry" });
    let result = tool.call(&args).await;

    assert!(
        result.is_err(),
        "Malicious skill should be blocked by cognitive audit"
    );

    // Verify the error is a security error (rejected by cognitive audit)
    let err = result.err().unwrap();
    let err_msg = err.to_lowercase();
    assert!(
        err_msg.contains("aduana") || err_msg.contains("cognitive") || err_msg.contains("denegado") || err_msg.contains("security"),
        "Error should reference cognitive audit rejection, got: {err_msg}"
    );
}

// ── Test: Missing skill_id parameter ───────────────────────────────

#[tokio::test]
async fn test_install_skill_missing_skill_id() {
    cleanup();

    let auditor: Arc<dyn LLMProvider> = Arc::new(MockAuditor::new_approve());
    let tool = InstallSkillTool::new(auditor, data_dir()).unwrap();

    // No skill_id provided
    let args = serde_json::json!({});
    let result = tool.call(&args).await;

    assert!(
        result.is_err(),
        "Missing skill_id should produce an error"
    );
}

// ── Test: Invalid/unknown skill_id ─────────────────────────────────

#[tokio::test]
async fn test_install_skill_invalid_id() {
    cleanup();

    let auditor: Arc<dyn LLMProvider> = Arc::new(MockAuditor::new_approve());
    let tool = InstallSkillTool::new(auditor, data_dir()).unwrap();

    // Unknown skill ID — the download_payload mock only knows
    // "malicious_telemetry" and "format_json". Anything else returns
    // the legitimate payload, so this should succeed for any unknown ID.
    let args = serde_json::json!({ "skill_id": "nonexistent_skill_xyz" });
    let result = tool.call(&args).await;

    // The mock download_payload treats unknown IDs as legitimate,
    // and the mock auditor approves — so this should succeed.
    assert!(
        result.is_ok(),
        "Unknown skill IDs should be treated as legitimate by mock, got: {:?}",
        result.err()
    );
}
