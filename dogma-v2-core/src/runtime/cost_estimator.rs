//! # Cost Estimator — The "calculable" half of the Cost Gate
//!
//! Every expensive operation in the agent harness (LLM calls, WASM
//! execution, skill installation) must produce a [`CostEstimate`]
//! before the operation runs and a [`CostBreakdown`] after. The
//! estimate is the input to the [`CostGate`](super::cost_gate::CostGate);
//! the breakdown is the audit record.
//!
//! The estimator does not need to be perfectly accurate. The
//! commitment is "calculable", not "exact". The estimator learns
//! from the gap between estimate and actual (the `CostDelta`),
//! and the gap is published in the session graph for calibration.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Estimated cost of running an operation, with confidence bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    /// Worst-case lower bound (cheapest plausible outcome).
    pub min_cost_usd: f64,
    /// Best estimate (case medio).
    pub expected_cost_usd: f64,
    /// Worst-case upper bound (most expensive plausible outcome).
    pub max_cost_usd: f64,
    /// Estimated input tokens across all proposers + compiler.
    pub input_tokens: u64,
    /// Estimated output tokens across all proposers + compiler.
    pub output_tokens: u64,
    /// Estimated wall-time in milliseconds.
    pub wall_time_ms: u64,
}

impl CostEstimate {
    /// Construye un `CostEstimate` con todos los campos en cero.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            min_cost_usd: 0.0,
            expected_cost_usd: 0.0,
            max_cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            wall_time_ms: 0,
        }
    }

    /// Suma otra estimación a esta (útil para acumular costo de
    /// múltiples proposers o iteraciones).
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        Self {
            min_cost_usd: self.min_cost_usd + other.min_cost_usd,
            expected_cost_usd: self.expected_cost_usd + other.expected_cost_usd,
            max_cost_usd: self.max_cost_usd + other.max_cost_usd,
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            wall_time_ms: self.wall_time_ms + other.wall_time_ms,
        }
    }

    /// Multiplica esta estimación por un factor (útil para escalar
    /// por número de iteraciones).
    #[must_use]
    pub fn scale(&self, factor: f64) -> Self {
        Self {
            min_cost_usd: self.min_cost_usd * factor,
            expected_cost_usd: self.expected_cost_usd * factor,
            max_cost_usd: self.max_cost_usd * factor,
            input_tokens: (f64::from(self.input_tokens) * factor) as u64,
            output_tokens: (f64::from(self.output_tokens) * factor) as u64,
            wall_time_ms: (f64::from(self.wall_time_ms) * factor) as u64,
        }
    }
}

impl fmt::Display for CostEstimate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "${:.4} (±${:.4}) · {} in / {} out · ~{}ms",
            self.expected_cost_usd,
            self.max_cost_usd - self.expected_cost_usd,
            self.input_tokens,
            self.output_tokens,
            self.wall_time_ms
        )
    }
}

/// Cost breakdown for one provider in one iteration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCost {
    /// Nombre del provider (ej: "openai", "ollama", "anthropic").
    pub provider: String,
    /// Modelo (ej: "gpt-4o", "claude-sonnet-4", "llama-3.1-70b").
    pub model: String,
    /// Role en el MoA loop: proposer, compiler, verifier.
    pub role: ProviderRole,
    /// Costo estimado antes de la corrida.
    pub estimate: CostEstimate,
    /// Costo real después de la corrida (con tokens reportados por el provider).
    pub actual: Option<CostEstimate>,
}

/// Role de un provider en una operación MoA.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    /// Uno de los N proposers que corren en paralelo.
    Proposer,
    /// El compiler que sintetiza las respuestas.
    Compiler,
    /// Verifier que valida el resultado (opcional).
    Verifier,
}

/// Desglose completo del costo de una operación.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostBreakdown {
    pub proposers: Vec<ProviderCost>,
    pub compiler: Option<ProviderCost>,
    pub verifiers: Vec<ProviderCost>,
    pub total_estimate: CostEstimate,
    pub total_actual: Option<CostEstimate>,
}

impl CostBreakdown {
    /// Construye un breakdown vacío.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            proposers: Vec::new(),
            compiler: None,
            verifiers: Vec::new(),
            total_estimate: CostEstimate::zero(),
            total_actual: None,
        }
    }

    /// Suma un `ProviderCost` al breakdown y actualiza el total.
    pub fn add(&mut self, cost: ProviderCost) {
        match cost.role {
            ProviderRole::Proposer => {
                self.total_estimate = self.total_estimate.add(&cost.estimate);
                if let Some(actual) = &cost.actual {
                    let actual = self.total_actual.clone().unwrap_or_else(CostEstimate::zero);
                    self.total_actual = Some(actual.add(actual));
                }
                self.proposers.push(cost);
            }
            ProviderRole::Compiler => {
                self.total_estimate = self.total_estimate.add(&cost.estimate);
                self.compiler = Some(cost);
            }
            ProviderRole::Verifier => {
                self.total_estimate = self.total_estimate.add(&cost.estimate);
                self.verifiers.push(cost);
            }
        }
    }
}

/// Diferencia entre el costo estimado y el real.
///
/// El `delta_usd` es `actual - expected`. Positivo = sub-presupuestado,
/// negativo = sobre-presupuestado. Se persiste en el session graph
/// para que el estimator pueda calibrarse con datos reales.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostDelta {
    pub expected_usd: f64,
    pub actual_usd: f64,
    pub delta_usd: f64,
    pub input_token_delta: i64,
    pub output_token_delta: i64,
    pub wall_time_delta_ms: i64,
}

impl CostDelta {
    /// Calcula la diferencia entre un estimate y el actual.
    #[must_use]
    pub fn between(expected: &CostEstimate, actual: &CostEstimate) -> Self {
        Self {
            expected_usd: expected.expected_cost_usd,
            actual_usd: actual.expected_cost_usd,
            delta_usd: actual.expected_cost_usd - expected.expected_cost_usd,
            input_token_delta: actual.input_tokens as i64 - expected.input_tokens as i64,
            output_token_delta: actual.output_tokens as i64 - expected.output_tokens as i64,
            wall_time_delta_ms: actual.wall_time_ms as i64 - expected.wall_time_ms as i64,
        }
    }
}

/// Trait para estimar el costo de una operación **antes** de correrla.
///
/// Cualquier operación cara (LLM call, WASM execution, skill install)
/// debe poder estimar su costo. El estimador es la mitad "calculable"
/// del Cost Gate; la otra mitad es el [`CostGate`](super::cost_gate::CostGate)
/// que pide confirmación humana.
pub trait CostCalculable {
    /// Devuelve el costo estimado de la operación.
    fn estimate_cost(&self) -> CostEstimate;
}

/// Política de precios de un modelo.
///
/// El `cost_per_1k_input_tokens` y `cost_per_1k_output_tokens` están
/// en USD. Para modelos locales (Ollama, candle) son $0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    pub model: String,
    pub cost_per_1k_input_tokens: f64,
    pub cost_per_1k_output_tokens: f64,
}

impl ModelPricing {
    /// Estima el costo en USD dado el conteo de tokens.
    #[must_use]
    pub fn estimate_usd(&self, input_tokens: u64, output_tokens: u64) -> f64 {
        (f64::from(input_tokens) / 1000.0) * self.cost_per_1k_input_tokens
            + (f64::from(output_tokens) / 1000.0) * self.cost_per_1k_output_tokens
    }
}

/// Catálogo de precios hardcoded para modelos comunes.
///
/// Los precios se actualizan trimestralmente. Para modelos no
/// listados, devuelve `None` y el llamador debe usar un estimado
/// genérico o pedirle al usuario que confirme el pricing.
#[must_use]
pub fn pricing_for(model: &str) -> Option<ModelPricing> {
    match model {
        // OpenAI
        "gpt-4o" | "gpt-4o-2024-08-06" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.0025,
            cost_per_1k_output_tokens: 0.01,
        }),
        "gpt-4o-mini" | "gpt-4o-mini-2024-07-18" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.00015,
            cost_per_1k_output_tokens: 0.0006,
        }),
        "o1" | "o1-2024-12-17" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.015,
            cost_per_1k_output_tokens: 0.06,
        }),
        "o1-mini" | "o1-mini-2024-09-12" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.003,
            cost_per_1k_output_tokens: 0.012,
        }),
        // Anthropic
        "claude-sonnet-4-20250514" | "claude-sonnet-4" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.003,
            cost_per_1k_output_tokens: 0.015,
        }),
        "claude-3-5-sonnet-20241022" | "claude-3-5-sonnet" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.003,
            cost_per_1k_output_tokens: 0.015,
        }),
        "claude-3-5-haiku-20241022" | "claude-3-5-haiku" => Some(ModelPricing {
            model: model.to_string(),
            cost_per_1k_input_tokens: 0.0008,
            cost_per_1k_output_tokens: 0.004,
        }),
        // Local (Ollama) — $0
        m if m.starts_with("llama")
            || m.starts_with("qwen")
            || m.starts_with("mistral")
            || m.starts_with("mixtral")
            || m.starts_with("deepseek")
            || m.starts_with("gemma") =>
        {
            Some(ModelPricing {
                model: model.to_string(),
                cost_per_1k_input_tokens: 0.0,
                cost_per_1k_output_tokens: 0.0,
            })
        }
        // Unknown — return None; caller must handle
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_estimate_zero_is_zero() {
        let z = CostEstimate::zero();
        assert_eq!(z.expected_cost_usd, 0.0);
        assert_eq!(z.input_tokens, 0);
    }

    #[test]
    fn cost_estimate_add_sums_fields() {
        let a = CostEstimate {
            min_cost_usd: 1.0,
            expected_cost_usd: 2.0,
            max_cost_usd: 3.0,
            input_tokens: 100,
            output_tokens: 200,
            wall_time_ms: 1000,
        };
        let b = CostEstimate {
            min_cost_usd: 4.0,
            expected_cost_usd: 5.0,
            max_cost_usd: 6.0,
            input_tokens: 50,
            output_tokens: 100,
            wall_time_ms: 500,
        };
        let sum = a.add(&b);
        assert_eq!(sum.expected_cost_usd, 7.0);
        assert_eq!(sum.input_tokens, 150);
        assert_eq!(sum.output_tokens, 300);
        assert_eq!(sum.wall_time_ms, 1500);
    }

    #[test]
    fn cost_estimate_scale_multiplies() {
        let a = CostEstimate {
            min_cost_usd: 1.0,
            expected_cost_usd: 2.0,
            max_cost_usd: 3.0,
            input_tokens: 100,
            output_tokens: 200,
            wall_time_ms: 1000,
        };
        let s = a.scale(2.0);
        assert_eq!(s.expected_cost_usd, 4.0);
        assert_eq!(s.input_tokens, 200);
    }

    #[test]
    fn pricing_for_openai_gpt4o() {
        let p = pricing_for("gpt-4o").unwrap();
        let cost = p.estimate_usd(1000, 500);
        // 1k * 0.0025 + 0.5k * 0.01 = 0.0025 + 0.005 = 0.0075
        assert!((cost - 0.0075).abs() < 1e-6);
    }

    #[test]
    fn pricing_for_local_is_free() {
        let p = pricing_for("llama-3.1-70b").unwrap();
        assert_eq!(p.estimate_usd(1000, 1000), 0.0);
    }

    #[test]
    fn pricing_for_unknown_returns_none() {
        assert!(pricing_for("some-unknown-model-xyz").is_none());
    }

    #[test]
    fn cost_delta_between_computes_difference() {
        let expected = CostEstimate {
            min_cost_usd: 0.0,
            expected_cost_usd: 0.01,
            max_cost_usd: 0.02,
            input_tokens: 1000,
            output_tokens: 500,
            wall_time_ms: 2000,
        };
        let actual = CostEstimate {
            min_cost_usd: 0.0,
            expected_cost_usd: 0.012,
            max_cost_usd: 0.022,
            input_tokens: 1100,
            output_tokens: 550,
            wall_time_ms: 2200,
        };
        let delta = CostDelta::between(&expected, &actual);
        assert!((delta.delta_usd - 0.002).abs() < 1e-6);
        assert_eq!(delta.input_token_delta, 100);
    }
}
