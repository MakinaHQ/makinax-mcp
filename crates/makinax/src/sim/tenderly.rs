//! Minimal Tenderly REST client for ephemeral simulations.
//!
//! Persistent virtual testnet writes don't go through this module — they go via a
//! wallet-attached `alloy` provider pointed at the testnet's RPC URL (configured under
//! `[dev_rpc_urls.<chain>]`). This module is the fallback when no testnet RPC is set.

use alloy::primitives::{Address, B256, Bytes, U256};
use serde::{Deserialize, Serialize};

use crate::{
    config::TenderlyConfig,
    error::{Error, Result},
};

const SIMULATE_ENDPOINT: &str = "https://api.tenderly.co/api/v1/account";

#[derive(Clone)]
pub struct TenderlyClient {
    cfg: TenderlyConfig,
    api_key: String,
    http: reqwest::Client,
}

impl TenderlyClient {
    pub fn new(cfg: TenderlyConfig) -> Result<Self> {
        // This field takes an env var NAME, not a key. A literal key here
        // makes Tenderly silently non-functional (env::var never resolves) AND
        // parks the secret in config.toml — the live config did exactly this,
        // and the old message ("is not set") sent the operator looking for a
        // missing env var instead. Say which of the two we got.
        if looks_like_a_key(&cfg.api_key_env) {
            return Err(Error::Config(format!(
                "tenderly.api_key_env expects the NAME of an environment variable (e.g. \
                 \"TENDERLY_ACCESS_KEY\"), but the configured value looks like a KEY itself \
                 ({} chars, no lowercase-underscore name shape). Move the secret into the \
                 environment and put the variable's name here — a key in config.toml is a \
                 secret at rest, and Tenderly is non-functional until this is fixed.",
                cfg.api_key_env.len()
            )));
        }
        let api_key = std::env::var(&cfg.api_key_env).map_err(|_| {
            Error::Config(format!(
                "tenderly.api_key_env=`{}` names an environment variable that is not set",
                cfg.api_key_env
            ))
        })?;
        Ok(Self {
            cfg,
            api_key,
            // 30s request deadline — the simulate call must fail in bounded
            // time, not hang the pipeline on a stalled endpoint.
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        })
    }

    pub fn account(&self) -> &str {
        &self.cfg.account
    }

    pub fn project(&self) -> &str {
        &self.cfg.project
    }

    pub fn dashboard_url(&self, sim_id: &str) -> String {
        format!(
            "https://dashboard.tenderly.co/{}/{}/simulator/{}",
            self.cfg.account, self.cfg.project, sim_id
        )
    }

    /// Run an ephemeral simulation. Returns parsed asset changes, gas used, and a
    /// dashboard URL. Does not persist any state.
    pub async fn simulate(
        &self,
        chain_id: u64,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
    ) -> Result<SimulationOutcome> {
        let url = format!(
            "{}/{}/project/{}/simulate",
            SIMULATE_ENDPOINT, self.cfg.account, self.cfg.project
        );

        let body = SimulateRequest {
            network_id: chain_id.to_string(),
            from: format!("0x{:x}", from),
            to: format!("0x{:x}", to),
            input: format!("0x{}", alloy::hex::encode(&data)),
            value: value.to_string(),
            gas: 30_000_000,
            gas_price: "0".to_string(),
            save: true,
            simulation_type: "full".to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .header("X-Access-Key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::rpc(format!("tenderly simulate POST: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(Error::rpc(format!(
                "tenderly simulate {}: {}",
                status, text
            )));
        }

        let parsed = parse_simulate_response(&text)?;
        Ok(SimulationOutcome {
            success: parsed.success,
            simulation_id: parsed.id.clone(),
            dashboard_url: self.dashboard_url(&parsed.id),
            gas_used: parsed.gas_used,
            asset_changes: parsed.asset_changes,
            error_message: parsed.error_message,
            tx_hash: None, // ephemeral; no broadcast
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SimulationOutcome {
    pub success: bool,
    pub simulation_id: String,
    pub dashboard_url: String,
    pub gas_used: u64,
    pub asset_changes: Vec<AssetChange>,
    pub error_message: Option<String>,
    /// Set when a real broadcast happened (virtual testnet path), `None` for ephemeral sim.
    pub tx_hash: Option<B256>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssetChange {
    pub token_symbol: Option<String>,
    pub token_address: Option<String>,
    pub change_type: Option<String>,
    pub amount: Option<String>,
    /// Raw token units (decimal string) — the flow-evidence quantity.
    pub raw_amount: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub decimals: Option<u8>,
}

/// A parsed simulate-API response body. `parse_simulate_response` is public
/// so recorded-fixture tests exercise the exact production parse.
#[derive(Debug)]
pub struct ParsedSimulation {
    pub success: bool,
    pub id: String,
    pub gas_used: u64,
    pub asset_changes: Vec<AssetChange>,
    pub error_message: Option<String>,
}

pub fn parse_simulate_response(text: &str) -> Result<ParsedSimulation> {
    let parsed: SimulateResponse = serde_json::from_str(text).map_err(|e| {
        Error::rpc(format!(
            "tenderly simulate response parse: {} (body: {})",
            e, text
        ))
    })?;
    let asset_changes = parsed
        .transaction
        .as_ref()
        .and_then(|t| t.transaction_info.as_ref())
        .and_then(|ti| ti.asset_changes.as_ref())
        .map(|v| {
            v.iter()
                .map(|a| AssetChange {
                    token_symbol: a.token_info.as_ref().and_then(|t| t.symbol.clone()),
                    token_address: a
                        .token_info
                        .as_ref()
                        .and_then(|t| t.contract_address.clone()),
                    change_type: a.change_type.clone(),
                    amount: a.amount.clone(),
                    raw_amount: a.raw_amount.clone(),
                    from: a.from.clone(),
                    to: a.to.clone(),
                    decimals: a.token_info.as_ref().and_then(|t| t.decimals),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedSimulation {
        success: parsed.simulation.status,
        id: parsed.simulation.id.clone(),
        gas_used: parsed.transaction.as_ref().map(|t| t.gas_used).unwrap_or(0),
        asset_changes,
        error_message: parsed.simulation.error_message,
    })
}

// ─── Wire types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct SimulateRequest {
    network_id: String,
    from: String,
    to: String,
    input: String,
    value: String,
    gas: u64,
    gas_price: String,
    save: bool,
    simulation_type: String,
}

#[derive(Debug, Deserialize)]
struct SimulateResponse {
    simulation: SimulationField,
    #[serde(default)]
    transaction: Option<TransactionField>,
}

#[derive(Debug, Deserialize)]
struct SimulationField {
    id: String,
    status: bool,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TransactionField {
    #[serde(default)]
    gas_used: u64,
    #[serde(default)]
    transaction_info: Option<TransactionInfoField>,
}

#[derive(Debug, Deserialize)]
struct TransactionInfoField {
    #[serde(default)]
    asset_changes: Option<Vec<AssetChangeField>>,
}

#[derive(Debug, Deserialize)]
struct AssetChangeField {
    #[serde(default)]
    token_info: Option<TokenInfoField>,
    #[serde(default, rename = "type")]
    change_type: Option<String>,
    #[serde(default)]
    amount: Option<String>,
    #[serde(default)]
    raw_amount: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenInfoField {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    decimals: Option<u8>,
    #[serde(default)]
    contract_address: Option<String>,
}

/// Does this look like a secret rather than an env-var name? Env var names are
/// short, uppercase/underscore, and never contain the mixed-case entropy of a
/// provider key. Conservative: a false negative just restores the old
/// behaviour (env lookup fails and says so), while a false positive would
/// refuse a valid config, so the bar is deliberately high.
fn looks_like_a_key(v: &str) -> bool {
    let v = v.trim();
    if v.len() < 20 {
        return false;
    }
    let name_shaped = v
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if name_shaped {
        return false;
    }
    // Mixed case or a key-ish separator, and long: not a variable name.
    v.chars().any(|c| c.is_ascii_lowercase()) && v.chars().any(|c| c.is_ascii_alphanumeric())
}
