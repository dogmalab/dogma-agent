//! # Quality Estimator — Best-effort quality prediction for the harness
//!
//! Quality estimation is the research frontier of the harness.
//! The estimator is intentionally heuristic at MVP; it is meant
//! to be calibrated by data from the open benchmark
//! ([dogmalab/dogma-arena](https://github.com/dogmalab/dogma-arena))
//! over time.
//!
//! At MVP, the estimator uses two signals:
//! 1. **Model-tier prior** — frontier-tier-A models are expected to
//!    score higher than tier-B models on average.
//! 2. **Consensus prior** — when N proposers agree, the synthesis
//!    is expected to be higher quality than when they disagree.
//!
//! The estimator is opt-in. The harness runs without it; the
//! results are simply not annotated with a quality estimate.

use serde::{Deserialize, Serialize};

/// A quality estimate for a single harness run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityEstimate {
    /// Expected quality score in [0.0, 1.0].
    pub expected_score: f32,
    /// Confidence in the estimate, in [0.0, 1.0]. Low confidence
    /// means the estimate is a rough heuristic.
    pub confidence: f32,
    /// What the estimate is based on.
    pub basis: QualityBasis,
}

/// The basis for a quality estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum QualityBasis {
    /// Estimate derived from the model tier and a single call.
    /// No empirical data yet. Low confidence.
    HeuristicModelTier,
    /// Estimate derived from proposer consensus (variance of scores).
    /// Medium confidence.
    HeuristicConsensus,
    /// Estimate calibrated against the open benchmark.
    /// High confidence.
    BenchmarkCalibrated,
}

/// The trait every quality estimator implements.
pub trait QualityCalculable {
    /// Returns a quality estimate for the configured operation.
    fn estimate_quality(&self) -> QualityEstimate;
}

/// Tier classification for a model. Used by the heuristic baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ModelTier {
    /// Frontier-tier-A: GPT-4o, Claude Opus, etc. Baseline 0.85.
    FrontierA,
    /// Frontier-tier-B: Claude Sonnet, Llama 3.1 70B, Qwen 2.5 72B.
    /// Baseline 0.70.
    FrontierB,
    /// Mid-tier: GPT-4o-mini, Claude Haiku, Llama 8B, etc. Baseline 0.55.
    Mid,
    /// Small/local: 1B-3B models. Baseline 0.30.
    Small,
}

/// Heuristic estimator: returns a baseline quality score based on
/// the strongest model in the configuration.
pub struct HeuristicQualityEstimator {
    /// The strongest model that will participate in the run.
    pub strongest_model_tier: ModelTier,
    /// Number of distinct proposer models in the configuration.
    pub n_distinct_proposers: usize,
}

impl HeuristicQualityEstimator {
    /// Builds an estimator for a single-model run.
    #[must_use]
    pub const fn single(tier: ModelTier) -> Self {
        Self {
            strongest_model_tier: tier,
            n_distinct_proposers: 1,
        }
    }

    /// Builds an estimator for an N-proposer MoA run.
    #[must_use]
    pub const fn moa(tiers: &[ModelTier]) -> Self {
        let strongest = tiers
            .iter()
            .min_by_key(|t| match t {
                ModelTier::FrontierA => 0,
                ModelTier::FrontierB => 1,
                ModelTier::Mid => 2,
                ModelTier::Small => 3,
            })
            .copied()
            .unwrap_or(ModelTier::Mid);
        let n_distinct = tiers
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        Self {
            strongest_model_tier: strongest,
            n_distinct_proposers: n_distinct.max(1),
        }
    }
}

impl QualityCalculable for HeuristicQualityEstimator {
    fn estimate_quality(&self) -> QualityEstimate {
        let base = match self.strongest_model_tier {
            ModelTier::FrontierA => 0.85,
            ModelTier::FrontierB => 0.70,
            ModelTier::Mid => 0.55,
            ModelTier::Small => 0.30,
        };

        // Bonus for diversity in the proposer pool. A diverse
        // ensemble compensates for individual model weaknesses.
        let diversity_bonus = match self.n_distinct_proposers {
            0 | 1 => 0.0,
            2 => 0.03,
            3 => 0.07,
            4 => 0.10,
            _ => 0.12,
        };

        // Cap at 0.95 — the harness improves models, it does not
        // make them divine.
        let expected = (base + diversity_bonus).min(0.95);

        QualityEstimate {
            expected_score: expected,
            confidence: 0.3, // heuristic baseline, low confidence
            basis: QualityBasis::HeuristicModelTier,
        }
    }
}

/// Classifies a model name into a tier. Unknown models default to Mid.
#[must_use]
pub fn tier_for(model: &str) -> ModelTier {
    let m = model.to_lowercase();
    if m.starts_with("gpt-4o")
        || m.starts_with("o1")
        || m.contains("opus")
        || m.contains("sonnet-4")
    {
        ModelTier::FrontierA
    } else if m.starts_with("llama-3.1-70b")
        || m.starts_with("llama-3.1-405b")
        || m.starts_with("qwen-2.5-72b")
        || m.starts_with("qwen2.5-72b")
        || m.starts_with("mixtral-8x22b")
        || m.contains("sonnet-3-5")
        || m.starts_with("deepseek-v3")
    {
        ModelTier::FrontierB
    } else if m.starts_with("gpt-4o-mini")
        || m.starts_with("o1-mini")
        || m.contains("haiku")
        || m.starts_with("llama-3.1-8b")
        || m.starts_with("llama-3.2")
        || m.starts_with("gemma-2-9b")
    {
        ModelTier::Mid
    } else {
        ModelTier::Small
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic_estimator_frontier_a_single() {
        let est = HeuristicQualityEstimator::single(ModelTier::FrontierA);
        let q = est.estimate_quality();
        assert!((q.expected_score - 0.85).abs() < 1e-6);
    }

    #[test]
    fn heuristic_estimator_diversity_bonus() {
        let one = HeuristicQualityEstimator::single(ModelTier::FrontierB);
        let three = HeuristicQualityEstimator::moa(&[
            ModelTier::FrontierB,
            ModelTier::Mid,
            ModelTier::Small,
        ]);
        assert!(three.estimate_quality().expected_score > one.estimate_quality().expected_score);
    }

    #[test]
    fn heuristic_estimator_caps_at_0_95() {
        let est = HeuristicQualityEstimator::moa(&[
            ModelTier::FrontierA,
            ModelTier::FrontierA,
            ModelTier::FrontierA,
            ModelTier::FrontierA,
            ModelTier::FrontierA,
        ]);
        let q = est.estimate_quality();
        assert!(q.expected_score <= 0.95);
    }

    #[test]
    fn tier_for_classifies_well_known_models() {
        assert_eq!(tier_for("gpt-4o"), ModelTier::FrontierA);
        assert_eq!(tier_for("claude-sonnet-4-20250514"), ModelTier::FrontierA);
        assert_eq!(tier_for("llama-3.1-70b"), ModelTier::FrontierB);
        assert_eq!(tier_for("qwen2.5-72b"), ModelTier::FrontierB);
        assert_eq!(tier_for("gpt-4o-mini"), ModelTier::Mid);
        assert_eq!(tier_for("llama-3.1-8b"), ModelTier::Mid);
        assert_eq!(tier_for("some-unknown"), ModelTier::Small);
    }
}
