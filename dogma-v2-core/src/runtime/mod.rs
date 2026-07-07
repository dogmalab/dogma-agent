//! # Runtime — Loop principal de IA, patrones compuestos y proveedores
//!
//! El runtime expone:
//!
//! * `LLMProvider` — trait genérico para proveedores OpenAI-Compatibles.
//! * `RuntimeLoop` — el ciclo RSI de un solo LLM con tool calls.
//! * `enriched::MoaLoop` — el patrón flagship: N LLMs en paralelo
//!   sintetizados por un compiler (Mixture-of-Agents / Enriched
//!   Inference).
//! * `cost_estimator::CostCalculable` + `cost_gate::CostGateImpl` —
//!   el patrón "AI should ask before it spends" (Cost Gate).
//! * `quality_estimator::QualityCalculable` — el estimador (heurístico
//!   en MVP, calibrado por el open benchmark en F5).
//! * `sub_agent` y `wasm_sandbox` — aislamiento de sub-agentes y
//!   ejecución segura de scripts.

pub mod cost_estimator;
pub mod cost_gate;
pub mod enriched;
pub mod loop_handle;
pub mod provider;
pub mod quality_estimator;
pub mod sub_agent;
pub mod wasm_sandbox;
