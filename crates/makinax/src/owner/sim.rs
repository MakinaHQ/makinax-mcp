//! Fork execution of owner batches AS the Safe — the shared machinery every
//! producer of an owner batch can prove it through. Producers drive a
//! [`SafeForkSession`] stepwise: acquire → apply batch(es) → run their own
//! post-state assertions against `session.url()` → teardown. Infra failures
//! are STRINGS (the caller reports "NOT verified", never a false claim);
//! genuine reverts carry the taxonomy-decoded reason and the call index.

use alloy::{
    network::TransactionBuilder,
    primitives::{Address, B256, TxKind, U256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
};

use crate::{
    chain::taxonomy,
    config::MakinaxConfig,
    sim::fork::{ExecTarget, ForkManager},
};

use super::batch::TxBuilderBatch;

#[derive(Debug)]
pub enum ApplyFailure {
    /// A call genuinely REVERTED — the artifact must not be delivered.
    Reverted {
        call_index: usize,
        call: String,
        decoded: String,
    },
    /// Transport/fork-lifecycle problem — could not simulate.
    Infra(String),
}

pub struct SafeForkSession {
    url: String,
    provider: DynProvider<alloy::network::Ethereum>,
    safe: Address,
    snap_id: String,
}

impl SafeForkSession {
    /// Acquire a fresh fork endpoint, snapshot it, impersonate + fund the
    /// Safe. `Err(String)` = infra (report NOT-verified upstream).
    pub async fn acquire(
        cfg: &MakinaxConfig,
        forks: &ForkManager,
        chain: &str,
        safe: Address,
    ) -> Result<Self, String> {
        let url = forks
            // Owner batches are signed against the LIVE chain, so simulating
            // them on a dev target would be the worst kind of fiction.
            .fresh_endpoint(chain, cfg, ExecTarget::Mainnet)
            .await
            .map_err(|e| format!("fork endpoint: {e}"))?;
        Self::attach(url, safe).await
    }

    /// [`SafeForkSession::acquire`] on an endpoint the caller already
    /// resolved (a consumer that resolves the fork endpoint through its own
    /// wrapper, e.g. to render the endpoint error in its own words): snapshot
    /// it, impersonate + fund the Safe. `Err(String)` = infra.
    ///
    /// **`url` must be a local fork.** Nothing here checks what `url` points
    /// at: a live chain is protected ONLY because `anvil_impersonateAccount`
    /// (called before anything is sent) errors on a non-anvil node, so
    /// `attach` fails there before any `eth_sendTransaction`. Resolve `url`
    /// through [`ForkManager::fresh_endpoint`] (as [`SafeForkSession::acquire`]
    /// does) with [`ExecTarget::Mainnet`].
    pub async fn attach(url: String, safe: Address) -> Result<Self, String> {
        let parsed = url.parse().map_err(|e| format!("fork url: {e}"))?;
        let provider = ProviderBuilder::new().connect_http(parsed).erased();
        let snap_id: String = provider
            .raw_request("evm_snapshot".into(), ())
            .await
            .map_err(|e| crate::util::redact::text(&format!("evm_snapshot: {e}")))?;
        provider
            .raw_request::<_, serde_json::Value>("anvil_impersonateAccount".into(), (safe,))
            .await
            .map_err(|e| crate::util::redact::text(&format!("anvil_impersonateAccount: {e}")))?;
        let _ = provider
            .raw_request::<_, serde_json::Value>(
                "anvil_setBalance".into(),
                (safe, U256::from(10u64).pow(U256::from(19u64))),
            )
            .await;
        Ok(Self {
            url,
            provider,
            safe,
            snap_id,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Nested checkpoint for independent what-if runs (fix simulation
    /// reverts between fixes so they don't contaminate each other).
    pub async fn checkpoint(&self) -> Result<String, String> {
        self.provider
            .raw_request("evm_snapshot".into(), ())
            .await
            .map_err(|e| crate::util::redact::text(&format!("evm_snapshot: {e}")))
    }

    pub async fn revert_to(&self, checkpoint: String) {
        let _ = self
            .provider
            .raw_request::<_, serde_json::Value>("evm_revert".into(), (checkpoint,))
            .await;
    }

    /// Execute one batch's transactions in order as the impersonated Safe.
    /// `offset` numbers the calls for cross-batch reporting.
    pub async fn apply(
        &self,
        batch: &TxBuilderBatch,
        offset: usize,
    ) -> Result<usize, ApplyFailure> {
        for (i, tx) in batch.transactions().iter().enumerate() {
            self.exec_one(&tx.to, &tx.value, &tx.data, offset + i + 1)
                .await?;
        }
        Ok(batch.len())
    }

    /// Execute a SINGLE owner call (fix simulation): same rule, same
    /// machinery — a one-call artifact is proven exactly like a batch.
    pub async fn apply_single(&self, to: &str, data_hex: &str) -> Result<(), ApplyFailure> {
        self.exec_one(to, "0", data_hex, 1).await
    }

    async fn exec_one(
        &self,
        to_str: &str,
        value_dec: &str,
        data_hex: &str,
        call_no: usize,
    ) -> Result<(), ApplyFailure> {
        let to: Address = to_str
            .parse()
            .map_err(|e| ApplyFailure::Infra(format!("bad `to` in batch: {e}")))?;
        let data = alloy::hex::decode(data_hex.trim_start_matches("0x"))
            .map_err(|e| ApplyFailure::Infra(format!("bad calldata in batch: {e}")))?;
        let value: alloy::primitives::U256 = value_dec
            .parse()
            .map_err(|e| ApplyFailure::Infra(format!("bad value in batch: {e}")))?;
        let req = TransactionRequest::default()
            .with_from(self.safe)
            .with_kind(TxKind::Call(to))
            .with_value(value)
            .with_input(data);
        let tx_hash: B256 = self
            .provider
            .raw_request("eth_sendTransaction".into(), (req.clone(),))
            .await
            .map_err(|e| {
                ApplyFailure::Infra(crate::util::redact::text(&format!(
                    "eth_sendTransaction: {e}"
                )))
            })?;
        let mut receipt = None;
        for _ in 0..50 {
            if let Ok(Some(r)) = self.provider.get_transaction_receipt(tx_hash).await {
                receipt = Some(r);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let Some(receipt) = receipt else {
            return Err(ApplyFailure::Infra(format!(
                "receipt for call #{call_no} never arrived"
            )));
        };
        if !receipt.status() {
            // Replay via eth_call to harvest the revert reason (a mined
            // revert carries no return data).
            let decoded = match self.provider.call(req).await {
                Err(e) => {
                    let payload = e
                        .as_error_resp()
                        .and_then(|r| r.as_revert_data())
                        .map(|b| taxonomy::decode_revert(&b))
                        .unwrap_or_else(|| {
                            taxonomy::DecodedError::infrastructure(
                                "unknown_revert",
                                crate::util::redact::text(&format!(
                                    "reverted; reason not recoverable: {e}"
                                )),
                            )
                        });
                    format!("[{}] {}", payload.code, payload.explanation)
                }
                Ok(_) => {
                    "reverted when mined but succeeds as eth_call (state-order dependent?)".into()
                }
            };
            return Err(ApplyFailure::Reverted {
                call_index: call_no,
                call: format!("to={to_str} data={}…", &data_hex[..data_hex.len().min(18)]),
                decoded,
            });
        }
        Ok(())
    }

    /// Best-effort teardown: revert the outer snapshot + stop impersonating.
    pub async fn teardown(self) {
        let _ = self
            .provider
            .raw_request::<_, serde_json::Value>("evm_revert".into(), (self.snap_id.clone(),))
            .await;
        let _ = self
            .provider
            .raw_request::<_, serde_json::Value>(
                "anvil_stopImpersonatingAccount".into(),
                (self.safe,),
            )
            .await;
    }
}
