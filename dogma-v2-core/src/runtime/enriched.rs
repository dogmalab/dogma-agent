//! # Enriched Inference (IE) — The flagship pattern of the agent harness
//!
//! IE is a Mixture-of-Agents (MoA) orchestrator: N LLMs run in parallel
//! as proposers, a compiler model synthesizes their responses, the
//! synthesis is iterated. The pattern is gated by the
//! [`CostGate`](super::cost_gate::CostGate) so the user always sees
//! the cost and confirms before any tokens are spent.
//!
//! ## The loop
//!
//! ```text
//! query
//!   → CostProposal → user confirms (Cost Gate)
//!   →
//!     [Phase 0] context preparation (RAG via SessionManager)
//!     [Phase 1] N proposers in parallel → Vec<MoAResponse>
//!     [Phase 2] compiler synthesizes → MoAResponse (iteration 1)
//!     [Phase 2'] if iter > 1: proposers refine based on synthesis
//!     [Phase 2''] compiler synthesizes again → MoAResponse (iteration 2)
//!     [Phase 3] optional: skills verify the answer
//!   → MoAResult
//! ```
//!
//! ## Why this matters
//!
//! Each proposer sees the same query + context. The compiler sees
//! all three responses and produces a synthesis. Proposers do NOT
//! see each other's responses (to avoid groupthink); they only see
//! the synthesis at iteration N+1 (to refine). This is the canonical
//! MoA pattern from the literature, adapted to the agent harness's
//! trait-based provider abstraction.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::cost_estimator::{
    pricing_for, CostBreakdown, CostCalculable, CostEstimate, ModelPricing, ProviderCost,
    ProviderRole,
};
use super::cost_gate::{CostDecision, CostGateImpl, CostProposal};
use super::quality_estimator::{
    HeuristicQualityEstimator, QualityBasis, QualityCalculable, QualityEstimate,
};
use super::provider::{LLMProvider, LLMResponse, Message, MessageRole, ProviderConfig};
use crate::state::session::SessionManager;
use dogma_v2_common::Result;
use dogma_v2_common::error::Error as DogmaError;
use dogma_vdb::doc::Document;

// =========================================================================
// Configuration
// =========================================================================

/// Configuration of an Enriched Inference run.
#[derive(Debug, Clone)]
pub struct MoaConfig {
    /// Number of proposer LLMs that run in parallel. Default: 3.
    pub n_proposers: usize,
    /// Number of compiler iterations. Default: 2.
    pub max_iterations: usize,
    /// The model used for the compiler step. If None, the
    /// strongest proposer is used.
    pub compiler: Option<CompilerConfig>,
    /// Whether to invoke skills that can verify the final answer.
    pub enable_verification_skills: bool,
    /// Maximum wall-time for the entire run, in milliseconds.
    pub max_wall_time_ms: u64,
    /// Maximum input tokens per proposer call.
    pub max_input_tokens_per_proposer: u32,
    /// Maximum output tokens per proposer call.
    pub max_output_tokens_per_proposer: u32,
}

impl Default for MoaConfig {
    fn default() -> Self {
        Self {
            n_proposers: 3,
            max_iterations: 2,
            compiler: None,
            enable_verification_skills: false,
            max_wall_time_ms: 60_000,
            max_input_tokens_per_proposer: 4096,
            max_output_tokens_per_proposer: 2048,
        }
    }
}

/// Configuration of the compiler LLM.
#[derive(Debug, Clone)]
pub struct CompilerConfig {
    pub provider: Arc<dyn LLMProvider>,
    pub pricing: Option<ModelPricing>,
}

impl CompilerConfig {
    /// Builds a compiler config from an LLMProvider, auto-detecting
    /// pricing from the model name.
    pub fn from_provider(provider: Arc<dyn LLMProvider>) -> Self {
        let model = provider.config().model.clone();
        Self {
            provider,
            pricing: pricing_for(&model),
        }
    }
}

// =========================================================================
// Cost + Quality traits (impls)
// =========================================================================

impl CostCalculable for MoaConfig {
    fn estimate_cost(&self) -> CostEstimate {
        // A conservative estimate: assume each proposer produces
        // `max_output_tokens_per_proposer` output tokens and consumes
        // `max_input_tokens_per_proposer` input tokens, with the
        // compiler doing the same per iteration.
        let input_per_call = u64::from(self.max_input_tokens_per_proposer);
        let output_per_call = u64::from(self.max_output_tokens_per_proposer);
        let n_calls = (self.n_proposers as u64) * (self.max_iterations as u64)
            + (self.max_iterations as u64); // compiler per iter
        let total_input = input_per_call * n_calls;
        let total_output = output_per_call * n_calls;

        // Conservative wall-time estimate: 2s per call, sequentially.
        let wall_time_ms = n_calls * 2000;

        CostEstimate {
            min_cost_usd: 0.0,
            expected_cost_usd: 0.0, // refined when we know the providers
            max_cost_usd: 0.0,
            input_tokens: total_input,
            output_tokens: total_output,
            wall_time_ms,
        }
    }
}

// =========================================================================
// Result types
// =========================================================================

/// The response of one LLM call in the MoA loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoaResponse {
    pub model: String,
    pub role: ProviderRole,
    pub iteration: usize,
    pub content: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub wall_time_ms: u64,
    pub estimated_cost_usd: f64,
}

/// One iteration of the MoA loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoaIteration {
    pub iteration: usize,
    pub proposer_responses: Vec<MoaResponse>,
    pub compiled_response: MoaResponse,
    pub skills_invoked: Vec<String>,
}

/// The final result of an Enriched Inference run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoaResult {
    pub final_text: String,
    pub iterations: Vec<MoaIteration>,
    pub cost: CostBreakdown,
    pub quality: QualityEstimate,
    pub total_wall_time_ms: u64,
}

// =========================================================================
// The orchestrator
// =========================================================================

/// The Enriched Inference orchestrator.
///
/// Holds the proposers, the compiler, the session manager, and the
/// Cost Gate. Each call to `run()` is one Enriched Inference
/// execution: it estimates cost, asks the gate, runs the loop,
/// persists every step in the session graph.
pub struct MoaLoop {
    pub config: MoaConfig,
    pub proposers: Vec<Arc<dyn LLMProvider>>,
    pub compiler: Arc<dyn LLMProvider>,
    pub gate: Arc<dyn CostGateImpl>,
    pub session: Option<Arc<parking_lot::RwLock<SessionManager>>>,
}

impl MoaLoop {
    /// Builds an `MoaLoop` with the given proposers, compiler, and gate.
    pub fn new(
        proposers: Vec<Arc<dyn LLMProvider>>,
        compiler: Arc<dyn LLMProvider>,
        gate: Arc<dyn CostGateImpl>,
        config: MoaConfig,
    ) -> Self {
        Self {
            config,
            proposers,
            compiler,
            gate,
            session: None,
        }
    }

    /// Attaches a session manager so every iteration is persisted.
    #[must_use]
    pub fn with_session(mut self, session: Arc<parking_lot::RwLock<SessionManager>>) -> Self {
        self.session = Some(session);
        self
    }

    /// Estimates the cost of a run with the current configuration.
    #[must_use]
    pub fn estimate_cost(&self) -> CostBreakdown {
        let mut breakdown = CostBreakdown::empty();

        // Proposers: each proposer is called once per iteration.
        for proposer in &self.proposers {
            let pc = self.estimate_provider_cost(proposer, ProviderRole::Proposer);
            breakdown.add(pc);
        }

        // Compiler: called once per iteration.
        let cc = self.estimate_provider_cost(&self.compiler, ProviderRole::Compiler);
        breakdown.add(cc);

        breakdown
    }

    fn estimate_provider_cost(
        &self,
        provider: &Arc<dyn LLMProvider>,
        role: ProviderRole,
    ) -> ProviderCost {
        let cfg = provider.config();
        let pricing = pricing_for(&cfg.model);
        let iters = if matches!(role, ProviderRole::Proposer) {
            self.config.max_iterations
        } else {
            self.config.max_iterations
        };
        let calls = if matches!(role, ProviderRole::Proposer) {
            self.config.n_proposers
        } else {
            1
        };
        let calls = calls * iters;
        let input = u64::from(self.config.max_input_tokens_per_proposer) * calls as u64;
        let output = u64::from(self.config.max_output_tokens_per_proposer) * calls as u64;
        let usd = pricing
            .as_ref()
            .map_or(0.0, |p| p.estimate_usd(input, output));
        let estimate = CostEstimate {
            min_cost_usd: usd * 0.5,
            expected_cost_usd: usd,
            max_cost_usd: usd * 2.0,
            input_tokens: input,
            output_tokens: output,
            wall_time_ms: calls as u64 * 2000,
        };
        ProviderCost {
            provider: cfg.base_url.clone(),
            model: cfg.model.clone(),
            role,
            estimate,
            actual: None,
        }
    }

    /// Runs the Enriched Inference loop.
    ///
    /// 1. Estimates cost.
    /// 2. Submits a `CostProposal` to the gate. If the user aborts,
    ///    returns `Error::Aborted`.
    /// 3. Runs N proposers in parallel (Phase 1).
    /// 4. Runs the compiler (Phase 2). Repeats `max_iterations` times.
    /// 5. Persists every step in the session graph (if attached).
    pub async fn run(&self, query: &str) -> Result<MoaResult> {
        let started = Instant::now();
        let breakdown = self.estimate_cost();
        let total_estimate = breakdown.total_estimate.clone();

        // Phase 0: Cost Gate
        let proposal = CostProposal {
            operation: "Enriched Inference".to_string(),
            estimate: total_estimate,
            proposed_config: format!(
                "n_proposers={}, max_iterations={}, compiler={}, verifier_skills={}",
                self.config.n_proposers,
                self.config.max_iterations,
                self.compiler.config().model,
                self.config.enable_verification_skills
            ),
            alternatives: Vec::new(), // F2.x: pre-computed cheaper alternatives
        };

        let decision = self.gate.ask(proposal).await?;
        let final_config_str = match &decision {
            CostDecision::Proceed => "default".to_string(),
            CostDecision::ProceedWithConfig { config_overrides } => config_overrides.clone(),
            CostDecision::Abort { reason } => {
                return Err(DogmaError::Execution(format!(
                    "Cost Gate aborted: {reason}"
                )));
            }
        };

        // Persist the cost proposal + decision in the session graph.
        self.persist_cost_proposal(query, &final_config_str, &decision)
            .await
            .ok(); // best-effort

        // Phase 1 + 2: the MoA loop
        let mut iterations: Vec<MoaIteration> = Vec::new();
        let mut current_query = query.to_string();
        let mut final_breakdown = CostBreakdown::empty();

        for iter_idx in 0..self.config.max_iterations {
            if started.elapsed().as_millis() as u64 > self.config.max_wall_time_ms {
                info!("MoA loop: wall-time budget exhausted at iter {iter_idx}");
                break;
            }

            // Phase 1: fan-out to N proposers (parallel)
            let proposer_responses = self
                .run_proposers_parallel(&current_query, iter_idx, &mut final_breakdown)
                .await?;

            // Persist proposer responses
            self.persist_iteration(iter_idx, &proposer_responses, None)
                .await
                .ok();

            // Phase 2: compiler synthesizes
            let compiled = self
                .run_compiler(&current_query, &proposer_responses, iter_idx, &mut final_breakdown)
                .await?;

            // Persist the compiled response
            self.persist_iteration(iter_idx, &[], Some(&compiled))
                .await
                .ok();

            iterations.push(MoaIteration {
                iteration: iter_idx,
                proposer_responses,
                compiled_response: compiled.clone(),
                skills_invoked: Vec::new(),
            });

            current_query = compiled.content.clone();
        }

        // Phase 3: optional verification skills (F2.x)
        // (skipped at MVP — enable_verification_skills is false by default)

        // Quality estimate
        let model_tiers: Vec<_> = self
            .proposers
            .iter()
            .map(|p| {
                let m = &p.config().model;
                super::quality_estimator::tier_for(m)
            })
            .collect();
        let quality_estimator = HeuristicQualityEstimator::moa(&model_tiers);
        let quality = quality_estimator.estimate_quality();

        let total_wall_time_ms = started.elapsed().as_millis() as u64;

        // Persist the actual cost
        self.persist_cost_actual(&final_breakdown, total_wall_time_ms)
            .await
            .ok();

        Ok(MoaResult {
            final_text: current_query,
            iterations,
            cost: final_breakdown,
            quality,
            total_wall_time_ms,
        })
    }

    // ── Phase helpers ────────────────────────────────────────────────

    async fn run_proposers_parallel(
        &self,
        query: &str,
        iter_idx: usize,
        breakdown: &mut CostBreakdown,
    ) -> Result<Vec<MoaResponse>> {
        let mut handles = Vec::new();
        for proposer in &self.proposers {
            let p = Arc::clone(proposer);
            let q = query.to_string();
            let h = tokio::spawn(async move { run_one_proposer(p, q).await });
            handles.push(h);
        }
        let mut responses = Vec::new();
        for (i, h) in handles.into_iter().enumerate() {
            let resp = h
                .await
                .map_err(|e| DogmaError::Internal(format!("proposer join error: {e}")))?;
            let mut resp = resp?;
            resp.iteration = iter_idx;
            // Record in breakdown (rough estimate; the actual
            // would be the real token count).
            if let Some(pc) = self.estimate_provider_cost_for_response(&responses) {
                let _ = pc;
            }
            breakdown.add(ProviderCost {
                provider: self.proposers.get(i).map(|p| p.config().base_url.clone()).unwrap_or_default(),
                model: resp.model.clone(),
                role: ProviderRole::Proposer,
                estimate: CostEstimate {
                    min_cost_usd: 0.0,
                    expected_cost_usd: resp.estimated_cost_usd,
                    max_cost_usd: resp.estimated_cost_usd * 2.0,
                    input_tokens: u64::from(resp.input_tokens),
                    output_tokens: u64::from(resp.output_tokens),
                    wall_time_ms: resp.wall_time_ms,
                },
                actual: None,
            });
            responses.push(resp);
        }
        Ok(responses)
    }

    fn estimate_provider_cost_for_response(&self, _responses: &[MoaResponse]) -> Option<()> {
        // Helper reserved for future actual-cost tracking.
        Some(())
    }

    async fn run_compiler(
        &self,
        query: &str,
        proposers: &[MoaResponse],
        iter_idx: usize,
        breakdown: &mut CostBreakdown,
    ) -> Result<MoaResponse> {
        let synthesis_prompt = build_synthesis_prompt(query, proposers, iter_idx);
        let messages = vec![
            Message::new(MessageRole::System, COMPILER_SYSTEM_PROMPT.to_string()),
            Message::new(MessageRole::User, synthesis_prompt),
        ];
        let started = Instant::now();
        let resp = self.compiler.chat(&messages, &[]).await?;
        let wall_time_ms = started.elapsed().as_millis() as u64;
        let pricing = pricing_for(&self.compiler.config().model);
        let input_tokens = resp.usage.prompt_tokens;
        let output_tokens = resp.usage.completion_tokens;
        let usd = pricing
            .as_ref()
            .map_or(0.0, |p| p.estimate_usd(u64::from(input_tokens), u64::from(output_tokens)));

        breakdown.add(ProviderCost {
            provider: self.compiler.config().base_url.clone(),
            model: self.compiler.config().model.clone(),
            role: ProviderRole::Compiler,
            estimate: CostEstimate {
                min_cost_usd: usd * 0.5,
                expected_cost_usd: usd,
                max_cost_usd: usd * 2.0,
                input_tokens: u64::from(input_tokens),
                output_tokens: u64::from(output_tokens),
                wall_time_ms,
            },
            actual: None,
        });

        Ok(MoaResponse {
            model: self.compiler.config().model.clone(),
            role: ProviderRole::Compiler,
            iteration: iter_idx,
            content: resp.content,
            input_tokens,
            output_tokens,
            wall_time_ms,
            estimated_cost_usd: usd,
        })
    }

    // ── Persistence ──────────────────────────────────────────────────

    async fn persist_cost_proposal(
        &self,
        query: &str,
        config: &str,
        decision: &CostDecision,
    ) -> Result<()> {
        let Some(session) = &self.session else { return Ok(()) };
        let session_id = query_session_id(query);
        let node = Document::builder(
            &format!("cost-proposal-{}", uuid::Uuid::new_v4()),
            format!("Cost proposal: {config}"),
        )
        .metadata("node_type", "CostProposal")
        .metadata("session_id", &session_id)
        .metadata("decision", &decision.to_string())
        .metadata("created_at", &chrono::Utc::now().to_rfc3339())
        .build();
        let _ = session.write().persist_node(node);
        Ok(())
    }

    async fn persist_iteration(
        &self,
        iter_idx: usize,
        proposers: &[MoaResponse],
        compiled: Option<&MoaResponse>,
    ) -> Result<()> {
        let Some(session) = &self.session else { return Ok(()) };
        for resp in proposers {
            let node = Document::builder(
                &format!("moa-proposer-{iter_idx}-{}", uuid::Uuid::new_v4()),
                &resp.content,
            )
            .metadata("node_type", "MoAProposer")
            .metadata("iteration", iter_idx.to_string())
            .metadata("model", &resp.model)
            .metadata("role", "proposer")
            .metadata("input_tokens", resp.input_tokens.to_string())
            .metadata("output_tokens", resp.output_tokens.to_string())
            .metadata("created_at", &chrono::Utc::now().to_rfc3339())
            .build();
            let _ = session.write().persist_node(node);
        }
        if let Some(c) = compiled {
            let node = Document::builder(
                &format!("moa-compiler-{iter_idx}-{}", uuid::Uuid::new_v4()),
                &c.content,
            )
            .metadata("node_type", "MoACompiler")
            .metadata("iteration", iter_idx.to_string())
            .metadata("model", &c.model)
            .metadata("role", "compiler")
            .metadata("input_tokens", c.input_tokens.to_string())
            .metadata("output_tokens", c.output_tokens.to_string())
            .metadata("created_at", &chrono::Utc::now().to_rfc3339())
            .build();
            let _ = session.write().persist_node(node);
        }
        Ok(())
    }

    async fn persist_cost_actual(
        &self,
        breakdown: &CostBreakdown,
        wall_time_ms: u64,
    ) -> Result<()> {
        let Some(session) = &self.session else { return Ok(()) };
        let node = Document::builder(
            &format!("cost-actual-{}", uuid::Uuid::new_v4()),
            format!(
                "Total wall: {wall_time_ms}ms; providers: {}",
                breakdown.proposers.len()
            ),
        )
        .metadata("node_type", "CostActual")
        .metadata("wall_time_ms", wall_time_ms.to_string())
        .metadata("created_at", &chrono::Utc::now().to_rfc3339())
        .build();
        let _ = session.write().persist_node(node);
        Ok(())
    }
}

// =========================================================================
// Free functions
// =========================================================================

const COMPILER_SYSTEM_PROMPT: &str = "\
    You are the compiler of a Mixture-of-Agents system. Multiple \
    independent AI models have produced candidate responses to the \
    user's query. Your job is to synthesize the best possible answer \
    by:\n\
    1. Identifying the strongest points in each candidate.\n\
    2. Resolving contradictions in favor of the better-supported claim.\n\
    3. Producing a final response that is more accurate, more \
    complete, and more useful than any individual candidate.\n\n\
    Be direct. Lead with the answer. The user is paying for your time; \
    do not waste it.";

fn build_synthesis_prompt(query: &str, proposers: &[MoaResponse], iter_idx: usize) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "Original query:\n<query>\n{query}\n</query>\n\n"
    ));
    s.push_str(&format!(
        "{n} candidate responses (iteration {iter}):\n\n",
        n = proposers.len(),
        iter = iter_idx + 1
    ));
    for (i, r) in proposers.iter().enumerate() {
        s.push_str(&format!(
            "--- Candidate {} ({}) ---\n{}\n\n",
            i + 1,
            r.model,
            r.content
        ));
    }
    s.push_str(
        "Produce a synthesized final answer. Be more accurate and more \
         complete than any individual candidate. Be direct.",
    );
    s
}

fn query_session_id(query: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    query.hash(&mut h);
    format!("ei-{:x}", h.finish())
}

async fn run_one_proposer(proposer: Arc<dyn LLMProvider>, query: String) -> Result<MoaResponse> {
    let messages = vec![
        Message::new(MessageRole::System, PROPOSER_SYSTEM_PROMPT.to_string()),
        Message::new(MessageRole::User, query),
    ];
    let started = Instant::now();
    let resp = proposer.chat(&messages, &[]).await?;
    let wall_time_ms = started.elapsed().as_millis() as u64;
    let pricing = pricing_for(&proposer.config().model);
    let usd = pricing.as_ref().map_or(0.0, |p| {
        p.estimate_usd(
            u64::from(resp.usage.prompt_tokens),
            u64::from(resp.usage.completion_tokens),
        )
    });
    Ok(MoaResponse {
        model: proposer.config().model.clone(),
        role: ProviderRole::Proposer,
        iteration: 0,
        content: resp.content,
        input_tokens: resp.usage.prompt_tokens,
        output_tokens: resp.usage.completion_tokens,
        wall_time_ms,
        estimated_cost_usd: usd,
    })
}

const PROPOSER_SYSTEM_PROMPT: &str = "\
    You are a proposer in a Mixture-of-Agents system. The user has \
    asked a question. Produce a direct, accurate, complete answer. \
    Do not hedge. Do not be brief. Be useful. If you do not know, say \
    so clearly and explain what would be needed to find out.";

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moa_config_default_has_three_proposers() {
        let c = MoaConfig::default();
        assert_eq!(c.n_proposers, 3);
        assert_eq!(c.max_iterations, 2);
        assert!(!c.enable_verification_skills);
    }

    #[test]
    fn moa_config_estimate_returns_non_zero_tokens() {
        let c = MoaConfig::default();
        let est = c.estimate_cost();
        assert!(est.input_tokens > 0);
        assert!(est.output_tokens > 0);
        assert!(est.wall_time_ms > 0);
    }

    #[test]
    fn synthesis_prompt_includes_query_and_responses() {
        let proposers = vec![MoaResponse {
            model: "m1".to_string(),
            role: ProviderRole::Proposer,
            iteration: 0,
            content: "answer 1".to_string(),
            input_tokens: 0,
            output_tokens: 0,
            wall_time_ms: 0,
            estimated_cost_usd: 0.0,
        }];
        let prompt = build_synthesis_prompt("what is X?", &proposers, 0);
        assert!(prompt.contains("what is X?"));
        assert!(prompt.contains("answer 1"));
        assert!(prompt.contains("Candidate 1"));
        assert!(prompt.contains("m1"));
    }

    #[test]
    fn session_id_is_deterministic() {
        assert_eq!(query_session_id("foo"), query_session_id("foo"));
        assert_ne!(query_session_id("foo"), query_session_id("bar"));
    }
}
