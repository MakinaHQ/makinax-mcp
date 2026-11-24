//! Fetched data: swap quotes, bridge fee quotes and CCTP attestations.

use makinax::ext::{AcrossFeeRequest, QuoteRequest};
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    MakinaxService,
    service::{parse_address, parse_u256, respond, to_json},
};

/// The client name sent to quote and fee APIs.
const CLIENT_NAME: &str = "makinax-mcp";

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct SwapQuoteArgs {
    /// The module's name in the config (`[modules.<name>]`). The module is
    /// the swap's sender and receiver.
    pub module: String,
    pub input_token: String,
    pub output_token: String,
    /// Raw input-token units, as a decimal string.
    pub input_amount: String,
    /// Allowed slippage in basis points; sets the quote's minimum output.
    pub slippage_bps: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct BridgeQuoteArgs {
    /// The module's name in the config (`[modules.<name>]`). Its chain is
    /// the origin.
    pub module: String,
    /// The destination chain's name, e.g. "mainnet".
    pub dest_chain: String,
    pub input_token: String,
    /// The token received on the destination chain.
    pub output_token: String,
    /// Raw input-token units, as a decimal string.
    pub amount: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct AttestationArgs {
    /// The chain the burn was sent on, by name.
    pub source_chain: String,
    /// The burn transaction's hash.
    pub tx_hash: String,
    /// Circle's domain number for the source chain. Default:
    /// bridges.cctp_v2.domains.<source_chain> from the config.
    #[serde(default)]
    pub source_domain: Option<u32>,
}

#[tool_router(router = fetch_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "Ask the swap aggregators for a quote with the module as sender and receiver, and return the best one (with calldata) and every aggregator's answer. Nothing is built or sent.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_swap_quote(&self, Parameters(a): Parameters<SwapQuoteArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let adapters = makinax::ext::default_adapters(&self.cfg.fetch);
                if adapters.is_empty() {
                    return Err(
                        "no swap aggregator is enabled: fetch.kyber and fetch.odos are both false"
                            .to_string(),
                    );
                }
                let req = QuoteRequest {
                    chain: &m.chain,
                    input_token: parse_address("input_token", &a.input_token)?,
                    output_token: parse_address("output_token", &a.output_token)?,
                    input_amount: parse_u256("input_amount", &a.input_amount)?,
                    slippage_bps: a.slippage_bps,
                    module: m.module,
                };
                let (best, all) = makinax::ext::best_quote(&adapters, &req)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "best": to_json(&best)?, "quotes": to_json(&all)? }))
            }
            .await,
        )
    }

    #[tool(
        description = "Ask Across for the fee to bridge a token from the module's chain to another chain: relay fee, expected output, limits and the quote timestamp a deposit must carry. Nothing is built or sent.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_bridge_quote(&self, Parameters(a): Parameters<BridgeQuoteArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                if !makinax::ext::across_enabled(&self.cfg.fetch) {
                    return Err("the Across fee fetcher is off: fetch.across is false".to_string());
                }
                let origin = makinax::ext::chain_id_for(&m.chain).ok_or_else(|| {
                    format!(
                        "module '{}' is on chain '{}', which has no known chain id",
                        a.module, m.chain
                    )
                })?;
                let dest = makinax::ext::chain_id_for(&a.dest_chain).ok_or_else(|| {
                    format!("`dest_chain` '{}' is not a known chain", a.dest_chain)
                })?;
                let req = AcrossFeeRequest {
                    input_token: parse_address("input_token", &a.input_token)?,
                    output_token: parse_address("output_token", &a.output_token)?,
                    origin_chain_id: origin,
                    destination_chain_id: dest,
                    amount: parse_u256("amount", &a.amount)?,
                };
                let q = makinax::ext::fetch_suggested_fees(
                    makinax::ext::ACROSS_API_URL,
                    &req,
                    CLIENT_NAME,
                )
                .await
                .map_err(|e| e.to_string())?;
                Ok(json!({
                    "bridge": "across",
                    "origin_chain_id": origin,
                    "destination_chain_id": dest,
                    "total_relay_fee": q.total_relay_fee.to_string(),
                    "output_amount": q.output_amount.to_string(),
                    "is_amount_too_low": q.is_amount_too_low,
                    "quote_timestamp": q.quote_timestamp,
                    "spoke_pool": format!("{:?}", q.spoke_pool),
                    "exclusive_relayer": format!("{:?}", q.exclusive_relayer),
                    "exclusivity_deadline": q.exclusivity_deadline,
                    "fill_deadline": q.fill_deadline,
                    "min_deposit": q.min_deposit.to_string(),
                    "max_deposit": q.max_deposit.to_string(),
                    "estimated_fill_time_s": q.estimated_fill_time_s,
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Ask Circle whether the CCTP burn in tx_hash, sent on source_chain, is attested: not_observed, pending, or complete with the message and attestation bytes a claim submits.",
        annotations(read_only_hint = true, open_world_hint = true)
    )]
    async fn get_cctp_attestation(
        &self,
        Parameters(a): Parameters<AttestationArgs>,
    ) -> CallToolResult {
        respond(
            async {
                self.available()?;
                if !makinax::ext::cctp_attestation_enabled(&self.cfg.fetch) {
                    return Err(
                        "the CCTP attestation fetcher is off: fetch.cctp_attestation is false"
                            .to_string(),
                    );
                }
                let domain = match a.source_domain {
                    Some(d) => d,
                    None => self
                        .cfg
                        .bridges
                        .cctp_v2
                        .as_ref()
                        .and_then(|c| c.domains.get(&a.source_chain).copied())
                        .ok_or_else(|| {
                            format!(
                                "bridges.cctp_v2.domains.{} is not configured; pass `source_domain`",
                                a.source_chain
                            )
                        })?,
                };
                let tx_hash: alloy::primitives::B256 = a
                    .tx_hash
                    .trim()
                    .parse()
                    .map_err(|e| format!("`tx_hash` is not a 32-byte hash ({}): {e}", a.tx_hash))?;
                let att =
                    makinax::ext::fetch_attestation(makinax::ext::IRIS_BASE_URL, domain, tx_hash)
                        .await;
                let mut out = json!({ "status": att.label(), "source_domain": domain });
                if let makinax::ext::Attestation::Complete {
                    message,
                    attestation,
                    forward_state,
                    nonce,
                } = &att
                {
                    out["message"] = json!(message.to_string());
                    out["attestation"] = json!(attestation.to_string());
                    out["forward_state"] = json!(forward_state);
                    out["nonce"] = json!(nonce);
                }
                Ok(out)
            }
            .await,
        )
    }
}
