//! Simulation: a call to the module, and an owner batch as the Safe.
//!
//! The read-write build simulates on a local fork (or eth_call, or
//! Tenderly when configured) through `makinax::ext::simulate`, and runs an
//! owner batch as the Safe on a fork. The read-only build has no fork
//! tooling: both tools use eth_call against the latest block.

use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use super::OwnerCallArgs;
use crate::{
    MakinaxService,
    service::{parse_address, respond},
};
use makinax::ext::{CallSpec, ModuleConfig};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SimCallArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// What to simulate; `kind` selects the call.
    pub call: CallSpec,
    /// For a CCTP bridge only: the route the order is built against.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    pub cctp_route: Option<serde_json::Value>,
    /// The sender. Default: the configured signer's address (read-write
    /// build), else modules.<name>.operator_address.
    #[serde(default)]
    pub from: Option<String>,
    /// "fork" (default in the read-write build), "eth_call" or "tenderly".
    /// The read-only build has eth_call only.
    #[serde(default)]
    pub backend: Option<String>,
    /// "mainnet" (default) or "dev": which endpoint the simulation stands
    /// for. "dev" uses dev_rpc_urls.<chain> when configured.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SimOwnerArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// The owner calls, in the order they run.
    pub calls: Vec<OwnerCallArgs>,
}

/// What the read-only build's simulation tools say, since they check with
/// eth_call rather than a fork. [`MakinaxService::router`] puts these in
/// place of the `#[tool]` descriptions below, which describe the
/// read-write build.
#[cfg(not(feature = "write"))]
pub(crate) const READ_ONLY_DESCRIPTIONS: [(&str, &str); 2] = [
    (
        "simulate_call",
        "Check a call to the module with eth_call at the latest block, without sending it: success or the decoded revert, gas, and the managed position's value change. No fork, no balance changes.",
    ),
    (
        "simulate_owner_batch",
        "Check a list of owner-restricted calls as the module's Safe with eth_call at the latest block, without sending anything: each call checked on its own, so a later call does not see an earlier call's effects. No fork, no balance changes.",
    ),
];

#[tool_router(router = sim_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "Simulate a call to the module without sending it: success or the decoded revert, gas, the managed position's value change, and the Safe's token balance changes where the backend reports them.",
        annotations(read_only_hint = true)
    )]
    async fn simulate_call(&self, Parameters(a): Parameters<SimCallArgs>) -> CallToolResult {
        respond(self.simulate_call_impl(a).await)
    }

    #[tool(
        description = "Run a list of owner-restricted calls as the module's Safe, in order, on a local fork, without sending anything to the chain: whether each succeeds, and the decoded revert of the first that fails.",
        annotations(read_only_hint = true)
    )]
    async fn simulate_owner_batch(
        &self,
        Parameters(a): Parameters<SimOwnerArgs>,
    ) -> CallToolResult {
        respond(self.simulate_owner_batch_impl(a).await)
    }
}

impl MakinaxService {
    /// The sender a simulation runs as.
    fn sender_for(
        &self,
        name: &str,
        m: &ModuleConfig,
        from: Option<&str>,
    ) -> Result<alloy::primitives::Address, String> {
        if let Some(f) = from {
            return parse_address("from", f);
        }
        #[cfg(feature = "write")]
        if let crate::service::SignerState::Ready { address, .. } = self.signer.as_ref() {
            return Ok(*address);
        }
        m.operator_address.ok_or_else(|| {
            format!(
                "no sender: pass `from`, configure a signer, or set modules.{name}.operator_address"
            )
        })
    }

    #[cfg(feature = "write")]
    async fn simulate_call_impl(&self, a: SimCallArgs) -> Result<serde_json::Value, String> {
        use makinax::ext::{ExecTarget, SimBackend, TenderlyClient};
        let m = self.module(&a.module)?;
        self.provider_for(&a.module, m)?;
        let backend = match a.backend.as_deref() {
            None | Some("fork") => SimBackend::Fork,
            Some("eth_call") => SimBackend::EthCall,
            Some("tenderly") => SimBackend::Tenderly,
            Some(other) => {
                return Err(format!(
                    "`backend` '{other}' is not one of fork, eth_call, tenderly"
                ));
            }
        };
        let target = match a.target.as_deref() {
            None | Some("mainnet") => ExecTarget::Mainnet,
            Some("dev") => ExecTarget::Dev,
            Some(other) => return Err(format!("`target` '{other}' is not one of mainnet, dev")),
        };
        let built = self
            .build_for(&a.module, m, &a.call, a.cctp_route.as_ref())
            .await?;
        let from = self.sender_for(&a.module, m, a.from.as_deref())?;
        // Tenderly is used when asked for, and as the fork's fallback when
        // configured and usable.
        let tenderly = match (&self.cfg.tenderly, backend) {
            (Some(t), SimBackend::Tenderly) => {
                Some(TenderlyClient::new(t.clone()).map_err(|e| e.to_string())?)
            }
            (Some(t), _) => TenderlyClient::new(t.clone()).ok(),
            (None, SimBackend::Tenderly) => {
                return Err("backend tenderly needs [tenderly] in the config".to_string());
            }
            (None, _) => None,
        };
        let result = makinax::ext::simulate(
            &a.module,
            m,
            &self.cfg,
            &built,
            from,
            backend,
            &self.providers,
            &self.forks,
            &self.receipts,
            tenderly.as_ref(),
            target,
        )
        .await
        .map_err(|e| e.to_string())?;
        crate::service::to_json(&result)
    }

    #[cfg(not(feature = "write"))]
    async fn simulate_call_impl(&self, a: SimCallArgs) -> Result<serde_json::Value, String> {
        let m = self.module(&a.module)?;
        let p = self.provider_for(&a.module, m)?;
        match a.backend.as_deref() {
            None | Some("eth_call") => {}
            Some(other) => {
                return Err(format!(
                    "`backend` '{other}' is not available in the read-only build; it simulates with eth_call only"
                ));
            }
        }
        match a.target.as_deref() {
            None | Some("mainnet") => {}
            Some(other) => {
                return Err(format!(
                    "`target` '{other}' is not available in the read-only build; it reads rpc_urls.{} only",
                    m.chain
                ));
            }
        }
        let built = self
            .build_for(&a.module, m, &a.call, a.cctp_route.as_ref())
            .await?;
        let from = self.sender_for(&a.module, m, a.from.as_deref())?;
        let block = latest_block(p).await?;
        let mut out = ro::eth_call(p, from, built.to, built.data.clone(), built.value, block).await;
        if built.is_manage_position
            && let Some(ret) = out.get("return_data").and_then(|v| v.as_str())
        {
            out["expected"] = ro::decode_manage_position(ret);
        }
        out["backend"] = serde_json::json!("eth_call");
        out["note"] = serde_json::json!(
            "eth_call checks success or revert and gas only; it reports no balance changes"
        );
        Ok(out)
    }

    #[cfg(feature = "write")]
    async fn simulate_owner_batch_impl(
        &self,
        a: SimOwnerArgs,
    ) -> Result<serde_json::Value, String> {
        use makinax::ext::{ApplyFailure, SafeForkSession};
        let m = self.module(&a.module)?;
        self.provider_for(&a.module, m)?;
        if a.calls.is_empty() {
            return Err("`calls` is empty".to_string());
        }
        let chain_id = self.chain_id_of(&a.module, m).await?;
        let (batch, summary) = super::owner_batch(m, chain_id, "simulation", "", &a.calls)?;
        let session = SafeForkSession::acquire(&self.cfg, &self.forks, &m.chain, m.safe)
            .await
            .map_err(|e| format!("could not simulate the batch: {e}"))?;
        let applied = session.apply(&batch, 0).await;
        session.teardown().await;
        match applied {
            Ok(n) => Ok(serde_json::json!({
                "backend": "fork",
                "success": true,
                "calls": n,
                "summary": summary,
            })),
            Err(ApplyFailure::Reverted {
                call_index,
                call,
                decoded,
            }) => Ok(serde_json::json!({
                "backend": "fork",
                "success": false,
                "reverted_call": call_index,
                "call": call,
                "revert": decoded,
                "summary": summary,
            })),
            Err(ApplyFailure::Infra(e)) => Err(format!("could not simulate the batch: {e}")),
        }
    }

    #[cfg(not(feature = "write"))]
    async fn simulate_owner_batch_impl(
        &self,
        a: SimOwnerArgs,
    ) -> Result<serde_json::Value, String> {
        let m = self.module(&a.module)?;
        let p = self.provider_for(&a.module, m)?;
        if a.calls.is_empty() {
            return Err("`calls` is empty".to_string());
        }
        let (batch, summary) = super::owner_batch(m, 0, "simulation", "", &a.calls)?;
        let block = latest_block(p).await?;
        let mut results = Vec::with_capacity(batch.len());
        for (i, tx) in batch.transactions().iter().enumerate() {
            let to = parse_address("to", &tx.to)?;
            let data: alloy::primitives::Bytes = tx
                .data
                .parse()
                .map_err(|e| format!("call {}: calldata: {e}", i + 1))?;
            let mut r =
                ro::eth_call(p, m.safe, to, data, alloy::primitives::U256::ZERO, block).await;
            r["call"] = serde_json::json!(summary.get(i));
            results.push(r);
        }
        let success = results
            .iter()
            .all(|r| r.get("success") == Some(&serde_json::Value::Bool(true)));
        Ok(serde_json::json!({
            "backend": "eth_call",
            "block": block,
            "success": success,
            "results": results,
            "note": "each call was checked on its own against the same block, as the Safe; a later call does not see an earlier call's effects",
        }))
    }
}

#[cfg(not(feature = "write"))]
async fn latest_block(p: &makinax::ext::RootProvider) -> Result<u64, String> {
    use alloy::providers::Provider as _;
    p.get_block_number()
        .await
        .map_err(|e| format!("latest block: {e}"))
}

/// eth_call checks for the read-only build.
#[cfg(not(feature = "write"))]
mod ro {
    use alloy::{
        network::TransactionBuilder,
        primitives::{Address, Bytes, U256},
        providers::Provider,
        rpc::types::TransactionRequest,
        sol_types::SolCall,
    };
    use makinax::ext::{RootProvider, bindings::IMakinaXModule, decode_revert};
    use serde_json::{Value, json};

    /// Call `to` from `from` at `block`: success with the return data and a
    /// gas estimate, or the decoded revert. A failure with no revert data
    /// is an RPC failure and says so.
    pub(super) async fn eth_call(
        p: &RootProvider,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
        block: u64,
    ) -> Value {
        let tx = TransactionRequest::default()
            .with_from(from)
            .with_to(to)
            .with_input(data.clone())
            .with_value(value);
        let echo = json!({
            "from": format!("{from:?}"),
            "to": format!("{to:?}"),
            "data": data.to_string(),
            "value": value.to_string(),
        });
        match p.call(tx.clone()).block(block.into()).await {
            Ok(ret) => {
                let gas = p.estimate_gas(tx).block(block.into()).await.ok();
                json!({
                    "success": true,
                    "block": block,
                    "gas_estimate": gas,
                    "return_data": ret.to_string(),
                    "call": echo,
                })
            }
            Err(e) => match e.as_error_resp().and_then(|r| r.as_revert_data()) {
                Some(revert) => json!({
                    "success": false,
                    "block": block,
                    "error": decode_revert(&revert),
                    "call": echo,
                }),
                None => json!({
                    "success": false,
                    "block": block,
                    "rpc_error": crate::service::error_text(e),
                    "call": echo,
                }),
            },
        }
    }

    /// The position value and change a managePosition call returns.
    pub(super) fn decode_manage_position(ret_hex: &str) -> Value {
        let Ok(bytes) = ret_hex.parse::<Bytes>() else {
            return Value::Null;
        };
        match IMakinaXModule::managePositionCall::abi_decode_returns(&bytes) {
            Ok(r) => json!({ "value": r.value.to_string(), "change": r.change.to_string() }),
            Err(_) => Value::Null,
        }
    }
}
