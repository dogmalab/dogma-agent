//! # Tests E2E — Verificación integral del pipeline CLI + Runtime
//!
//! Estos tests validan que todos los componentes del sistema funcionen
//! juntos: carga de configuración, inicialización de sesión, tools,
//! y el RuntimeLoop completo con un proveedor mock (sin red real).

use std::sync::Arc;

use async_trait::async_trait;
use dogma_v2_common::Result;
use dogma_v2_core::RuntimeLoop;
use dogma_v2_core::runtime::loop_handle::LoopConfig;
use dogma_v2_core::runtime::provider::{
    LLMProvider, LLMResponse, Message, ProviderConfig, TokenUsage,
};
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::create_survival_tools;

// ---------------------------------------------------------------------------
// Mock LLM Provider — respuestas prefabricadas, sin red
// ---------------------------------------------------------------------------

/// Proveedor mock que devuelve respuestas prefabricadas.
///
/// Usado en tests E2E para validar el pipeline completo sin depender
/// de conectividad externa.
struct MockLLMProvider {
    config: ProviderConfig,
    response: String,
}

impl MockLLMProvider {
    fn new(config: ProviderConfig, response: impl Into<String>) -> Self {
        Self {
            config,
            response: response.into(),
        }
    }
}

#[async_trait]
impl LLMProvider for MockLLMProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        Ok(LLMResponse {
            content: self.response.clone(),
            tool_calls: vec![],
            usage: TokenUsage::default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_provider_config() -> ProviderConfig {
    ProviderConfig {
        base_url: "https://mock.test/v1".into(),
        model: "mock-model".into(),
        api_key: Some("sk-mock-key".into()),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Tests E2E
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_runtime_loop_returns_mock_response() {
    let dir = tempfile::tempdir().expect("temp dir");

    // Crear sesión antes de pasar ownership al RuntimeLoop
    let mut session = SessionManager::open(dir.path()).expect("open session");
    let session_id = session.create_session("e2e-test").expect("create session");

    let provider = Arc::new(MockLLMProvider::new(
        test_provider_config(),
        "E2E test response — no tool calls",
    ));
    let tools = create_survival_tools();
    let loop_config = LoopConfig::default();

    let runtime = RuntimeLoop::new(provider, tools, session, loop_config);

    let response = runtime
        .run("Hello from E2E test!", &session_id)
        .await
        .expect("runtime loop should succeed");

    assert_eq!(response, "E2E test response — no tool calls");
}

#[tokio::test]
async fn test_runtime_loop_can_be_called_multiple_times() {
    let dir = tempfile::tempdir().expect("temp dir");

    let mut session = SessionManager::open(dir.path()).expect("open session");
    let session_id = session
        .create_session("multi-call")
        .expect("create session");

    let provider = Arc::new(MockLLMProvider::new(
        test_provider_config(),
        "multi-call response",
    ));
    let tools = create_survival_tools();
    let loop_config = LoopConfig {
        max_tool_iterations: 3,
        ..Default::default()
    };

    let runtime = RuntimeLoop::new(provider, tools, session, loop_config);

    // Primera llamada
    let r1 = runtime
        .run("First prompt", &session_id)
        .await
        .expect("first call");
    assert_eq!(r1, "multi-call response");

    // Segunda llamada (mismo runtime, mismo session_id)
    let r2 = runtime
        .run("Second prompt", &session_id)
        .await
        .expect("second call");
    assert_eq!(r2, "multi-call response");
}

#[tokio::test]
async fn test_session_is_persisted_after_runtime_loop() {
    // NOTA: la verificación de nodos persistidos está limitada por
    // dogma-vdb (get_session_nodes devuelve vacío hasta que se
    // implemente metadata filtering). Este test verifica que el
    // RuntimeLoop no falle al escribir en la sesión y que podamos
    // re-abrir el archivo sin errores.
    let dir = tempfile::tempdir().expect("temp dir");

    let mut session = SessionManager::open(dir.path()).expect("open session");
    let session_id = session
        .create_session("persist-test")
        .expect("create session");

    let provider = Arc::new(MockLLMProvider::new(
        test_provider_config(),
        "check persistence",
    ));
    let tools = create_survival_tools();
    let loop_config = LoopConfig::default();

    let runtime = RuntimeLoop::new(provider, tools, session, loop_config);
    runtime
        .run("Test persistence", &session_id)
        .await
        .expect("runtime loop");

    // Re-abrir la sesión no debe fallar — el archivo vdb existe y
    // contiene los datos escritos por RuntimeLoop.
    let _session_reopened =
        SessionManager::open(dir.path()).expect("reopen session without errors");
}

#[tokio::test]
async fn test_runtime_loop_with_max_iterations() {
    // Verificar que el límite de iteraciones funciona: si el mock
    // devuelve tool_calls, el loop se detiene tras N iteraciones.
    let dir = tempfile::tempdir().expect("temp dir");

    let mut session = SessionManager::open(dir.path()).expect("open session");
    let session_id = session
        .create_session("max-iter-test")
        .expect("create session");

    // Provider que devuelve tool_calls para forzar iteraciones
    let provider = Arc::new(ToolCallMockProvider::new(test_provider_config()));
    let tools = create_survival_tools();
    let loop_config = LoopConfig {
        max_tool_iterations: 2,
        ..Default::default()
    };

    let runtime = RuntimeLoop::new(provider, tools, session, loop_config);
    let response = runtime
        .run("Trigger tool calls", &session_id)
        .await
        .expect("runtime loop should not crash on max iterations");

    assert!(
        response.contains("Max iterations"),
        "Response should mention iteration limit: {response}"
    );
}

// ---------------------------------------------------------------------------
// Mock que siempre devuelve tool_calls (para probar límite de iteraciones)
// ---------------------------------------------------------------------------

struct ToolCallMockProvider {
    config: ProviderConfig,
}

impl ToolCallMockProvider {
    fn new(config: ProviderConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl LLMProvider for ToolCallMockProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat(
        &self,
        _messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        Ok(LLMResponse {
            content: "I need to use tools.".into(),
            tool_calls: vec![dogma_v2_core::runtime::provider::ToolCall {
                id: "call_mock_1".into(),
                name: "read_file".into(),
                arguments: r#"{"path": "/tmp/test.txt"}"#.into(),
            }],
            usage: TokenUsage::default(),
        })
    }
}
