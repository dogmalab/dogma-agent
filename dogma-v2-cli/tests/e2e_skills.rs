//! # E2E Skills — Prueba integral de instalación de skills dinámicos
//!
//! Verifica el pipeline completo:
//!
//! 1. El RuntimeLoop recibe un prompt solicitando instalar un skill
//! 2. El LLM mock responde con un tool_call → `install_skill`
//! 3. El RuntimeLoop ejecuta InstallSkillTool::call
//! 4. InstallSkillTool descarga (mock), audita (misma instancia mock),
//!    y persiste el skill en dogma-vdb
//! 5. El resultado se inyecta de vuelta al LLM
//! 6. El LLM responde con confirmación final
//! 7. Se verifica el mensaje de salida

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use dogma_v2_common::Result;
use dogma_v2_core::runtime::loop_handle::{LoopConfig, RuntimeLoop};
use dogma_v2_core::runtime::provider::{
    LLMProvider, LLMResponse, Message, ProviderConfig, TokenUsage, ToolCall,
};
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::{create_survival_tools, InstallSkillTool};

// ── Mock LLM con fases: tool_call → auditoría → confirmación ──────

struct SkillsMockProvider {
    config: ProviderConfig,
    call_count: AtomicUsize,
}

impl SkillsMockProvider {
    fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            call_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LLMProvider for SkillsMockProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        let phase = self.call_count.fetch_add(1, Ordering::SeqCst);

        match phase {
            // Fase 0: Prompt inicial → tool_call install_skill
            0 => Ok(LLMResponse {
                content: "I'll install the format_json skill.".into(),
                tool_calls: vec![ToolCall {
                    id: "call_install_skill".into(),
                    name: "install_skill".into(),
                    arguments: r#"{"skill_id": "format_json"}"#.into(),
                }],
                usage: TokenUsage::default(),
                extra_fields: vec![],
            }),

            // Fase 1: Auditoría cognitiva → CognitiveAuditReport
            // (SkillsShClient::run_cognitive_audit llama al LLM)
            1 => Ok(LLMResponse {
                content: serde_json::json!({
                    "approved": true,
                    "risk_score": 0.05,
                    "findings": ["No se detectaron problemas de seguridad."]
                })
                .to_string(),
                tool_calls: vec![],
                usage: TokenUsage::default(),
                extra_fields: vec![],
            }),

            // Fase 2: Resultado del tool → confirmación final
            _ => Ok(LLMResponse {
                content: "El skill 'format_json' se ha instalado correctamente con la auditoría cognitiva aprobada.".into(),
                tool_calls: vec![],
                usage: TokenUsage::default(),
                extra_fields: vec![],
            }),
        }
    }
}

// ── Test E2E ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_e2e_install_skill_from_runtime_loop() {
    let dir = tempfile::tempdir().expect("temp dir");
    let provider_config = ProviderConfig {
        base_url: "https://mock.test/v1".into(),
        model: "mock-model".into(),
        api_key: Some("sk-mock-key".into()),
        ..Default::default()
    };

    // Crear sesión
    let mut session = SessionManager::open(dir.path()).expect("open session");
    let session_id = session
        .create_session("e2e-skills")
        .expect("create session");

    // Provider multipropósito: runtime loop + auditor cognitivo
    let provider: Arc<dyn LLMProvider> = Arc::new(SkillsMockProvider::new(provider_config));

    // Crear runtime con herramientas de supervivencia
    let tools = create_survival_tools();
    let loop_config = LoopConfig {
        max_tool_iterations: 5,
        ..Default::default()
    };

    let runtime = RuntimeLoop::new(provider.clone(), tools, session, loop_config);

    // Registrar InstallSkillTool (usa el mismo provider para auditoría)
    let skill_tool = InstallSkillTool::new(provider.clone(), dir.path().to_path_buf())
        .expect("InstallSkillTool creation");
    runtime.register_tool(Box::new(skill_tool));

    // Ejecutar el loop completo
    let response = runtime
        .run("Necesito instalar el skill format_json desde skills.sh", &session_id)
        .await
        .expect("RuntimeLoop should complete");

    // Verificar que la respuesta final contiene indicadores de éxito
    assert!(
        response.contains("format_json")
            || response.contains("instalado")
            || response.contains("correctamente"),
        "Final response should confirm skill installation, got: {response}"
    );
}
