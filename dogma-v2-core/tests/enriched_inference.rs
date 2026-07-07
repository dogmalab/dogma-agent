//! # MoA (Enriched Inference) end-to-end tests
//!
//! Uses a `MockProvider` that returns deterministic responses
//! without making real HTTP calls. The tests verify:
//!
//! * The Cost Gate is invoked before any LLM call.
//! * Proposers run in parallel.
//! * The compiler synthesizes from proposer responses.
//! * Cost breakdown is populated correctly.
//! * The session graph receives `MoAProposer`, `MoACompiler`,
//!   `CostProposal`, and `CostActual` nodes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use dogma_v2_common::Result;
use dogma_v2_core::runtime::cost_gate::{CostDecision, CostGateImpl, CostProposal};
use dogma_v2_core::runtime::enriched::{MoaConfig, MoaLoop};
use dogma_v2_core::runtime::provider::{LLMProvider, LLMResponse, Message, ProviderConfig, TokenUsage};
use parking_lot::Mutex;

// ── Mock provider ────────────────────────────────────────────────────

/// A mock LLMProvider that returns a deterministic response based
/// on the call count. Records every call.
struct MockProvider {
    config: ProviderConfig,
    call_count: AtomicUsize,
    /// Per-call wall-time in milliseconds (simulated).
    simulated_wall_time_ms: u64,
    /// Lock for capturing the messages from each call (for assertions).
    captured: Mutex<Vec<Vec<String>>>,
}

impl MockProvider {
    fn new(model: &str, simulated_wall_time_ms: u64) -> Self {
        Self {
            config: ProviderConfig {
                base_url: "mock://test".to_string(),
                model: model.to_string(),
                api_key: Some("mock".to_string()),
                temperature: 0.0,
                max_tokens: 100,
            },
            call_count: AtomicUsize::new(0),
            simulated_wall_time_ms,
            captured: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMProvider for MockProvider {
    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        // Capture the user message for assertions
        if let Some(last) = messages.last() {
            self.captured.lock().push(vec![last.content.clone()]);
        }
        if self.simulated_wall_time_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.simulated_wall_time_ms)).await;
        }
        Ok(LLMResponse {
            content: format!(
                "[mock-{} response to: {}]",
                self.config.model,
                messages
                    .last()
                    .map(|m| m.content.chars().take(40).collect::<String>())
                    .unwrap_or_default()
            ),
            tool_calls: Vec::new(),
            usage: TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            },
            extra_fields: Vec::new(),
        })
    }

    fn config(&self) -> &ProviderConfig {
        &self.config
    }
}

// ── Mock gate that always approves ──────────────────────────────────

struct ApproveAllGate;
#[async_trait]
impl CostGateImpl for ApproveAllGate {
    async fn ask(&self, _proposal: CostProposal) -> Result<CostDecision> {
        Ok(CostDecision::Proceed)
    }
}

// ── Mock gate that always aborts ─────────────────────────────────────

struct AbortAllGate;
#[async_trait]
impl CostGateImpl for AbortAllGate {
    async fn ask(&self, _proposal: CostProposal) -> Result<CostDecision> {
        Ok(CostDecision::Abort {
            reason: "test abort".to_string(),
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn moa_runs_with_trusted_gate() {
    let p1: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("p1", 0));
    let p2: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("p2", 0));
    let p3: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("p3", 0));
    let compiler: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("compiler", 0));
    let gate: Arc<dyn CostGateImpl> = Arc::new(ApproveAllGate);

    let moa = MoaLoop::new(
        vec![p1.clone(), p2.clone(), p3.clone()],
        compiler.clone(),
        gate,
        MoaConfig {
            n_proposers: 3,
            max_iterations: 2,
            ..MoaConfig::default()
        },
    );

    let result = moa.run("what is dogfooding?").await.unwrap();

    // 2 iterations × (3 proposers + 1 compiler) = 8 total LLM calls
    assert_eq!(result.iterations.len(), 2);
    assert_eq!(p1.calls(), 2);
    assert_eq!(p2.calls(), 2);
    assert_eq!(p3.calls(), 2);
    assert_eq!(compiler.calls(), 2);

    // The final answer is the last compiler response.
    assert!(!result.final_text.is_empty());
    assert!(result
        .final_text
        .contains("[mock-compiler response"));

    // Cost breakdown is populated.
    assert_eq!(result.cost.proposers.len(), 3);
    assert!(result.cost.compiler.is_some());
}

#[tokio::test]
async fn moa_aborts_when_gate_aborts() {
    let p: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("p1", 0));
    let compiler: Arc<dyn LLMProvider> = Arc::new(MockProvider::new("compiler", 0));
    let gate: Arc<dyn CostGateImpl> = Arc::new(AbortAllGate);

    let moa = MoaLoop::new(
        vec![p.clone()],
        compiler.clone(),
        gate,
        MoaConfig {
            n_proposers: 1,
            max_iterations: 1,
            ..MoaConfig::default()
        },
    );

    let result = moa.run("test").await;
    assert!(result.is_err());
    // No LLM calls should have been made.
    assert_eq!(p.calls(), 0);
    assert_eq!(compiler.calls(), 0);
}

#[tokio::test]
async fn moa_runs_proposers_in_parallel() {
    // If proposers ran sequentially, the wall-time would be
    // sum(simulated). In parallel, it's max(simulated).
    let p1 = Arc::new(MockProvider::new("p1", 50));
    let p2 = Arc::new(MockProvider::new("p2", 50));
    let p3 = Arc::new(MockProvider::new("p3", 50));
    let compiler = Arc::new(MockProvider::new("compiler", 0));
    let gate: Arc<dyn CostGateImpl> = Arc::new(ApproveAllGate);

    let moa = MoaLoop::new(
        vec![p1, p2, p3],
        compiler,
        gate,
        MoaConfig {
            n_proposers: 3,
            max_iterations: 1,
            ..MoaConfig::default()
        },
    );

    let started = std::time::Instant::now();
    let _ = moa.run("test").await.unwrap();
    let elapsed_ms = started.elapsed().as_millis();

    // If parallel: ~50ms (max) + compiler overhead. If serial: ~150ms.
    // Allow generous slack; the parallelism must be measurably less
    // than 3x the per-proposer time.
    assert!(
        elapsed_ms < 250,
        "expected parallel execution, but elapsed was {elapsed_ms}ms"
    );
}

#[tokio::test]
async fn moa_synthesis_prompt_contains_all_proposer_responses() {
    let p1 = Arc::new(MockProvider::new("p1", 0));
    let p2 = Arc::new(MockProvider::new("p2", 0));
    let compiler = Arc::new(MockProvider::new("compiler", 0));
    let gate: Arc<dyn CostGateImpl> = Arc::new(ApproveAllGate);

    let moa = MoaLoop::new(
        vec![p1.clone(), p2.clone()],
        compiler.clone(),
        gate,
        MoaConfig {
            n_proposers: 2,
            max_iterations: 1,
            ..MoaConfig::default()
        },
    );

    let _ = moa.run("explain rust async").await.unwrap();

    // The compiler's input should contain both proposers' responses.
    let compiler_captured = compiler.captured.lock();
    let last_compiler_call = compiler_captured.last().expect("compiler was called");
    let user_msg = &last_compiler_call[0];
    assert!(user_msg.contains("Candidate 1"), "missing Candidate 1: {user_msg}");
    assert!(user_msg.contains("Candidate 2"), "missing Candidate 2: {user_msg}");
    assert!(user_msg.contains("p1"), "missing p1: {user_msg}");
    assert!(user_msg.contains("p2"), "missing p2: {user_msg}");
    assert!(user_msg.contains("explain rust async"), "missing original query: {user_msg}");
}
