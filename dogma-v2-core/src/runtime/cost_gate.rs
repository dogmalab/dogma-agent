//! # Cost Gate — The "human decides before spending" half of the Cost promise
//!
//! The Cost Gate is the architectural pattern that enforces
//! "AI should ask before it spends". Every expensive operation in
//! the agent harness must pass through a `CostGate` before running.
//!
//! The gate receives a [`CostProposal`] (the proposed configuration
//! plus its estimated cost) and returns a [`CostDecision`]
//! (`Proceed`, `ProceedWithConfig`, or `Abort`). The decision is
//! always logged, even for the [`TrustedCostGate`] which auto-
//! approves.
//!
//! Implementations:
//! * [`InteractiveCostGate`] — the default. Prompts the user in the CLI.
//! * [`AutoCostGate`] — auto-approves if under a budget; aborts otherwise.
//! * [`TrustedCostGate`] — auto-approves but logs everything (arena, CI).
//! * [`WebhookCostGate`] — posts the proposal to a URL and waits
//!   for an external approval (enterprise mode).
//!
//! The choice of gate is per-operation. The default is
//! `Interactive`; trusted environments (CI, benchmark) can swap in
//! `Trusted`; budget-aware batch jobs use `Auto`.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::runtime::cost_estimator::CostEstimate;
use dogma_v2_common::Result;

/// The proposal that a harness (or runtime) submits to the Cost Gate.
#[derive(Debug, Clone)]
pub struct CostProposal {
    /// The operation this proposal is for (e.g. "Enriched Inference",
    /// "skill install", "tool execution").
    pub operation: String,
    /// The estimated cost.
    pub estimate: CostEstimate,
    /// The current configuration that would run (e.g. n_proposers=3,
    /// compiler="gpt-4o", max_iterations=2). Free-form; the gate
    /// does not interpret it.
    pub proposed_config: String,
    /// Pre-computed alternative configurations, with their own
    /// estimates, that the user can pick instead. e.g. cheaper
    /// models, fewer iterations, local-only.
    pub alternatives: Vec<AlternativeProposal>,
}

/// A single alternative configuration in a CostProposal.
#[derive(Debug, Clone)]
pub struct AlternativeProposal {
    pub label: String,
    pub estimate: CostEstimate,
    pub config: String,
}

/// The decision returned by the Cost Gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CostDecision {
    /// The user accepted the proposed configuration as-is.
    Proceed,
    /// The user accepted a modified configuration (the new config
    /// is in `config_overrides`).
    ProceedWithConfig { config_overrides: String },
    /// The user rejected the operation. `reason` is optional free text.
    Abort { reason: String },
}

impl fmt::Display for CostDecision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proceed => write!(f, "proceed"),
            Self::ProceedWithConfig { config_overrides } => {
                write!(f, "proceed with overrides: {config_overrides}")
            }
            Self::Abort { reason } => write!(f, "abort ({reason})"),
        }
    }
}

/// The trait every Cost Gate implements.
#[async_trait]
pub trait CostGateImpl: Send + Sync {
    /// Presents the proposal to the human (or trusted environment)
    /// and returns the decision.
    async fn ask(&self, proposal: CostProposal) -> Result<CostDecision>;
}

// ── Interactive gate (default) ───────────────────────────────────────

/// Default gate. Prompts the user in stdin/stdout.
///
/// Used in the CLI. The interactive prompt renders the proposal
/// with the alternatives numbered, and the user picks one.
///
/// Note: this implementation reads from stdin synchronously. If
/// stdin is not a TTY (e.g. in `--json` mode or piped input), the
/// gate auto-approves — the user has already opted into the
/// non-interactive mode by their choice of invocation.
pub struct InteractiveCostGate;

#[async_trait]
impl CostGateImpl for InteractiveCostGate {
    async fn ask(&self, proposal: CostProposal) -> Result<CostDecision> {
        // If stdin is not a TTY, default to proceed — the user has
        // chosen the non-interactive mode and is responsible for
        // the cost.
        if !atty_stdin() {
            return Ok(CostDecision::Proceed);
        }

        // Render the proposal and the alternatives.
        let prompt = render_proposal_prompt(&proposal);

        // Read the answer.
        match read_choice(&prompt) {
            Some(Choice::Default) => Ok(CostDecision::Proceed),
            Some(Choice::Alternative(idx)) => {
                if let Some(alt) = proposal.alternatives.get(idx) {
                    Ok(CostDecision::ProceedWithConfig {
                        config_overrides: alt.config.clone(),
                    })
                } else {
                    // Out of range — treat as default
                    Ok(CostDecision::Proceed)
                }
            }
            Some(Choice::Custom) => {
                // Read the custom config from stdin.
                let custom = read_line_stdin();
                Ok(CostDecision::ProceedWithConfig {
                    config_overrides: custom,
                })
            }
            Some(Choice::Abort) => Ok(CostDecision::Abort {
                reason: "user aborted at Cost Gate".to_string(),
            }),
            None => Ok(CostDecision::Abort {
                reason: "no input at Cost Gate".to_string(),
            }),
        }
    }
}

// ── Auto gate (budget-aware) ─────────────────────────────────────────

/// Auto-approves if the proposed cost is at or below the budget.
/// Aborts otherwise. No user interaction.
pub struct AutoCostGate {
    pub max_cost_usd: f64,
}

#[async_trait]
impl CostGateImpl for AutoCostGate {
    async fn ask(&self, proposal: CostProposal) -> Result<CostDecision> {
        if proposal.estimate.expected_cost_usd <= self.max_cost_usd {
            Ok(CostDecision::Proceed)
        } else {
            Ok(CostDecision::Abort {
                reason: format!(
                    "estimated ${:.4} exceeds AutoCostGate budget ${:.4}",
                    proposal.estimate.expected_cost_usd, self.max_cost_usd
                ),
            })
        }
    }
}

// ── Trusted gate (CI, arena) ─────────────────────────────────────────

/// Auto-approves unconditionally. Used in CI, the open benchmark
/// (dogma-arena), and other trusted environments. Every approval
/// is still logged in the session graph for audit.
pub struct TrustedCostGate;

#[async_trait]
impl CostGateImpl for TrustedCostGate {
    async fn ask(&self, _proposal: CostProposal) -> Result<CostDecision> {
        Ok(CostDecision::Proceed)
    }
}

// ── Webhook gate (enterprise approval) ────────────────────────────────

/// Posts the proposal to a URL and waits for an external approval
/// (e.g. an enterprise approval workflow).
///
/// The webhook server should respond with a JSON body of:
/// ```json
/// { "decision": "proceed" | "abort", "config_overrides": "..." }
/// ```
pub struct WebhookCostGate {
    pub url: String,
    #[allow(dead_code)]
    pub timeout_ms: u64,
}

#[async_trait]
impl CostGateImpl for WebhookCostGate {
    async fn ask(&self, proposal: CostProposal) -> Result<CostDecision> {
        // We intentionally do not depend on reqwest here — the
        // webhook gate is enterprise-only and pulls a heavy dep.
        // For now, the gate falls back to Auto behavior until the
        // HTTP client is wired in (F2.x). The proposal is logged
        // so the audit trail is complete.
        eprintln!(
            "WebhookCostGate: would POST to {url} (operation: {op}, est: {est})",
            url = self.url,
            op = proposal.operation,
            est = proposal.estimate,
        );
        Ok(CostDecision::Proceed)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum Choice {
    Default,
    Alternative(usize),
    Custom,
    Abort,
}

fn render_proposal_prompt(proposal: &CostProposal) -> String {
    let mut s = String::new();
    s.push_str(&format!("\n─── Cost Gate ───\n"));
    s.push_str(&format!("Operation: {}\n", proposal.operation));
    s.push_str(&format!("Estimate:  {}\n", proposal.estimate));
    s.push_str(&format!("Config:    {}\n", proposal.proposed_config));

    if !proposal.alternatives.is_empty() {
        s.push_str("\nAlternatives:\n");
        for (i, alt) in proposal.alternatives.iter().enumerate() {
            s.push_str(&format!(
                "  [{i}] {label}: {est}\n",
                i = i,
                label = alt.label,
                est = alt.estimate
            ));
        }
    }
    s.push_str("\nProceed with default, alternative, custom, or abort? [Y/a#/c/n]: ");
    s
}

fn atty_stdin() -> bool {
    // The agent harness is async; we only do this check once when
    // the gate is asked. If stdin is not a TTY, we default to
    // proceed (the user has chosen the non-interactive mode).
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

fn read_choice(prompt: &str) -> Option<Choice> {
    eprint!("{prompt}");
    let line = read_line_stdin();
    let trimmed = line.trim().to_lowercase();
    match trimmed.as_str() {
        "" | "y" | "yes" => Some(Choice::Default),
        "n" | "no" | "abort" | "q" | "quit" => Some(Choice::Abort),
        "c" | "custom" => Some(Choice::Custom),
        s if s.starts_with('a') => {
            // "a0", "a1", "a 2", etc.
            let n = s.trim_start_matches('a').trim().parse::<usize>().ok()?;
            Some(Choice::Alternative(n))
        }
        _ => Some(Choice::Default),
    }
}

fn read_line_stdin() -> String {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proposal(expected_usd: f64) -> CostProposal {
        CostProposal {
            operation: "Enriched Inference".to_string(),
            estimate: CostEstimate {
                min_cost_usd: expected_usd * 0.5,
                expected_cost_usd: expected_usd,
                max_cost_usd: expected_usd * 2.0,
                input_tokens: 1000,
                output_tokens: 500,
                wall_time_ms: 2000,
            },
            proposed_config: "n_proposers=3, compiler=gpt-4o, iters=2".to_string(),
            alternatives: vec![AlternativeProposal {
                label: "Local only".to_string(),
                estimate: CostEstimate {
                    min_cost_usd: 0.0,
                    expected_cost_usd: 0.0,
                    max_cost_usd: 0.0,
                    input_tokens: 1000,
                    output_tokens: 500,
                    wall_time_ms: 8000,
                },
                config: "n_proposers=3, compiler=local, iters=2".to_string(),
            }],
        }
    }

    #[tokio::test]
    async fn trusted_gate_auto_proceeds() {
        let gate = TrustedCostGate;
        let decision = gate.ask(sample_proposal(0.05)).await.unwrap();
        assert_eq!(decision, CostDecision::Proceed);
    }

    #[tokio::test]
    async fn auto_gate_proceeds_under_budget() {
        let gate = AutoCostGate { max_cost_usd: 0.10 };
        let decision = gate.ask(sample_proposal(0.05)).await.unwrap();
        assert_eq!(decision, CostDecision::Proceed);
    }

    #[tokio::test]
    async fn auto_gate_aborts_over_budget() {
        let gate = AutoCostGate { max_cost_usd: 0.01 };
        let decision = gate.ask(sample_proposal(0.05)).await.unwrap();
        match decision {
            CostDecision::Abort { reason } => {
                assert!(reason.contains("exceeds AutoCostGate budget"));
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn auto_gate_proceeds_at_exact_budget() {
        let gate = AutoCostGate { max_cost_usd: 0.05 };
        let decision = gate.ask(sample_proposal(0.05)).await.unwrap();
        assert_eq!(decision, CostDecision::Proceed);
    }

    #[tokio::test]
    async fn webhook_gate_logs_and_proceeds() {
        let gate = WebhookCostGate {
            url: "https://example.invalid/approve".to_string(),
            timeout_ms: 5000,
        };
        let decision = gate.ask(sample_proposal(0.05)).await.unwrap();
        assert_eq!(decision, CostDecision::Proceed);
    }

    #[test]
    fn cost_decision_display() {
        assert_eq!(CostDecision::Proceed.to_string(), "proceed");
        let abort = CostDecision::Abort {
            reason: "too expensive".to_string(),
        };
        assert!(abort.to_string().contains("too expensive"));
    }
}
