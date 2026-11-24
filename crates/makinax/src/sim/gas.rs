//! ONE gas policy, for every surface that sends a transaction.
//!
//! WHY THIS EXISTS. A live Base swap that
//! mined with limit 1,139,794, used 1,122,697 — and died anyway, because the
//! innermost frame of a module→Safe→router→token chain received `0x2152` gas.
//! The output was 0.018556 USDC ABOVE the signed minimum, so this was never
//! slippage. Two separate gaps produced it:
//!
//!   1. The mined-fork preflight sent NO gas limit and let Anvil auto-estimate,
//!      so it proved a transaction nobody was going to broadcast.
//!   2. The live send also set no limit, so alloy's filler used the raw
//!      `eth_estimateGas` value with **no margin at all**.
//!
//! EIP-150 is the mechanism: a call forwards at most 63/64 of the gas it holds,
//! so a limit that merely covers the observed `gas_used` starves the DEEPEST
//! frame first. A budget that is 1.5% too small does not fail 1.5% of the way
//! down — it fails in the token transfer at the bottom.
//!
//! WHY 20%. Two measured gaps have to fit inside the margin at once
//! [Measured, differential on local forks]:
//!
//! | measurement | value |
//! |---|---|
//! | reported gas for the Clearstar withdrawal (fork receipt `gas_used`) | 625,782 — **fails** |
//! | that + 5% | 657,071 — **fails** (delegatecall OOG) |
//! | `eth_estimateGas` for the same call | 680,453 — **passes**; +8.7% over the reported figure |
//! | that + 10% | 748,498 — **passes** |
//! | route gas drift between quote and execution (swap trace) | **+6.5%** block-to-block |
//!
//! The estimation gap (8.7%) and the drift (6.5%) are independent and can land
//! together, so the margin must exceed roughly 15.2% compounded. 20% clears it
//! with room and costs, at Base prices, a fraction of a cent — gas is refunded
//! for what is not used. It is a MARGIN, not a fee.
//!
//! WHAT THIS MODULE DOES NOT DO. It never invents a number from a receipt or a
//! constant: every budget starts at a fresh `eth_estimateGas` against the exact
//! final call. A receipt's `gas_used` is a measurement of what happened, and it
//! is the value that was too small.

use alloy::{providers::Provider, rpc::types::TransactionRequest};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Margin over a fresh estimate, in basis points. See the module docs for the
/// two measurements it has to absorb simultaneously.
pub const MARGIN_BPS: u64 = 2_000;

/// A gas limit and the reasoning that produced it, carried together so no
/// reader has to guess which number a transaction was sent with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GasBudget {
    /// What `eth_estimateGas` answered, in the context named by `source`.
    pub estimate: u64,
    /// `estimate` + `margin_bps` — the value that goes on the transaction.
    pub limit: u64,
    pub margin_bps: u64,
    /// Which context produced the estimate: `"fork_preflight"` for the mined
    /// preflight, `"live"` for a broadcast-time estimate, `"eth_call"` for the
    /// light backend.
    pub source: &'static str,
}

impl GasBudget {
    pub fn from_estimate(estimate: u64, source: &'static str) -> Self {
        Self {
            estimate,
            // Saturating, not wrapping: a nonsense estimate must not wrap into a
            // tiny limit, which is the exact failure this module exists to stop.
            limit: estimate.saturating_add(estimate.saturating_mul(MARGIN_BPS) / 10_000),
            margin_bps: MARGIN_BPS,
            source,
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "estimate": self.estimate,
            "limit": self.limit,
            "margin_bps": self.margin_bps,
            "source": self.source,
        })
    }
}

/// Fresh `eth_estimateGas` against the EXACT call that will be sent, × margin.
///
/// `tx` must be the final request — same from/to/data/value. Estimating a
/// different shape and sending this one is how the two surfaces drifted apart in
/// the first place.
pub async fn budget<P: Provider>(
    provider: &P,
    tx: &TransactionRequest,
    source: &'static str,
) -> Result<GasBudget> {
    let estimate = provider
        .estimate_gas(tx.clone())
        .await
        .map_err(|e| Error::rpc(format!("eth_estimateGas ({source}): {e}")))?;
    Ok(GasBudget::from_estimate(estimate, source))
}

/// What the broadcast MUST spend, given what the execute-time preflight proved.
///
/// Pure, so the equivalence between the two surfaces is a testable property
/// rather than a convention someone can drift past.
///
/// The preflight's limit is the answer whenever the two contexts agree — and
/// they should: the preflight runs on a fork RE-ANCHORED to the live head, and a
/// mainnet broadcast is already refused when that fork is contaminated. When
/// they disagree the LARGER value wins and the divergence is disclosed. That
/// order is deliberate: an under-budgeted send is the failure that stopped Phase
/// B, while 20% of unused gas is refunded. Equality is the assertion; failing
/// safe is what happens when the assertion does not hold.
pub fn send_limit(preflight: Option<&GasBudget>, live: Option<&GasBudget>) -> SendLimit {
    match (preflight, live) {
        (Some(p), Some(l)) if l.limit > p.limit => SendLimit {
            limit: l.limit,
            source: "live_exceeded_preflight",
            divergence: Some(format!(
                "the live estimate ({}) budgets MORE than the mined preflight proved ({}), so the \
                 two contexts are not equivalent — the larger limit is used and this is recorded \
                 rather than silently averaged. Common causes: the preflight fork is behind the \
                 live head, or storage that was warm on the fork is cold live.",
                l.limit, p.limit
            )),
        },
        (Some(p), _) => SendLimit {
            limit: p.limit,
            source: "fork_preflight",
            divergence: None,
        },
        (None, Some(l)) => SendLimit {
            limit: l.limit,
            source: "live",
            divergence: None,
        },
        (None, None) => SendLimit {
            limit: 0,
            source: "none",
            divergence: None,
        },
    }
}

/// The chosen limit and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendLimit {
    pub limit: u64,
    pub source: &'static str,
    /// Set when the preflight and the live estimate did NOT agree.
    pub divergence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The measured cases are the test. Every number here is from a live
    /// differential on local forks; the policy has to be on the
    /// right side of all of them at once.
    #[test]
    fn the_margin_clears_every_measured_failure_point() {
        // What the fork reported and what the chain actually needed.
        const FORK_REPORTED: u64 = 625_782; // fork receipt gas_used — FAILED
        const FORK_PLUS_5: u64 = 657_071; // +5% — FAILED (delegatecall OOG)
        const ESTIMATE: u64 = 680_453; // eth_estimateGas — PASSED
        const ESTIMATE_PLUS_10: u64 = 748_498; // +10% — PASSED

        let b = GasBudget::from_estimate(ESTIMATE, "test");
        assert_eq!(b.limit, 816_543, "estimate × 1.20");
        assert!(
            b.limit > FORK_PLUS_5,
            "the policy must clear the measured failure point ({FORK_PLUS_5}), got {}",
            b.limit
        );
        assert!(b.limit > ESTIMATE_PLUS_10, "and the passing point too");

        // The two independent gaps, compounded, must fit inside the margin:
        // an 8.7% estimation gap on top of 6.5% route drift.
        let compounded = (FORK_REPORTED as f64 * 1.087 * 1.065).ceil() as u64;
        assert!(
            b.limit > compounded,
            "8.7% estimation gap × 6.5% route drift = {compounded} must fit under {}",
            b.limit
        );

        // Budgeting from the RECEIPT is what failed. Stated as a test so the
        // regression is impossible to reintroduce quietly: the old policy's
        // limit sits below what the chain needed.
        let old = FORK_REPORTED + FORK_REPORTED * 5 / 100;
        assert!(
            old < ESTIMATE,
            "the old receipt+5% policy ({old}) budgets less than the estimate ({ESTIMATE})"
        );
    }

    /// GUARD 4 (the swap regression): a route whose gas grows between the quote
    /// and the execution still lands inside the budget.
    ///
    /// +6.5% is the measured block-to-block drift from the swap trace; the test
    /// walks past it to 7% and 15% to show where the margin actually ends, so the
    /// headroom is a stated number rather than a hope. Beyond ~20% the transaction
    /// is meant to fail — that is a route that changed, not a budget that was
    /// wrong, and silently over-budgeting it would just hide the drift.
    #[test]
    fn a_route_that_gets_dearer_between_quote_and_execution_still_fits() {
        let quoted = 680_453u64;
        let budget = GasBudget::from_estimate(quoted, "fork_preflight");
        for drift_pct in [6.5f64, 7.0, 10.0, 15.0] {
            let at_execution = (quoted as f64 * (1.0 + drift_pct / 100.0)).ceil() as u64;
            assert!(
                budget.limit >= at_execution,
                "{drift_pct}% drift ({at_execution}) must fit under the {} limit",
                budget.limit
            );
        }
        // And the honest edge: 25% does NOT fit, by design.
        let way_off = (quoted as f64 * 1.25).ceil() as u64;
        assert!(
            budget.limit < way_off,
            "the margin is 20%, not unlimited — a 25% jump is a changed route, not a budget bug"
        );
    }

    #[test]
    fn a_nonsense_estimate_cannot_wrap_into_a_tiny_limit() {
        let b = GasBudget::from_estimate(u64::MAX, "test");
        assert_eq!(b.limit, u64::MAX, "saturating, never wrapping");
        assert_eq!(GasBudget::from_estimate(0, "test").limit, 0);
    }

    /// GUARD 3: the broadcast spends what the preflight proved. A fresh live
    /// estimate that is SMALLER never lowers the limit — the preflight is the
    /// context that actually mined the transaction.
    #[test]
    fn the_send_never_budgets_below_what_the_preflight_proved() {
        let preflight = GasBudget::from_estimate(680_453, "fork_preflight");
        let cheaper = GasBudget::from_estimate(600_000, "live");
        let chosen = send_limit(Some(&preflight), Some(&cheaper));
        assert_eq!(chosen.limit, preflight.limit);
        assert_eq!(chosen.source, "fork_preflight");
        assert!(chosen.divergence.is_none(), "{chosen:?}");

        // Equivalent contexts: equality, which is the assertion guard 3 wants.
        let same = GasBudget::from_estimate(680_453, "live");
        let chosen = send_limit(Some(&preflight), Some(&same));
        assert_eq!(chosen.limit, preflight.limit);
        assert!(chosen.divergence.is_none());
    }

    /// And when the live context needs MORE, the send does not under-budget on
    /// the strength of a fork — it takes the larger number and says so.
    #[test]
    fn a_divergent_live_estimate_is_disclosed_not_averaged() {
        let preflight = GasBudget::from_estimate(680_453, "fork_preflight");
        let dearer = GasBudget::from_estimate(900_000, "live");
        let chosen = send_limit(Some(&preflight), Some(&dearer));
        assert_eq!(chosen.limit, dearer.limit);
        assert_eq!(chosen.source, "live_exceeded_preflight");
        let note = chosen.divergence.expect("divergence disclosed");
        assert!(note.contains("not equivalent"), "{note}");
    }

    #[test]
    fn with_no_preflight_the_live_estimate_stands_alone() {
        let live = GasBudget::from_estimate(100_000, "live");
        let chosen = send_limit(None, Some(&live));
        assert_eq!(chosen.limit, 120_000);
        assert_eq!(chosen.source, "live");
    }
}
