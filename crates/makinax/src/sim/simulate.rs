//! Simulation entry point: tier 1 = eth_call (fast, expected returns + gas),
//! tier 2 = anvil fork (full balance diffs, snapshot/revert), tier 3 =
//! Tenderly (optional, `tenderly` feature). Every result carries
//! operating_mode, block, and (on success) a block-TTL'd sim_receipt.
//! Failed simulations return success=false with a DECODED revert — only
//! infrastructure problems return Err.
//!
//! What a simulation result is FOR — whether a call may be sent without
//! one, what a stale receipt means — is the caller's decision.

use alloy::{
    network::TransactionBuilder,
    primitives::{Address, TxKind, U256, hex},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    sol_types::SolCall,
};
use serde::{Deserialize, Serialize};

use crate::{
    build::{BuiltCall, call_key},
    chain::{
        Providers, RootProvider,
        module::{IERC20, IMakinaXModule},
        taxonomy::{DecodedError, decode_revert},
    },
    config::{MakinaxConfig, ModuleConfig},
    error::{Error, Result},
    sim::{
        fork::{ExecTarget, ForkManager},
        gas,
        receipts::{SimReceipt, SimReceiptStore},
        trace, weiroll_diag,
    },
};

/// The Tenderly client a simulation may use. Without the `tenderly` feature
/// the only possible value is `None` (`Infallible` has no inhabitants), so
/// the signature of [`simulate`] is the same in every build.
#[cfg(feature = "tenderly")]
pub type TenderlyRef<'a> = Option<&'a crate::sim::tenderly::TenderlyClient>;
#[cfg(not(feature = "tenderly"))]
pub type TenderlyRef<'a> = Option<&'a std::convert::Infallible>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SimBackend {
    EthCall,
    Fork,
    Tenderly,
}

impl SimBackend {
    pub fn label(&self) -> &'static str {
        match self {
            SimBackend::EthCall => "eth_call",
            SimBackend::Fork => "fork",
            SimBackend::Tenderly => "tenderly",
        }
    }
}

/// THE fallback-safety predicate (centralized, unit-tested against both kinds).
///
/// Was this Fork backend result an INFRASTRUCTURE failure — the fork could not
/// *evaluate* the tx (RPC/transport error, "failed to get storage", spawn/
/// timeout, 429, snapshot/impersonate/send failures)? Those are eligible for
/// an automatic fallback to a higher-fidelity-available backend.
///
/// Returns FALSE for a genuine decoded revert or a mined-tx revert
/// (`fork_execution_reverted` included): those describe a tx that WOULD revert
/// on-chain and must NEVER be masked by a lighter backend (eth_call would miss
/// a state-dependent revert and hand back false confidence). This is the crux
/// safety property — a deposit/swap that would revert is still blocked.
fn is_fork_infra_failure(res: &Result<BackendOutcome>) -> bool {
    match res {
        // Fork lifecycle / RPC transport errors are never a contract revert:
        // genuine reverts always flow through the Ok(success:false) path below.
        Err(Error::Rpc(_)) | Err(Error::ForkBackend { .. }) => true,
        Err(_) => false,
        Ok(o) => !o.success && o.infra_failure,
    }
}

/// The next-best-fidelity backend to try after a fork infra failure: Tenderly
/// when [tenderly] credentials are configured (asset-change diffs), else
/// eth_call (success/revert + gas, but no balances).
fn fallback_backend(tenderly_configured: bool) -> SimBackend {
    if tenderly_configured {
        SimBackend::Tenderly
    } else {
        SimBackend::EthCall
    }
}

/// The explicit fidelity caveat attached to a fallback result — fork-grade flow
/// evidence was unavailable, and the caller must SEE that. None when no
/// fallback occurred.
pub fn fidelity_note(fallback_from: Option<SimBackend>, used: SimBackend) -> Option<String> {
    fallback_from?;
    Some(match used {
        SimBackend::EthCall =>
            "fork simulation fell back to eth_call after an infrastructure failure: this sim is a \
             success/revert + gas check but carries NO absolute balances or Safe balance-diffs — the \
             fork-grade value-loss / flow-evidence checks were unavailable. Re-simulate on the fork \
             for full fidelity once the backend recovers."
                .to_string(),
        SimBackend::Tenderly =>
            "fork simulation fell back to Tenderly after an infrastructure failure: per-token asset \
             deltas are present but absolute balances are not — Tenderly reports deltas only."
                .to_string(),
        SimBackend::Fork => return None,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ExpectedChange {
    /// managePosition return: position value after (accounting currency raw units).
    pub value: String,
    /// Signed change vs before.
    pub change: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BalanceDiff {
    pub token: String,
    /// Absolute balances — known on the anvil-fork backend; None on the
    /// Tenderly backend (asset changes carry deltas only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Signed delta (after - before), decimal string. The flow evidence.
    pub delta: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallEcho {
    pub from: String,
    pub to: String,
    pub data: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimulationResult {
    pub sim_receipt: SimReceipt,
    pub success: bool,
    /// OPEN | FENCED | WALLED | UNKNOWN. Callers must not miss this: OPEN has
    /// no on-chain value checks; FENCED checks swaps and bridges (loss bps,
    /// cooldowns, bridge recipients) but not positions; only WALLED adds the
    /// position increase/decrease checks, the instruction cooldown and
    /// mandatory accounting (MakinaXModule.sol at makina-x e704cfa).
    pub operating_mode: String,
    pub block: u64,
    pub backend: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_estimate: Option<u64>,
    /// The gas LIMIT policy for this call — a fresh estimate × margin.
    ///
    /// Distinct from `gas_estimate`, which is a measurement (the fork receipt's
    /// `gas_used`, Tenderly's figure). A measurement is the number that starves
    /// a nested frame when used as a budget. When the fork backend produced
    /// this, the preflight transaction was sent with exactly this limit — so a
    /// broadcast that spends it sends the transaction that was proven.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_budget: Option<gas::GasBudget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<ExpectedChange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub balance_diffs: Vec<BalanceDiff>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DecodedError>,
    /// PROVENANCE of the state this answer was computed against.
    ///
    /// Keyed on CONTAMINATION, not endpoint class: a pristine dev fork of
    /// mainnet state is a faithful mirror, and what makes an answer fiction is
    /// dev executions having been applied to it. Endpoint class would mis-mark
    /// clean dev sims as suspect AND could mark a contaminated one as fine.
    ///   `clean`            — the fork re-anchors; this mirrors the live chain.
    ///   `dev_contaminated` — N dev executions applied; balances and positions
    ///                        here are NOT the live chain's.
    ///   `attached_dev_endpoint` — an externally-managed dev RPC; its state is
    ///                        whatever that endpoint says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_state: Option<String>,
    /// Present only when `fork_state` is not `clean`: what that means for the
    /// reader, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_state_note: Option<String>,
    /// Set to `"fork"` when the fork backend hit an INFRASTRUCTURE failure and
    /// this result was produced by an automatic fallback to `backend` instead.
    /// The fidelity is explicit, never silent (see [`fidelity_note`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sim_backend_fallback_from: Option<String>,
    /// Present on a fallback: states which flow-evidence checks the lighter
    /// backend could NOT perform (eth_call carries no balances at all;
    /// Tenderly carries deltas but no absolutes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fidelity_note: Option<String>,
    pub call: CallEcho,
    /// The raw logs of the simulated transaction's receipt — present on the
    /// fork backend only (eth_call and Tenderly produce no receipt). For a
    /// caller that reads chain-specific evidence out of them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<alloy::rpc::types::Log>,
}

struct BackendOutcome {
    logs: Vec<alloy::rpc::types::Log>,
    success: bool,
    gas_estimate: Option<u64>,
    expected: Option<ExpectedChange>,
    balance_diffs: Vec<BalanceDiff>,
    error: Option<DecodedError>,
    block: u64,
    /// True ONLY when the backend could not EVALUATE the tx — an RPC/transport
    /// failure with no revert data (rpc_error, "failed to get storage", …).
    /// A genuine decoded revert or a MINED-tx revert sets this false: the tx
    /// would fail on-chain and must never be masked by a lighter backend. This
    /// is the flag `is_fork_infra_failure` reads to decide fork→fallback.
    infra_failure: bool,
    /// The gas budget this backend USED (fork) or computed (eth_call).
    /// Carried out of the simulation so a broadcast can spend what the
    /// preflight proved instead of budgeting independently.
    gas_budget: Option<gas::GasBudget>,
}

/// Simulate `built` from `from` against the module named `name` (the name
/// binds the receipt, see [`call_key`]).
#[allow(clippy::too_many_arguments)]
pub async fn simulate(
    name: &str,
    cfg: &ModuleConfig,
    mcfg: &MakinaxConfig,
    built: &BuiltCall,
    from: Address,
    backend: SimBackend,
    providers: &Providers,
    forks: &ForkManager,
    receipts: &SimReceiptStore,
    tenderly: TenderlyRef<'_>,
    // What this simulation is REHEARSING. A mainnet-targeted sim may not run
    // on a dev endpoint or a dev-contaminated fork — `fresh_endpoint` enforces
    // that structurally, so no caller can opt out.
    target: ExecTarget,
) -> Result<SimulationResult> {
    let provider = providers.get(&cfg.chain)?;
    let module = IMakinaXModule::new(cfg.module, provider);
    // Accounting/simulation is mode-agnostic but the caller must SEE the mode.
    let operating_mode = match module.operatingMode().call().await {
        Ok(m) => crate::reads::module_state::mode_label(m),
        Err(_) => match module.lockdownMode().call().await {
            Ok(true) => "WALLED".into(),
            Ok(false) => "OPEN".into(),
            Err(_) => "UNKNOWN".into(),
        },
    };

    // Fork is the flakiest dependency in the system. When it hits an
    // INFRASTRUCTURE failure (not a revert), automatically fall back to the
    // next-best-available backend so a naive strategy prompt doesn't silently
    // drop the action. This is read-only (simulation only) — execution is
    // untouched. A genuine revert is never rescued (see is_fork_infra_failure).
    let mut backend_used = backend;
    let mut fallback_from: Option<SimBackend> = None;
    let mut outcome = match backend {
        SimBackend::EthCall => eth_call_sim(cfg, built, from, providers).await?,
        SimBackend::Tenderly => tenderly_sim(cfg, built, from, tenderly, providers).await?,
        SimBackend::Fork => {
            let fork_res = fork_sim(cfg, built, from, forks, mcfg, target).await;
            if mcfg.fork.sim_fallback && is_fork_infra_failure(&fork_res) {
                let next = fallback_backend(tenderly.is_some());
                let detail = match &fork_res {
                    Err(e) => e.to_string(),
                    Ok(o) => o
                        .error
                        .as_ref()
                        .map(|e| format!("[{}] {}", e.code, e.explanation))
                        .unwrap_or_else(|| "unspecified fork infrastructure failure".into()),
                };
                tracing::warn!(
                    module = name,
                    chain = %cfg.chain,
                    fallback_to = next.label(),
                    detail = %detail,
                    "fork simulation infrastructure failure; falling back to the next-best-available backend (read-only; execution untouched)"
                );
                forks.record_fallback(&cfg.chain);
                let next_outcome = match next {
                    SimBackend::Tenderly => {
                        tenderly_sim(cfg, built, from, tenderly, providers).await?
                    }
                    _ => eth_call_sim(cfg, built, from, providers).await?,
                };
                backend_used = next;
                fallback_from = Some(SimBackend::Fork);
                next_outcome
            } else {
                // Genuine revert, or fallback disabled: surface exactly as before.
                fork_res?
            }
        }
    };

    let key = call_key(&cfg.chain, name, from, built);
    let sim_receipt = if outcome.success {
        receipts.issue(key, outcome.block)
    } else {
        SimReceipt {
            id: "sim_failed".into(),
            issued_block: outcome.block,
            expires_block: outcome.block,
        }
    };

    // Advisory diagnosis: when the failure is a weiroll
    // ExecutionFailed, probe the failing target as an ERC-4626 vault for the
    // Safe. Probe results are diagnosis, not a gate — the sim stays the
    // source of truth.
    if let Some(err) = outcome.error.as_mut()
        && err.code == "weiroll_execution_failed"
        && let Some(target) = err
            .details
            .as_ref()
            .and_then(|d| d["target"].as_str())
            .and_then(|t| t.parse::<Address>().ok())
        && let Some(probe) = erc4626_probe(provider, target, cfg.safe).await
        && let Some(d) = err.details.as_mut().and_then(|d| d.as_object_mut())
    {
        if let Some(note) = probe.get("note").and_then(|n| n.as_str()) {
            err.explanation = format!("{} — {}", err.explanation, note);
        }
        d.insert("erc4626_probe".into(), probe);
    }

    // A target that reverts with a bare `require` gives weiroll nothing to
    // report, and "(empty revert reason)" is the same answer for a top-up that
    // is short by a hair and one aimed at the wrong contract. Recover the
    // command list from the calldata and name the token that is short.
    if let Some(err) = outcome.error.as_mut()
        && err.code == "weiroll_execution_failed"
        && let Some(failing) = err
            .details
            .as_ref()
            .and_then(|d| d["target"].as_str())
            .and_then(|t| t.parse::<Address>().ok())
    {
        let wanted: Vec<_> = weiroll_diag::instructions_in(&built.data)
            .iter()
            .flat_map(|i| {
                weiroll_diag::requested_amounts(
                    &weiroll_diag::decode_commands(&i.commands, &i.state),
                    failing,
                )
            })
            .collect();
        let short = weiroll_diag::shortfalls(provider, cfg.safe, &wanted).await;
        if !short.is_empty() {
            err.explanation = format!(
                "{} — {}",
                err.explanation,
                short
                    .iter()
                    .map(|s| s.sentence())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            err.remediation = Some(
                "Fund the Safe with the missing token, or run the action for an amount it can \
                 cover."
                    .into(),
            );
            let rows: Vec<_> = short
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "token": format!("{:?}", s.token),
                        "symbol": s.symbol,
                        "decimals": s.decimals,
                        "held_base_units": s.held.to_string(),
                        "needed_base_units": s.needed.to_string(),
                    })
                })
                .collect();
            if let Some(d) = err.details.as_mut().and_then(|d| d.as_object_mut()) {
                d.insert("insufficient_balance".into(), serde_json::json!(rows));
            }
        }
    }

    // Only fork-backed answers have fork provenance; eth_call and Tenderly
    // read the live chain, so labelling them would be noise.
    let (fork_state, fork_state_note) = if backend_used == SimBackend::Fork {
        let (st, note) = fork_provenance(forks, mcfg, &cfg.chain);
        (Some(st), note)
    } else {
        (None, None)
    };

    Ok(SimulationResult {
        logs: outcome.logs,
        fork_state,
        fork_state_note,
        sim_receipt,
        success: outcome.success,
        operating_mode,
        block: outcome.block,
        // The backend that ACTUALLY produced this receipt (the fallback one,
        // if a fallback occurred) — never the merely-requested backend.
        backend: backend_used.label().to_string(),
        gas_estimate: outcome.gas_estimate,
        gas_budget: outcome.gas_budget,
        expected: outcome.expected,
        balance_diffs: outcome.balance_diffs,
        error: outcome.error,
        sim_backend_fallback_from: fallback_from.map(|b| b.label().to_string()),
        fidelity_note: fidelity_note(fallback_from, backend_used),
        call: CallEcho {
            from: format!("{:?}", from),
            to: format!("{:?}", built.to),
            data: format!("0x{}", hex::encode(&built.data)),
            value: built.value.to_string(),
        },
    })
}

fn tx_request(built: &BuiltCall, from: Address) -> TransactionRequest {
    TransactionRequest::default()
        .with_from(from)
        .with_kind(TxKind::Call(built.to))
        .with_input(built.data.clone())
        .with_value(built.value)
}

/// Extract revert bytes from an alloy call error, if any.
fn revert_bytes(e: &alloy::transports::TransportError) -> Option<Vec<u8>> {
    e.as_error_resp()
        .and_then(|payload| payload.as_revert_data())
        .map(|b| b.to_vec())
}

fn decode_call_error(e: alloy::transports::TransportError) -> DecodedError {
    match revert_bytes(&e) {
        Some(bytes) => decode_revert(&bytes),
        None => {
            DecodedError::infrastructure("rpc_error", crate::util::redact::text(&e.to_string()))
        }
    }
}

/// How long the best-effort trace of a reverted call may take before the
/// simulation answers without it.
const FAILING_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Set `details[key]`, keeping whatever the decoder already put there.
fn put_detail(d: &mut DecodedError, key: &str, value: serde_json::Value) {
    match d.details.as_mut() {
        Some(serde_json::Value::Object(obj)) => {
            obj.insert(key.into(), value);
        }
        Some(_) => {}
        None => d.details = Some(serde_json::json!({ key: value })),
    }
}

/// Best effort: trace a call that reverted and attach the frame the failure
/// started in (`details.failing_frame`), whose revert bytes a wrapper may
/// have replaced on the way out. A backend that cannot trace leaves the
/// error as it was, with `details.failing_frame_unavailable` saying why. A
/// failure without revert data is not a revert and is not traced.
async fn with_failing_frame<P: alloy::providers::Provider>(
    provider: &P,
    tx: &TransactionRequest,
    mut decoded: DecodedError,
) -> DecodedError {
    if decoded.raw == "0x" {
        return decoded;
    }
    let traced = tokio::time::timeout(FAILING_FRAME_TIMEOUT, trace::fetch_call(provider, tx))
        .await
        .unwrap_or_else(|_| Err("debug_traceCall: timed out".into()));
    match traced.map(|root| trace::failing_frame(&root)) {
        Ok(Some(frame)) => put_detail(&mut decoded, "failing_frame", frame.json()),
        Ok(None) => put_detail(
            &mut decoded,
            "failing_frame_unavailable",
            serde_json::json!("the traced call did not fail"),
        ),
        Err(e) => put_detail(
            &mut decoded,
            "failing_frame_unavailable",
            serde_json::json!(e),
        ),
    }
    decoded
}

async fn eth_call_sim(
    cfg: &ModuleConfig,
    built: &BuiltCall,
    from: Address,
    providers: &Providers,
) -> Result<BackendOutcome> {
    let provider = providers.get(&cfg.chain)?;
    let block = provider
        .get_block_number()
        .await
        .map_err(|e| Error::rpc(format!("get_block_number: {}", e)))?;
    let tx = tx_request(built, from);

    match provider.call(tx.clone()).await {
        Ok(output) => {
            let expected = if built.is_manage_position {
                IMakinaXModule::managePositionCall::abi_decode_returns(&output)
                    .ok()
                    .map(|r| ExpectedChange {
                        value: r.value.to_string(),
                        change: r.change.to_string(),
                    })
            } else {
                None
            };
            let gas_estimate = provider.estimate_gas(tx).await.ok();
            Ok(BackendOutcome {
                logs: Vec::new(),
                success: true,
                gas_estimate,
                expected,
                balance_diffs: vec![],
                error: None,
                block,
                infra_failure: false,
                // The light backend sends nothing, but it DID estimate —
                // so it can still hand the broadcast a budget under the one
                // policy rather than leaving it to alloy's marginless filler.
                gas_budget: gas_estimate.map(|e| gas::GasBudget::from_estimate(e, "eth_call")),
            })
        }
        Err(e) => {
            let decoded = with_failing_frame(provider, &tx, decode_call_error(e)).await;
            // No revert bytes ⇒ a transport/RPC failure, not a contract revert.
            let infra_failure = decoded.raw == "0x";
            Ok(BackendOutcome {
                logs: Vec::new(),
                success: false,
                gas_estimate: None,
                expected: None,
                balance_diffs: vec![],
                error: Some(decoded),
                block,
                infra_failure,
                gas_budget: None,
            })
        }
    }
}

/// The remedy that ends [`fork_provenance`]'s `dev_contaminated` note.
/// Exported so a consumer can match it exactly (and word its own remedy).
pub const DEV_CONTAMINATED_REMEDY: &str = "Start a new process for a clean fork (dev-write marks are in-memory), or configure [tenderly].";

/// PROVENANCE of the state a fork-backed answer was computed against.
///
/// Contamination, NOT endpoint class: a pristine dev fork of mainnet state is a
/// faithful mirror. What makes an answer fiction is dev executions having been
/// applied to the fork it ran on.
pub fn fork_provenance(
    forks: &ForkManager,
    mcfg: &MakinaxConfig,
    chain: &str,
) -> (String, Option<String>) {
    if mcfg.dev_rpc_urls.contains_key(chain) {
        return (
            "attached_dev_endpoint".into(),
            Some(format!(
                "computed against `dev_rpc_urls.{chain}`, an externally-managed dev endpoint — its balances and positions are whatever that endpoint holds, NOT the live chain's"
            )),
        );
    }
    let dev_writes = forks.dev_writes(chain);
    if dev_writes > 0 {
        return (
            "dev_contaminated".into(),
            Some(format!(
                "computed against a warm fork carrying {dev_writes} dev execution(s); it stopped re-anchoring when the first one landed, so balances and positions here are NOT the live chain's. {DEV_CONTAMINATED_REMEDY}"
            )),
        );
    }
    ("clean".into(), None)
}

/// The remediation a `fork_out_of_gas` diagnosis carries. Exported so a
/// consumer can match it exactly (and word its own).
pub fn out_of_gas_remediation() -> String {
    format!(
        "Re-simulate with a fresh eth_estimateGas × {}% margin budgeting both the preflight and \
         the broadcast. If this recurs at that budget, the estimate itself is below what the \
         nested chain needs — raise the margin rather than the slippage, because output was \
         never the problem.",
        100 + gas::MARGIN_BPS / 100
    )
}

/// Diagnose a mined preflight transaction that FAILED — and preserve what said
/// so, because the caller is about to revert the snapshot.
///
/// The old version of this was three wrong things at once. It replayed an
/// UNBOUNDED `eth_call` at the post-failure state, so the replay differed from
/// the transaction in both gas and block; when that replay succeeded it declared
/// the failure "state-dependent", which for an out-of-gas is a conclusion
/// manufactured by the very difference it failed to control for; and then the
/// snapshot revert discarded the transaction, its gas limit and its trace, so
/// nobody could recover what gas the production failure actually had — **the
/// diagnosis destroyed its own evidence.**
///
/// What replaces it:
///
/// 1. **The trace decides.** A `callTracer` trace names the frame that ran out
///    of gas, so a nested OOG is its own code (`fork_out_of_gas`) rather than a
///    revert. `fork_execution_reverted` keeps meaning what it says only if OOG
///    is excluded from it.
/// 2. **Like-for-like replay.** Same gas limit, and at the PARENT block — the
///    state the transaction actually met. An unbounded `eth_call` can only say
///    "this call is valid somewhere"; it can never say "this transaction would
///    have succeeded".
/// 3. **Evidence, always.** tx hash, the limit that was used, the receipt, the
///    block, and the terminal trace frame ride out in `details` whether or not
///    the classification is confident.
async fn diagnose_failed_send<P: alloy::providers::Provider>(
    provider: &P,
    sent: &TransactionRequest,
    tx_hash: alloy::primitives::B256,
    receipt: &alloy::rpc::types::TransactionReceipt,
    budget: Option<&gas::GasBudget>,
) -> DecodedError {
    let limit = sent.gas;
    let block = receipt.block_number;
    // Preserve FIRST. Everything below can fail; none of it may cost the caller
    // the facts that are already in hand.
    let mut evidence = serde_json::json!({
        "tx_hash": format!("{tx_hash:?}"),
        "gas_limit": limit,
        "gas_used": receipt.gas_used,
        "status": if receipt.status() { "success" } else { "failed" },
        "block": block,
        "gas_budget": budget.map(|b| b.json()),
        "why_this_is_here": "the fork snapshot is reverted after a preflight, which used to \
                             discard the failed transaction, its gas limit and its trace",
    });

    let tree = trace::fetch_tree(provider, tx_hash).await;
    let failing = tree.as_ref().ok().and_then(trace::failing_frame);
    let traced = tree.map(|root| trace::CallTrace {
        frames: trace::flatten(&root, 0, &[]),
    });
    match &traced {
        Ok(t) => {
            evidence["trace_frames"] = serde_json::json!(t.frames.len());
            if let Some(f) = t.terminal_frame() {
                evidence["terminal_frame"] = f.json();
            }
        }
        Err(e) => {
            evidence["trace"] = serde_json::json!(format!("unavailable: {e}"));
        }
    }

    // Like-for-like: the same budget, at the state the transaction met. The
    // parent block, not the mined one — at the mined block the transaction's own
    // effects are already applied.
    let replay_at = block.map(|b| b.saturating_sub(1));
    let mut replay = sent.clone();
    if let Some(l) = limit {
        replay = replay.with_gas_limit(l);
    }
    let replayed = match replay_at {
        Some(b) => provider.call(replay).block(b.into()).await,
        None => provider.call(replay).await,
    };
    evidence["replay"] = serde_json::json!({
        "block": replay_at,
        "gas_limit": limit,
        "like_for_like": true,
        "outcome": match &replayed {
            Ok(_) => "succeeded".to_string(),
            Err(e) => crate::util::redact::text(&format!("failed: {e}")),
        },
    });

    // Classify. Gas first: an OOG that decoded as a revert is the mislabelling
    // this function exists to end.
    let mut decoded = match traced.as_ref().ok().and_then(|t| t.starved_frame()) {
        Some(f) => {
            let mut d = DecodedError::infrastructure(
                "fork_out_of_gas",
                format!(
                    "OUT OF GAS, not a revert: the frame at depth {} ({} to {}) was given only {} \
                     gas and consumed all of it. Call chain: {}. The transaction's limit was {} \
                     and it used {}. EIP-150 forwards at most 63/64 of the gas a frame holds, so a \
                     limit that merely covers the total starves the DEEPEST call first — nothing \
                     about the action or its parameters is wrong here, the budget is.",
                    f.depth,
                    f.call_type,
                    f.to,
                    f.gas,
                    f.path_arrow(),
                    limit
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "auto".into()),
                    receipt.gas_used,
                ),
            );
            d.remediation = Some(out_of_gas_remediation());
            d
        }
        // Not gas. A like-for-like replay that reverts hands us the reason.
        None => match replayed {
            Err(e) => {
                let mut d = decode_call_error(e);
                if let Some(frame) = &failing {
                    put_detail(&mut d, "failing_frame", frame.json());
                }
                d
            }
            Ok(_) => {
                let traced_note = match &traced {
                    Ok(_) => "the trace shows no out-of-gas frame",
                    Err(_) => "the trace could not be read, so gas has NOT been ruled out",
                };
                DecodedError::infrastructure(
                    "fork_execution_reverted",
                    format!(
                        "the transaction failed on the fork, yet replaying the SAME call with the \
                         SAME gas limit at the parent block succeeds, and {traced_note}. That \
                         combination is genuinely state-dependent — for swaps typically a stale \
                         route or an expired deadline: re-quote and simulate promptly. The failed \
                         transaction's hash, limit, receipt and trace are in `details` (they used \
                         to be discarded with the fork snapshot)."
                    ),
                )
            }
        },
    };

    match decoded.details.as_mut().and_then(|d| d.as_object_mut()) {
        Some(obj) => {
            obj.insert("preflight_evidence".into(), evidence);
        }
        None => {
            decoded.details = Some(serde_json::json!({ "preflight_evidence": evidence }));
        }
    }
    decoded
}

async fn fork_sim(
    cfg: &ModuleConfig,
    built: &BuiltCall,
    from: Address,
    forks: &ForkManager,
    mcfg: &MakinaxConfig,
    target: ExecTarget,
) -> Result<BackendOutcome> {
    let url = forks.fresh_endpoint(&cfg.chain, mcfg, target).await?;
    let parsed = url
        .parse::<reqwest::Url>()
        .map_err(|e| Error::Config(format!("fork endpoint url: {}", e)))?;
    let provider = ProviderBuilder::new().connect_http(parsed);

    let block = provider
        .get_block_number()
        .await
        .map_err(|e| Error::rpc(format!("fork get_block_number: {}", e)))?;

    let snap: serde_json::Value = provider
        .raw_request("evm_snapshot".into(), ())
        .await
        .map_err(|e| Error::rpc(format!("evm_snapshot: {}", e)))?;

    // Impersonate + fund the sender so the fork accepts the tx.
    let _: serde_json::Value = provider
        .raw_request("anvil_impersonateAccount".into(), (from,))
        .await
        .map_err(|e| Error::rpc(format!("anvil_impersonateAccount: {}", e)))?;
    let _: serde_json::Value = provider
        .raw_request(
            "anvil_setBalance".into(),
            (from, U256::from(10u64).pow(U256::from(18u64))),
        )
        .await
        .map_err(|e| Error::rpc(format!("anvil_setBalance: {}", e)))?;

    // Balances before (Safe), for diffing.
    //
    // A failed read is NOT a zero balance: `delta = after - before` with
    // `before` fabricated as 0 yields a large positive delta — a **phantom
    // inflow** — which a caller classifying flows would read as an exit.
    // Refuse to produce flow evidence that could not be measured.
    let mut before = Vec::with_capacity(built.affected_tokens.len());
    for t in &built.affected_tokens {
        let bal = IERC20::new(*t, &provider)
            .balanceOf(cfg.safe)
            .call()
            .await
            .map_err(|e| {
                Error::rpc(format!(
                    "pre-state balanceOf({t}) on the fork: {e} — the simulation cannot produce \
                     flow evidence for a token whose balance could not be read"
                ))
            })?;
        before.push(bal);
    }

    // Expected returns via eth_call on the fork FIRST (sendTransaction has no
    // return data). A revert here fails the whole simulation.
    let tx = tx_request(built, from);
    let expected = match provider.call(tx.clone()).await {
        Ok(output) if built.is_manage_position => {
            IMakinaXModule::managePositionCall::abi_decode_returns(&output)
                .ok()
                .map(|r| ExpectedChange {
                    value: r.value.to_string(),
                    change: r.change.to_string(),
                })
        }
        Ok(_) => None,
        Err(e) => {
            let decoded = with_failing_frame(&provider, &tx, decode_call_error(e)).await;
            // No revert bytes ⇒ the fork could not evaluate the tx (RPC error,
            // "failed to get storage") — infra, eligible for fallback. Revert
            // bytes ⇒ a genuine revert the chain would produce — NEVER fall back.
            let infra_failure = decoded.raw == "0x";
            let _: serde_json::Value = provider
                .raw_request("evm_revert".into(), (snap.clone(),))
                .await
                .unwrap_or_default();
            return Ok(BackendOutcome {
                logs: Vec::new(),
                success: false,
                gas_estimate: None,
                expected: None,
                balance_diffs: vec![],
                error: Some(decoded),
                block,
                infra_failure,
                gas_budget: None,
            });
        }
    };

    // BUDGET THE PREFLIGHT LIKE THE BROADCAST. A preflight with no gas limit
    // lets Anvil auto-estimate it, proving a transaction nobody is going to
    // send. One policy, both surfaces: a fresh estimate against the exact call
    // × the margin (see `sim::gas` for the two measurements it absorbs).
    //
    // A failed estimate degrades to the old behaviour rather than failing the
    // simulation: `gas_budget: None` then makes the LIVE path take its own fresh
    // estimate instead of inheriting a number this fork could not produce.
    let gas_budget = match gas::budget(&provider, &tx, "fork_preflight").await {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "fork preflight could not estimate gas; sending unbudgeted and letting the \
                 broadcast estimate for itself"
            );
            None
        }
    };
    let sent = match &gas_budget {
        Some(b) => tx.clone().with_gas_limit(b.limit),
        None => tx.clone(),
    };

    // Send for real (impersonated) to observe balance effects + gas.
    let tx_hash: alloy::primitives::B256 = provider
        .raw_request("eth_sendTransaction".into(), (sent.clone(),))
        .await
        .map_err(|e| Error::rpc(format!("fork eth_sendTransaction: {}", e)))?;

    let mut receipt = None;
    for _ in 0..50 {
        if let Ok(Some(r)) = provider.get_transaction_receipt(tx_hash).await {
            receipt = Some(r);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let receipt = receipt.ok_or_else(|| Error::Rpc("fork receipt not found".into()))?;
    let success = receipt.status();
    let gas_estimate = Some(receipt.gas_used);

    // A failed sim must ALWAYS carry a decoded explanation — and, since the preflight work,
    // one derived from the transaction that actually failed, with its evidence
    // preserved before the snapshot revert destroys it.
    let error = if success {
        None
    } else {
        Some(diagnose_failed_send(&provider, &sent, tx_hash, &receipt, gas_budget.as_ref()).await)
    };

    // Balances after → signed diffs.
    //
    // Same rule as the pre-state read, mirrored: `after` fabricated as 0 yields
    // `-before`, a phantom OUTFLOW. Neither direction of this substitution is
    // safe, so an unreadable post-state balance fails the simulation rather
    // than inventing flow evidence.
    let mut balance_diffs = Vec::with_capacity(built.affected_tokens.len());
    for (i, t) in built.affected_tokens.iter().enumerate() {
        let after = IERC20::new(*t, &provider)
            .balanceOf(cfg.safe)
            .call()
            .await
            .map_err(|e| {
                Error::rpc(format!(
                    "post-state balanceOf({t}) on the fork: {e} — refusing to report a balance \
                     delta derived from a balance that could not be read"
                ))
            })?;
        let b = before[i];
        let delta = if after >= b {
            format!("{}", after - b)
        } else {
            format!("-{}", b - after)
        };
        balance_diffs.push(BalanceDiff {
            token: format!("{:?}", t),
            before: Some(b.to_string()),
            after: Some(after.to_string()),
            delta,
        });
    }

    // Leave the fork as we found it.
    let _: serde_json::Value = provider
        .raw_request("evm_revert".into(), (snap,))
        .await
        .unwrap_or_default();
    let _: serde_json::Value = provider
        .raw_request("anvil_stopImpersonatingAccount".into(), (from,))
        .await
        .unwrap_or_default();

    // infra_failure stays false here: a mined-tx revert is a GENUINE revert
    // (the receipt proved it) even when the replay could not harvest the
    // reason — never mask it with a lighter backend (case: fork_execution_
    // reverted and its RPC-failed-replay sibling both surface, never fall back).
    Ok(BackendOutcome {
        logs: receipt.inner.logs().to_vec(),
        success,
        gas_estimate,
        expected,
        balance_diffs,
        error,
        block,
        infra_failure: false,
        // The budget the mined transaction was ACTUALLY sent with — the number
        // the broadcast must spend, or the preflight proved nothing about it.
        gas_budget,
    })
}

#[cfg(not(feature = "tenderly"))]
async fn tenderly_sim(
    _cfg: &ModuleConfig,
    _built: &BuiltCall,
    _from: Address,
    _tenderly: TenderlyRef<'_>,
    _providers: &Providers,
) -> Result<BackendOutcome> {
    Err(Error::Config(
        "backend \"tenderly\" is not compiled into this build (the `tenderly` feature)".into(),
    ))
}

#[cfg(feature = "tenderly")]
async fn tenderly_sim(
    cfg: &ModuleConfig,
    built: &BuiltCall,
    from: Address,
    tenderly: TenderlyRef<'_>,
    providers: &Providers,
) -> Result<BackendOutcome> {
    let client = tenderly.ok_or_else(|| {
        Error::Config("backend \"tenderly\" requires [tenderly] credentials in config".into())
    })?;
    let chain_id = crate::chains::service_chain_id(&cfg.chain).ok_or_else(|| {
        Error::Config(format!(
            "the Tenderly simulate API does not serve `{}`",
            cfg.chain
        ))
    })?;
    let outcome = client
        .simulate(chain_id, from, built.to, built.data.clone(), built.value)
        .await?;

    // Flow evidence: the Safe's per-token net deltas from the simulate
    // response's asset changes — same quantity the anvil fork measures by
    // balance diffing (absolutes are unknown here; deltas are the flow
    // evidence).
    let balance_diffs = diffs_from_asset_changes(cfg.safe, &outcome.asset_changes);

    // Best-effort expected returns via plain eth_call (Tenderly's response
    // has no decoded return data for us).
    let (expected, block) = match providers.get(&cfg.chain) {
        Ok(provider) => {
            // Block 0 is a real block number, so it cannot double as "we could
            // not read the head". The simulation is anchored to a block; an
            // unanchored one must not claim to be at genesis.
            let block = provider.get_block_number().await.map_err(|e| {
                Error::rpc(format!(
                    "get_block_number for the Tenderly simulation anchor: {e}"
                ))
            })?;
            let expected = if built.is_manage_position && outcome.success {
                provider
                    .call(tx_request(built, from))
                    .await
                    .ok()
                    .and_then(|output| {
                        IMakinaXModule::managePositionCall::abi_decode_returns(&output).ok()
                    })
                    .map(|r| ExpectedChange {
                        value: r.value.to_string(),
                        change: r.change.to_string(),
                    })
            } else {
                None
            };
            (expected, block)
        }
        Err(_) => (None, 0),
    };

    let error = tenderly_error(outcome.success, outcome.error_message);
    Ok(BackendOutcome {
        logs: Vec::new(),
        success: outcome.success,
        gas_estimate: Some(outcome.gas_used),
        expected,
        balance_diffs,
        error,
        block,
        // Tenderly is a fallback TARGET, never the source of a fork→fallback
        // decision, so this flag is not consulted for it — false is correct
        // (Tenderly conflates infra and revert; we never claim its failure is
        // "just infra" and re-downgrade further).
        infra_failure: false,
        // Tenderly's gas_used is a measurement from ITS simulator, not an
        // estimate against the node the tx will be sent to, and a measurement is
        // exactly the wrong basis for a budget (see `sim::gas`). None makes the
        // broadcast take its own fresh estimate.
        gas_budget: None,
    })
}

/// A failed sim must ALWAYS carry an error object — a bare success:false with
/// zero diffs is undiagnosable. Tenderly sometimes
/// reports failure without an error_message; synthesize one.
pub fn tenderly_error(success: bool, message: Option<String>) -> Option<DecodedError> {
    match (success, message) {
        (_, Some(m)) => Some(DecodedError::infrastructure("tenderly_error", m)),
        (false, None) => Some(DecodedError::infrastructure(
            "tenderly_unspecified_failure",
            "Tenderly reported the simulation failed but returned no error message; \
             re-run with backend `fork` for a decoded revert"
                .to_string(),
        )),
        (true, None) => None,
    }
}

/// Probe a target as an ERC-4626 vault for `actor`. Returns None when the
/// target doesn't answer 4626 getters (not a vault). Advisory only.
pub async fn erc4626_probe(
    provider: &RootProvider,
    target: Address,
    actor: Address,
) -> Option<serde_json::Value> {
    use crate::chain::module::IERC4626;
    let vault = IERC4626::new(target, provider);
    let max_deposit = vault.maxDeposit(actor).call().await.ok()?;
    let max_withdraw = vault.maxWithdraw(actor).call().await.ok();
    let max_redeem = vault.maxRedeem(actor).call().await.ok();
    let note = if max_deposit.is_zero() {
        Some("vault deposits closed for this actor (maxDeposit = 0)")
    } else if max_withdraw == Some(U256::ZERO) && max_redeem == Some(U256::ZERO) {
        Some("vault withdrawals closed for this actor (maxWithdraw = maxRedeem = 0)")
    } else {
        None
    };
    Some(serde_json::json!({
        "max_deposit": max_deposit.to_string(),
        "max_withdraw": max_withdraw.map(|v| v.to_string()),
        "max_redeem": max_redeem.map(|v| v.to_string()),
        "note": note,
    }))
}

/// Net per-token deltas for `safe` out of Tenderly asset changes. Public so
/// a recorded-fixture test can prove parity with the anvil path's diffs.
#[cfg(feature = "tenderly")]
pub fn diffs_from_asset_changes(
    safe: Address,
    changes: &[crate::sim::tenderly::AssetChange],
) -> Vec<BalanceDiff> {
    use alloy::primitives::I256;
    use std::collections::BTreeMap;
    let safe_lc = format!("{:?}", safe).to_lowercase();
    // token address -> net signed delta
    let mut net: BTreeMap<String, I256> = BTreeMap::new();
    for c in changes {
        let Some(token) = c.token_address.as_ref() else {
            continue;
        };
        let Some(raw) = c.raw_amount.as_ref().and_then(|r| r.parse::<I256>().ok()) else {
            continue;
        };
        let token = token.to_lowercase();
        if c.from.as_deref().map(|a| a.to_lowercase()) == Some(safe_lc.clone()) {
            *net.entry(token.clone()).or_default() -= raw;
        }
        if c.to.as_deref().map(|a| a.to_lowercase()) == Some(safe_lc.clone()) {
            *net.entry(token).or_default() += raw;
        }
    }
    net.into_iter()
        .filter(|(_, d)| !d.is_zero())
        .map(|(token, d)| BalanceDiff {
            token,
            before: None,
            after: None,
            delta: d.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod fork_provenance_tests {
    use super::*;

    fn cfg_with_dev_rpc(dev: bool) -> MakinaxConfig {
        let mut cfg = MakinaxConfig::default();
        cfg.rpc_urls
            .insert("base".into(), "http://127.0.0.1:1".into());
        if dev {
            cfg.dev_rpc_urls
                .insert("base".into(), "http://127.0.0.1:9".into());
        }
        cfg
    }

    /// A CONTAMINATED fork must say so. Keyed on dev executions, not
    /// endpoint class — a pristine dev fork of mainnet state is a faithful
    /// mirror, so endpoint class would mis-mark clean dev sims as suspect AND
    /// could mark a contaminated one as fine.
    #[test]
    fn a_contaminated_fork_is_labelled_and_a_clean_one_is_not() {
        let forks = ForkManager::new();
        let cfg = cfg_with_dev_rpc(false);

        // Clean warm fork: a faithful mirror, no note.
        let (state, note) = fork_provenance(&forks, &cfg, "base");
        assert_eq!(state, "clean");
        assert!(note.is_none());

        // One dev execution and the fork stops re-anchoring — from here its
        // balances are not the live chain's, and the answer must say so.
        forks.mark_dev_write("base");
        let (state, note) = fork_provenance(&forks, &cfg, "base");
        assert_eq!(state, "dev_contaminated");
        let note = note.expect("a contaminated fork must explain itself");
        assert!(note.contains("1 dev execution"), "{note}");
        assert!(note.contains("NOT the live chain"), "{note}");
        assert!(
            note.ends_with(DEV_CONTAMINATED_REMEDY),
            "the remedy is the exported constant: {note}"
        );

        // A different chain is unaffected — contamination is per-chain.
        let (state, _) = fork_provenance(&forks, &cfg, "mainnet");
        assert_eq!(state, "clean");

        // An attached dev endpoint is its own provenance: externally managed,
        // so its state is whatever that endpoint holds.
        let (state, note) = fork_provenance(&ForkManager::new(), &cfg_with_dev_rpc(true), "base");
        assert_eq!(state, "attached_dev_endpoint");
        assert!(note.unwrap().contains("externally-managed"));
    }

    /// The structural half: a mainnet-targeted request can never RECEIVE a dev
    /// endpoint, whatever the caller.
    #[tokio::test]
    async fn a_mainnet_sim_is_refused_a_dev_target_whatever_the_caller() {
        let forks = ForkManager::new();
        let cfg = cfg_with_dev_rpc(true);

        let err = forks
            .fresh_endpoint("base", &cfg, ExecTarget::Mainnet)
            .await
            .expect_err("a mainnet sim must not run on a dev endpoint");
        assert!(
            matches!(&err, Error::MainnetOnDevEndpoint { chain } if chain == "base"),
            "{err}"
        );

        // The SAME call with a dev target is fine.
        let url = forks
            .fresh_endpoint("base", &cfg, ExecTarget::Dev)
            .await
            .expect("a dev sim may use the dev endpoint");
        assert_eq!(url, "http://127.0.0.1:9");

        // Contaminated warm fork + mainnet target => refused too.
        let clean = cfg_with_dev_rpc(false);
        forks.mark_dev_write("base");
        let err = forks
            .fresh_endpoint("base", &clean, ExecTarget::Mainnet)
            .await
            .expect_err("a mainnet sim must not run on a contaminated fork");
        assert!(
            matches!(&err, Error::MainnetOnDevFork { chain, dev_writes: 1 } if chain == "base"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolError;

    /// The preflight guards, on the diagnosis itself.
    ///
    /// The chain is mocked because the property under test is what the
    /// diagnosis CONCLUDES from a given set of facts, and those facts are the
    /// measured ones from a live Base swap (addresses other than the token are
    /// placeholders).
    mod diagnosis {
        use super::*;
        use alloy::{
            primitives::{Address, B256, Bytes, U256, address},
            providers::{ProviderBuilder, mock::Asserter},
        };

        /// The production call tree: module → Safe → router → USDC, with the
        /// innermost frame given 0x2152 gas and consuming all of it.
        fn nested_oog_trace() -> serde_json::Value {
            serde_json::json!({
                "type": "CALL", "from": "0x0000000000000000000000000000000000000f01",
                "to": "0x0000000000000000000000000000000000000f02",
                "gas": "0x1160D2", "gasUsed": "0x112189", "error": "out of gas",
                "calls": [{
                    "type": "CALL", "from": "0x0000000000000000000000000000000000000f02",
                    "to": "0x0000000000000000000000000000000000000f03",
                    "gas": "0x10E000", "gasUsed": "0x10D000", "error": "out of gas",
                    "calls": [{
                        "type": "DELEGATECALL",
                        "from": "0x0000000000000000000000000000000000000f03",
                        "to": "0x0000000000000000000000000000000000000f04",
                        "gas": "0x10C000", "gasUsed": "0x10B000", "error": "out of gas",
                        "calls": [{
                            "type": "CALL",
                            "from": "0x0000000000000000000000000000000000000f04",
                            "to": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
                            "gas": "0x2152", "gasUsed": "0x2152", "error": "out of gas"
                        }]
                    }]
                }]
            })
        }

        fn receipt(gas_used: u64, block: u64) -> alloy::rpc::types::TransactionReceipt {
            serde_json::from_value(serde_json::json!({
                "transactionHash": format!("0x{:064x}", 0xf00du64),
                "transactionIndex": "0x0",
                "blockHash": format!("0x{:064x}", 1u64),
                "blockNumber": format!("0x{block:x}"),
                "cumulativeGasUsed": format!("0x{gas_used:x}"),
                "gasUsed": format!("0x{gas_used:x}"),
                "effectiveGasPrice": "0x1",
                "from": "0x0000000000000000000000000000000000000f01",
                "to": "0x0000000000000000000000000000000000000f02",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "0".repeat(512)),
                "status": "0x0",
                "type": "0x2",
            }))
            .expect("receipt fixture")
        }

        fn sent_tx(limit: u64) -> TransactionRequest {
            TransactionRequest::default()
                .with_from(address!("0x0000000000000000000000000000000000000f01"))
                .with_kind(TxKind::Call(address!(
                    "0x0000000000000000000000000000000000000f02"
                )))
                .with_input(Bytes::from(vec![0xdeu8, 0xad, 0xbe, 0xef]))
                .with_value(U256::ZERO)
                .with_gas_limit(limit)
        }

        /// GUARD 1: the nested call shape at the old budget is classified as
        /// OUT OF GAS with the starved frame named — never
        /// `fork_execution_reverted`, which is what it used to say while calling
        /// the failure "state-dependent".
        ///
        /// GUARD 2: the evidence survives. `details.preflight_evidence` carries
        /// the gas limit, the receipt and the terminal frame — the facts the
        /// snapshot revert used to destroy.
        #[tokio::test]
        async fn a_nested_out_of_gas_is_its_own_diagnosis_with_the_frame_named() {
            let a = Asserter::new();
            a.push_success(&nested_oog_trace()); // debug_traceTransaction
            a.push_success(&Bytes::new()); // like-for-like replay: succeeds
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();

            // The old policy's budget: the fork receipt's gas_used + 5%.
            const OLD_LIMIT: u64 = 657_071;
            let budget = gas::GasBudget::from_estimate(680_453, "fork_preflight");
            let d = diagnose_failed_send(
                &provider,
                &sent_tx(OLD_LIMIT),
                B256::from(U256::from(0xf00du64)),
                &receipt(655_000, 21_000_000),
                Some(&budget),
            )
            .await;

            println!(
                "\n─── diagnosis: nested out of gas ───\n[{}] {}\nremediation: {}\n",
                d.code,
                d.explanation,
                d.remediation.as_deref().unwrap_or("-")
            );
            assert_eq!(
                d.code, "fork_out_of_gas",
                "OOG must be excluded from fork_execution_reverted, or that code stops meaning \
                 what it says"
            );
            assert!(
                !d.explanation.contains("state-dependent"),
                "the old label was manufactured by an unbounded replay: {}",
                d.explanation
            );
            // The frame that starved, named — the deepest one, not the module.
            assert!(
                d.explanation
                    .contains("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
                "the starved frame's address must appear: {}",
                d.explanation
            );
            assert!(
                d.explanation.contains("8530"),
                "the gas it got: {}",
                d.explanation
            );
            assert!(d.explanation.contains("depth 3"), "{}", d.explanation);
            assert!(d.explanation.contains("EIP-150"), "the mechanism, stated");
            // The remediation is exactly the exported text a consumer
            // matches on, and it names the margin the budget really uses.
            assert_eq!(
                d.remediation.as_deref(),
                Some(out_of_gas_remediation().as_str())
            );
            assert!(
                out_of_gas_remediation()
                    .contains(&format!("× {}% margin", 100 + gas::MARGIN_BPS / 100)),
                "{}",
                out_of_gas_remediation()
            );

            let ev = d.details.as_ref().expect("details present")["preflight_evidence"].clone();
            println!(
                "─── preserved evidence ───\n{}",
                serde_json::to_string_pretty(&ev).unwrap()
            );
            assert_eq!(
                ev["gas_limit"], OLD_LIMIT,
                "the limit that was actually used"
            );
            assert_eq!(ev["gas_used"], 655_000);
            assert_eq!(ev["status"], "failed");
            assert_eq!(ev["block"], 21_000_000);
            assert_eq!(
                ev["terminal_frame"]["to"],
                "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
            );
            assert_eq!(ev["terminal_frame"]["gas_given"], 0x2152);
            assert_eq!(ev["gas_budget"]["limit"], budget.limit);
            // Like-for-like: same limit, and the PARENT block — an unbounded
            // eth_call at the head is what produced the wrong answer before.
            assert_eq!(ev["replay"]["block"], 20_999_999);
            assert_eq!(ev["replay"]["gas_limit"], OLD_LIMIT);
            assert_eq!(ev["replay"]["like_for_like"], true);
        }

        /// A failure that is NOT gas still decodes as before — the new code path
        /// must not swallow real reverts. And the replay that says
        /// "state-dependent" now has to earn it: same gas, parent block, and a
        /// trace showing no starved frame.
        #[tokio::test]
        async fn a_revert_is_still_a_revert_and_state_dependence_must_be_earned() {
            let trace = serde_json::json!({
                "type": "CALL", "from": "0x0f9d", "to": "0x5afe",
                "gas": "0x100000", "gasUsed": "0x2000", "error": "execution reverted"
            });
            let a = Asserter::new();
            a.push_success(&trace);
            a.push_success(&Bytes::new()); // replay succeeds → genuinely state-dependent
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let d = diagnose_failed_send(
                &provider,
                &sent_tx(800_000),
                B256::ZERO,
                &receipt(8_192, 100),
                None,
            )
            .await;
            println!(
                "\n─── diagnosis: not gas ───\n[{}] {}",
                d.code, d.explanation
            );
            assert_eq!(d.code, "fork_execution_reverted");
            assert!(
                d.explanation.contains("SAME gas limit") && d.explanation.contains("parent block"),
                "the claim now states the controls it was made under: {}",
                d.explanation
            );
            assert!(
                d.explanation.contains("no out-of-gas frame"),
                "and that gas was ruled out: {}",
                d.explanation
            );
            // Evidence rides out regardless of classification.
            assert!(
                d.details.as_ref().expect("details")["preflight_evidence"]["tx_hash"].is_string()
            );
        }

        /// A backend with no tracer must NOT let the diagnosis claim gas was
        /// ruled out.
        /// Unverified is not clean — the same rule the setup probes live by.
        #[tokio::test]
        async fn a_missing_trace_never_becomes_evidence_about_the_failure() {
            let a = Asserter::new();
            a.push_failure_msg("the method debug_traceTransaction does not exist");
            a.push_success(&Bytes::new()); // replay succeeds
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let d = diagnose_failed_send(
                &provider,
                &sent_tx(800_000),
                B256::ZERO,
                &receipt(799_000, 500),
                None,
            )
            .await;
            println!(
                "\n─── diagnosis: trace unavailable ───\n[{}] {}",
                d.code, d.explanation
            );
            assert!(
                d.explanation.contains("gas has NOT been ruled out"),
                "a backend limitation must not read as a finding: {}",
                d.explanation
            );
            let ev = &d.details.as_ref().expect("details")["preflight_evidence"];
            assert!(
                ev["trace"].as_str().unwrap().starts_with("unavailable:"),
                "{ev}"
            );
            // And the limit is still preserved: that is the one number an operator
            // could not recover from the production failure.
            assert_eq!(ev["gas_limit"], 800_000);
        }

        /// A batch step reverts with a bare custom error; the batch wraps it,
        /// and its VM writes "Unknown" for the reason. The trace's failing
        /// frame keeps the original bytes so a consumer can decode them.
        fn wrapped_step_trace() -> serde_json::Value {
            serde_json::json!({
                "type": "CALL", "from": "0x0000000000000000000000000000000000000f01",
                "to": "0x0000000000000000000000000000000000000f02",
                "gas": "0x100000", "gasUsed": "0x9000", "error": "execution reverted",
                "output": "0x00",
                "calls": [{
                    "type": "CALL", "from": "0x0000000000000000000000000000000000000f02",
                    "to": "0x0000000000000000000000000000000000000f05",
                    "gas": "0x8000", "gasUsed": "0x400", "error": "execution reverted",
                    "output": "0x1fbaba35"
                }]
            })
        }

        fn vm_unknown_revert() -> Vec<u8> {
            use alloy::sol_types::SolError;
            crate::chain::taxonomy::IWeirollVMErrors::ExecutionFailed {
                command_index: U256::from(1u64),
                target: address!("0x0000000000000000000000000000000000000f05"),
                message: "Unknown".into(),
            }
            .abi_encode()
        }

        fn revert_payload(data: &[u8]) -> alloy::rpc::json_rpc::ErrorPayload {
            alloy::rpc::json_rpc::ErrorPayload {
                code: 3,
                message: "execution reverted".into(),
                data: Some(
                    serde_json::value::to_raw_value(&format!(
                        "0x{}",
                        alloy::primitives::hex::encode(data)
                    ))
                    .unwrap(),
                ),
            }
        }

        fn amount_too_low() -> crate::chain::taxonomy::DeclaredError {
            crate::chain::taxonomy::DeclaredError {
                signature: "AmountTooLow()".into(),
                description: Some("The amount is below the minimum.".into()),
                contracts: vec![],
            }
        }

        #[tokio::test]
        async fn a_mined_wrapped_revert_keeps_the_frame_it_started_in() {
            let a = Asserter::new();
            a.push_success(&wrapped_step_trace()); // debug_traceTransaction
            a.push_failure(revert_payload(&vm_unknown_revert())); // replay reverts
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let d = diagnose_failed_send(
                &provider,
                &sent_tx(800_000),
                B256::ZERO,
                &receipt(9_000, 100),
                None,
            )
            .await;
            assert_eq!(d.code, "weiroll_execution_failed");
            assert!(!d.explanation.contains("Unknown"), "{}", d.explanation);
            let frame = &d.details.as_ref().unwrap()["failing_frame"];
            assert_eq!(frame["output"], "0x1fbaba35");
            assert_eq!(frame["to"], "0x0000000000000000000000000000000000000f05");
            let inner = crate::chain::taxonomy::decode_failing_frame(&d, &[amount_too_low()])
                .expect("a frame to decode");
            assert_eq!(
                inner.explanation,
                "AmountTooLow: The amount is below the minimum."
            );
        }

        /// A call that reverted before it was ever sent is traced with
        /// `debug_traceCall`; its failing frame rides on the decoded error.
        #[tokio::test]
        async fn a_reverted_call_gets_its_failing_frame_attached() {
            let a = Asserter::new();
            a.push_success(&wrapped_step_trace()); // debug_traceCall
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let decoded = decode_revert(&vm_unknown_revert());
            let d = with_failing_frame(&provider, &sent_tx(1), decoded).await;
            assert_eq!(d.code, "weiroll_execution_failed");
            let det = d.details.as_ref().unwrap();
            assert_eq!(
                det["command_index"], "1",
                "the decoder's own details are kept"
            );
            assert_eq!(det["failing_frame"]["output"], "0x1fbaba35");
        }

        /// No tracer on the backend: the error is unchanged apart from a note,
        /// and nothing about the failure is inferred from the missing trace.
        #[tokio::test]
        async fn an_untraceable_call_keeps_its_error_and_says_why() {
            let a = Asserter::new();
            a.push_failure_msg("the method debug_traceCall does not exist");
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let decoded = decode_revert(&vm_unknown_revert());
            let d = with_failing_frame(&provider, &sent_tx(1), decoded.clone()).await;
            assert_eq!(d.code, decoded.code);
            assert_eq!(d.explanation, decoded.explanation);
            let det = d.details.as_ref().unwrap();
            assert!(det.get("failing_frame").is_none());
            assert!(
                det["failing_frame_unavailable"]
                    .as_str()
                    .unwrap()
                    .contains("debug_traceCall")
            );
            assert!(
                crate::chain::taxonomy::decode_failing_frame(&d, &[amount_too_low()]).is_none()
            );
        }

        /// A tracer that never answers: the simulation still answers, within
        /// the bound, with the error unchanged and a note saying the trace
        /// timed out.
        #[tokio::test(start_paused = true)]
        async fn a_trace_that_never_answers_is_abandoned_within_the_bound() {
            use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket};
            use alloy::transports::{TransportError, TransportFut};
            use std::task::{Context, Poll};

            #[derive(Clone)]
            struct Silent;
            impl tower::Service<RequestPacket> for Silent {
                type Response = ResponsePacket;
                type Error = TransportError;
                type Future = TransportFut<'static>;
                fn poll_ready(
                    &mut self,
                    _: &mut Context<'_>,
                ) -> Poll<std::result::Result<(), Self::Error>> {
                    Poll::Ready(Ok(()))
                }
                fn call(&mut self, _: RequestPacket) -> Self::Future {
                    Box::pin(std::future::pending())
                }
            }
            let provider = ProviderBuilder::new()
                .connect_client(alloy::rpc::client::RpcClient::new(Silent, true))
                .erased();
            let decoded = decode_revert(&vm_unknown_revert());
            let started = tokio::time::Instant::now();
            let d = with_failing_frame(&provider, &sent_tx(1), decoded.clone()).await;
            let waited = started.elapsed();
            assert!(
                waited >= FAILING_FRAME_TIMEOUT && waited < FAILING_FRAME_TIMEOUT * 2,
                "{waited:?}"
            );
            assert_eq!(d.code, decoded.code);
            assert_eq!(
                d.details.as_ref().unwrap()["failing_frame_unavailable"],
                "debug_traceCall: timed out"
            );
        }

        /// A failure with no revert data is a transport failure, not a revert:
        /// nothing is traced.
        #[tokio::test]
        async fn a_transport_failure_is_not_traced() {
            let a = Asserter::new(); // any request would fail the test's expectation below
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let infra = DecodedError::infrastructure("rpc_error", "timeout".into());
            let d = with_failing_frame(&provider, &sent_tx(1), infra).await;
            assert!(d.details.is_none(), "{:?}", d.details);
        }

        /// The evidence is preserved even when the transaction had NO explicit
        /// limit — the old shape, which is exactly the state a row written
        /// by an older build is in.
        #[tokio::test]
        async fn an_unbudgeted_send_still_reports_what_it_had() {
            let a = Asserter::new();
            a.push_success(&nested_oog_trace());
            a.push_failure_msg("execution reverted");
            let provider = ProviderBuilder::new().connect_mocked_client(a).erased();
            let unbudgeted = TransactionRequest::default()
                .with_from(Address::ZERO)
                .with_kind(TxKind::Call(Address::ZERO))
                .with_input(Bytes::new())
                .with_value(U256::ZERO);
            let d = diagnose_failed_send(&provider, &unbudgeted, B256::ZERO, &receipt(1, 1), None)
                .await;
            assert_eq!(d.code, "fork_out_of_gas");
            assert!(
                d.explanation.contains("limit was auto"),
                "an absent limit is stated as absent, not as zero: {}",
                d.explanation
            );
        }
    }

    #[test]
    fn backend_labels() {
        assert_eq!(
            serde_json::from_str::<SimBackend>("\"eth_call\"")
                .unwrap()
                .label(),
            "eth_call"
        );
        assert_eq!(
            serde_json::from_str::<SimBackend>("\"fork\"")
                .unwrap()
                .label(),
            "fork"
        );
        assert_eq!(
            serde_json::from_str::<SimBackend>("\"tenderly\"")
                .unwrap()
                .label(),
            "tenderly"
        );
    }

    fn outcome(success: bool, error: Option<DecodedError>, infra_failure: bool) -> BackendOutcome {
        BackendOutcome {
            logs: Vec::new(),
            gas_budget: None,
            success,
            gas_estimate: None,
            expected: None,
            balance_diffs: vec![],
            error,
            block: 100,
            infra_failure,
        }
    }

    /// THE crux safety property: a call that reverts on the fork with a decoded
    /// reason is NOT classified as infra — so it never falls back, and the
    /// decoded revert is surfaced. A deposit/swap that would revert on-chain is
    /// still blocked, never rescued by a lighter backend.
    #[test]
    fn genuine_decoded_revert_is_never_infra() {
        // A real revert (has revert bytes, known code).
        let revert = decode_revert(
            &alloy::sol_types::Revert::from("ERC4626: deposit more than max").abi_encode(),
        );
        assert!(
            revert.raw != "0x" && revert.known,
            "sanity: a real revert has bytes"
        );
        let res: Result<BackendOutcome> = Ok(outcome(false, Some(revert), false));
        assert!(
            !is_fork_infra_failure(&res),
            "a genuine revert must NEVER fall back"
        );

        // An UNKNOWN selector is still a revert (has bytes) — also never infra.
        let unknown = decode_revert(&[0xde, 0xad, 0xbe, 0xef, 0x00]);
        assert!(!unknown.known && unknown.raw != "0x");
        let res: Result<BackendOutcome> = Ok(outcome(false, Some(unknown), false));
        assert!(
            !is_fork_infra_failure(&res),
            "an unknown-selector revert is still a revert"
        );
    }

    /// `fork_execution_reverted` (eth_call passed, mined tx reverted) is a REAL
    /// state-dependent revert, not infra — it must re-quote, never fall back to
    /// eth_call (which would miss it). infra_failure is false at its source.
    #[test]
    fn fork_execution_reverted_is_never_infra() {
        let e = DecodedError::infrastructure(
            "fork_execution_reverted",
            "eth_call passed but the mined tx reverted".into(),
        );
        // Even though it was built via infrastructure() (raw == "0x"), the fork
        // constructor sets infra_failure=false for any mined-tx revert.
        let res: Result<BackendOutcome> = Ok(outcome(false, Some(e), false));
        assert!(
            !is_fork_infra_failure(&res),
            "state-dependent revert must re-quote, not fall back"
        );
    }

    /// A fork infrastructure failure (no revert data — the fork could not
    /// evaluate the tx) IS eligible for fallback, in both its shapes.
    #[test]
    fn fork_infra_failures_are_eligible() {
        // Shape 1: Ok(success:false) with an infra-coded decoded error.
        let rpc = DecodedError::infrastructure(
            "rpc_error",
            "failed to get storage for module address".into(),
        );
        let res: Result<BackendOutcome> = Ok(outcome(false, Some(rpc), true));
        assert!(is_fork_infra_failure(&res));

        // Shape 2: hard Err from the fork lifecycle / RPC transport.
        assert!(is_fork_infra_failure(&Err(Error::Rpc(
            "fork get_storage: timeout".into()
        ))));
        assert!(is_fork_infra_failure(&Err(Error::ForkBackend {
            chain: "mainnet".into(),
            phase: "reset".into(),
            fork_source: "https://x".into(),
            detail: "429".into(),
        })));
    }

    /// A successful sim is not a failure; unrelated hard errors do not trigger
    /// fallback (only fork lifecycle/RPC errors do).
    #[test]
    fn success_and_unrelated_errors_do_not_fall_back() {
        assert!(!is_fork_infra_failure(&Ok(outcome(true, None, false))));
        assert!(!is_fork_infra_failure(&Err(Error::Config(
            "bad config".into()
        ))));
        assert!(!is_fork_infra_failure(&Err(Error::InvalidParams(
            "nope".into()
        ))));
    }

    #[test]
    fn fallback_backend_prefers_tenderly_when_configured() {
        assert_eq!(fallback_backend(true), SimBackend::Tenderly);
        assert_eq!(fallback_backend(false), SimBackend::EthCall);
    }

    /// Fidelity is explicit, never silent: a fallback carries a note naming
    /// which flow-evidence the lighter backend could not produce; no fallback
    /// ⇒ no note.
    #[test]
    fn fidelity_note_is_present_only_on_fallback() {
        assert!(fidelity_note(None, SimBackend::Fork).is_none());
        assert!(fidelity_note(None, SimBackend::EthCall).is_none());
        let eth = fidelity_note(Some(SimBackend::Fork), SimBackend::EthCall).unwrap();
        assert!(eth.contains("eth_call") && eth.contains("NO absolute balances"));
        let ten = fidelity_note(Some(SimBackend::Fork), SimBackend::Tenderly).unwrap();
        assert!(ten.contains("Tenderly") && ten.contains("deltas"));
    }

    /// A call that failed without revert data is reported as
    /// infrastructure with the transport's text, minus any endpoint key.
    #[test]
    fn a_transport_failure_is_decoded_without_the_endpoint_key() {
        let e = alloy::transports::TransportErrorKind::custom_str(
            "error sending request for url (https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr)",
        );
        assert!(e.to_string().contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr"));
        let decoded = decode_call_error(e);
        assert_eq!(decoded.code, "rpc_error");
        assert!(
            !decoded.explanation.contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr"),
            "{}",
            decoded.explanation
        );
        assert!(
            decoded
                .explanation
                .contains("https://rpc.example.com/v2/<redacted>"),
            "{}",
            decoded.explanation
        );
    }
}
