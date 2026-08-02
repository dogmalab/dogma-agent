//! # dogma-v2-core — Runtime asíncrono, Tool Loop, State Management
//!
//! Este crate implementa el núcleo del agente Dogma 2.0:
//!
//! * **Runtime** — Loop principal de IA (RSI) con trait para proveedores
//!   LLM OpenAI-Compatibles dinámicos, más el patrón flagship
//!   `MoaLoop` (Enriched Inference) y los patrones `Cost Gate` y
//!   `Quality Estimator`.
//! * **Tools** — Las 3 herramientas de supervivencia:
//!   `read_file`, `write_file`, `execute_script`.
//! * **State** — Session Manager y adaptadores sobre `dogma-vdb` para
//!   almacenar todo el estado como nodos de un grafo vectorial.

pub mod models;
pub mod runtime;
pub mod state;
pub mod tools;

pub use models::memory::EnvironmentMemory;
pub use models::plan::Plan;
pub use runtime::cost_estimator::{
    CostBreakdown, CostCalculable, CostDelta, CostEstimate, ModelPricing, ProviderCost,
    ProviderRole, pricing_for,
};
pub use runtime::cost_gate::{
    AlternativeProposal, AutoCostGate, CostDecision, CostGateImpl, CostProposal,
    InteractiveCostGate, TrustedCostGate, WebhookCostGate,
};
pub use runtime::enriched::{
    CompilerConfig, MoaConfig, MoaIteration, MoaLoop, MoaResponse, MoaResult,
};
pub use runtime::loop_handle::RuntimeLoop;
pub use runtime::provider::LLMProvider;
pub use runtime::quality_estimator::{
    HeuristicQualityEstimator, ModelTier, QualityBasis, QualityCalculable, QualityEstimate,
    tier_for,
};
pub use runtime::sub_agent::SubAgentManager;
pub use runtime::wasm_sandbox::WasmSandbox;
pub use state::compressor::Compressor;
pub use state::session::SessionManager;
pub use tools::{Tool, ToolResult};
