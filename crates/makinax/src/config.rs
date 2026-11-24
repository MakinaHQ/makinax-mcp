//! Neutral config types for reading, building and sending calls to a
//! makinaX module: signer selection, RPC endpoints, fork-backend tuning,
//! bridge lane addressing and rootfile sourcing.
//!
//! This module holds no state and no policy: it only knows about configured
//! Safe/module pairs and the primitives around them. Anything a caller layers
//! on top belongs in the caller's own config.

use std::{collections::BTreeMap, path::PathBuf};

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

/// One-or-many RPC endpoints for a chain. Deserializes from EITHER a bare
/// string (single endpoint, back-compat) OR an array of strings (ordered
/// failover). The provider layer builds a fallback-backed transport from
/// `list()`; failover happens ONLY on infrastructure errors, never on a decoded
/// contract revert (which is a valid JSON-RPC response, so the transport never
/// rotates on it).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Endpoints {
    One(String),
    Many(Vec<String>),
}

impl Endpoints {
    /// The endpoints in preference order (primary first).
    pub fn list(&self) -> Vec<String> {
        match self {
            Endpoints::One(s) => vec![s.clone()],
            Endpoints::Many(v) => v.clone(),
        }
    }
    /// The primary (most-preferred) endpoint, if any. Used where a single URL
    /// is unavoidable (anvil fork source, wallet-attached signing provider).
    pub fn primary(&self) -> Option<&str> {
        match self {
            Endpoints::One(s) => Some(s.as_str()),
            Endpoints::Many(v) => v.first().map(|s| s.as_str()),
        }
    }
}

impl From<&str> for Endpoints {
    fn from(s: &str) -> Self {
        Endpoints::One(s.to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignerConfig {
    Gcp(GcpSignerConfig),
    Keystore(KeystoreSignerConfig),
    PrivateKey(PrivateKeySignerConfig),
    Unsigned,
}

impl SignerConfig {
    /// Serde default for `MakinaxConfig::signer`: no signer configured.
    pub fn unsigned() -> Self {
        SignerConfig::Unsigned
    }
}

impl Default for SignerConfig {
    fn default() -> Self {
        SignerConfig::unsigned()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KeystoreSignerConfig {
    /// Path to the encrypted JSON keystore file.
    pub path: PathBuf,
    /// 0600 file holding the passphrase: the headless, cross-platform
    /// option for a signer with no interactive session.
    /// Read FIRST (before env, before keychain); refused if not 0600,
    /// because a loose passphrase file is a finding, not a convenience.
    #[serde(default)]
    pub passphrase_file: Option<PathBuf>,
    /// Env var holding the passphrase (checked second).
    #[serde(default)]
    pub passphrase_env: Option<String>,
    /// macOS Keychain service name holding the passphrase (checked third,
    /// via `security find-generic-password -s <service> -w`). NOT available
    /// over SSH / on a locked login keychain, and absent on Linux — an
    /// optional desktop hardening path, never the headless default.
    #[serde(default)]
    pub keychain_service: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GcpSignerConfig {
    pub project_id: String,
    #[serde(default = "GcpSignerConfig::default_location")]
    pub location: String,
    pub keyring: String,
    pub key_name: String,
    #[serde(default = "GcpSignerConfig::default_key_version")]
    pub key_version: u32,
}

impl GcpSignerConfig {
    fn default_location() -> String {
        "global".to_string()
    }
    fn default_key_version() -> u32 {
        1
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PrivateKeySignerConfig {
    /// Name of the env var containing the hex-encoded private key.
    pub env: String,
}

/// Anvil fork tuning for execute-time facts sims. Defaults tolerate
/// high-latency gateways (the stock alloy spawn waits only 10s and passes no
/// fork flags — that combination times out on real gateways).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ForkConfig {
    /// Per-chain upstream to fork FROM; falls back to [rpc_urls.<chain>].
    /// Use this to point forks at an archive/gateway endpoint without
    /// changing the main read path.
    #[serde(default)]
    pub source_rpc_urls: BTreeMap<String, String>,
    /// Spawn-ready wait AND anvil's per-request upstream timeout (ms).
    #[serde(default = "ForkConfig::default_timeout_ms")]
    pub timeout_ms: u64,
    /// anvil --retries: fork request retries on upstream errors.
    #[serde(default = "ForkConfig::default_retries")]
    pub retries: u32,
    /// anvil --fork-retry-backoff (ms).
    #[serde(default = "ForkConfig::default_retry_backoff_ms")]
    pub retry_backoff_ms: u64,
    /// anvil --compute-units-per-second: self-imposed rate limit against the
    /// fork source. Unset = anvil default.
    #[serde(default)]
    pub compute_units_per_second: Option<u64>,
    /// Explicit anvil binary. Unset: probe $PATH, then ~/.foundry/bin/anvil.
    #[serde(default)]
    pub anvil_path: Option<PathBuf>,
    /// Base-capable raw Anvil binary (base/base-anvil release). On Base,
    /// prefer this path, then the namespaced base-anvil wrapper on PATH.
    /// A raw binary receives --base; the official wrapper enables it itself.
    #[serde(default)]
    pub base_anvil_path: Option<PathBuf>,
    /// Health-check fork-readiness probe: "auto" (default; actively
    /// spawn-or-reset the warm fork per chain, result cached 300s) or "off"
    /// (constrained setups that would rather not pay a probe spawn).
    #[serde(default = "ForkConfig::default_probe")]
    pub probe: String,
    /// Automatically fall back to the next-best-available simulation backend
    /// when the fork backend hits an INFRASTRUCTURE failure (RPC error,
    /// "failed to get storage", spawn/timeout, 429) — Tenderly if [tenderly]
    /// is configured, else eth_call. The fork is the flakiest dependency; this
    /// keeps a naive strategy from silently dropping an action on a transient
    /// blip. A genuine on-chain revert is NEVER masked — only failures where
    /// the fork could not evaluate the tx fall back, and the reduced fidelity
    /// is flagged on the result. Set false for strict fork-only (an infra
    /// failure then surfaces as before).
    #[serde(default = "ForkConfig::default_sim_fallback")]
    pub sim_fallback: bool,
    /// EVM hardfork for spawned anvil forks (`anvil --hardfork`). Mainnet
    /// (2026) runs osaka; anvil's built-in "latest" default lags behind —
    /// without this, contracts using post-prague opcodes fail with
    /// NotActivated in fork sims.
    #[serde(default = "ForkConfig::default_hardfork")]
    pub hardfork: String,
}

impl ForkConfig {
    pub fn default_timeout_ms() -> u64 {
        120_000
    }
    pub fn default_retries() -> u32 {
        5
    }
    fn default_retry_backoff_ms() -> u64 {
        1_000
    }
    fn default_probe() -> String {
        "auto".into()
    }
    fn default_sim_fallback() -> bool {
        true
    }
    pub fn default_hardfork() -> String {
        "osaka".to_string()
    }
}

impl Default for ForkConfig {
    fn default() -> Self {
        Self {
            source_rpc_urls: BTreeMap::new(),
            timeout_ms: Self::default_timeout_ms(),
            retries: Self::default_retries(),
            retry_backoff_ms: Self::default_retry_backoff_ms(),
            compute_units_per_second: None,
            anvil_path: None,
            base_anvil_path: None,
            probe: Self::default_probe(),
            sim_fallback: Self::default_sim_fallback(),
            hardfork: Self::default_hardfork(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TenderlyConfig {
    pub account: String,
    pub project: String,
    pub api_key_env: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct BridgesConfig {
    pub cctp_v2: Option<CctpV2Config>,
    pub lz_v2: Option<LzV2Config>,
    pub across_v4: Option<AcrossV4Config>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CctpV2Config {
    pub domains: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LzV2Config {
    pub endpoints: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AcrossV4Config {
    pub spoke_pools: BTreeMap<String, Address>,
}

/// Source of a rootfile. Either a local filesystem path or a github reference.
/// Mirrors spellcaster's `local:` / `github:` prefix convention.
#[derive(Debug, Clone)]
pub enum RootfileSource {
    Local(PathBuf),
    Github(String),
}

impl<'de> Deserialize<'de> for RootfileSource {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(d)?;
        if let Some(rest) = raw.strip_prefix("local:") {
            Ok(RootfileSource::Local(PathBuf::from(rest)))
        } else if let Some(rest) = raw.strip_prefix("github:") {
            Ok(RootfileSource::Github(rest.to_string()))
        } else {
            Err(serde::de::Error::custom(
                "rootfile source must start with `local:` or `github:`",
            ))
        }
    }
}

impl std::fmt::Display for RootfileSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RootfileSource::Local(p) => write!(f, "local:{}", p.display()),
            RootfileSource::Github(g) => write!(f, "github:{}", g),
        }
    }
}

impl Serialize for RootfileSource {
    fn serialize<S>(&self, s: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RootfileSource::Local(p) => s.serialize_str(&format!("local:{}", p.display())),
            RootfileSource::Github(g) => s.serialize_str(&format!("github:{}", g)),
        }
    }
}

/// The module-generic slice of a configured Safe.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModuleConfig {
    pub chain: String,
    pub safe: Address,
    pub module: Address,
    #[serde(default)]
    pub rootfile: Option<RootfileSource>,
    #[serde(default)]
    pub operator_address: Option<Address>,
    #[serde(default)]
    pub guardian_address: Option<Address>,
    #[serde(default)]
    pub swappers: BTreeMap<String, u16>,
}

/// A consumer's whole config file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MakinaxConfig {
    #[serde(default)]
    pub modules: BTreeMap<String, ModuleConfig>,
    #[serde(default)]
    pub rpc_urls: BTreeMap<String, Endpoints>,
    /// Alternate endpoint per chain for DEV-targeted work: when present for
    /// a chain, the fork layer attaches to it (never spawned, never reset —
    /// externally managed, e.g. a persistent virtual testnet or a forked RPC)
    /// instead of spawning its own anvil fork, and refuses to answer a
    /// mainnet-targeted simulation from it.
    #[serde(default)]
    pub dev_rpc_urls: BTreeMap<String, String>,
    #[serde(default = "SignerConfig::unsigned")]
    pub signer: SignerConfig,
    pub tenderly: Option<TenderlyConfig>,
    #[serde(default)]
    pub fork: ForkConfig,
    #[serde(default)]
    pub bridges: BridgesConfig,
    #[serde(default)]
    pub events: EventsConfig,
    #[serde(default)]
    pub fetch: FetchConfig,
}

fn d_chunk() -> u64 {
    2000
}

fn d_calls() -> u32 {
    50
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventsConfig {
    #[serde(default = "d_chunk")]
    pub chunk_blocks: u64,
    #[serde(default = "d_calls")]
    pub max_calls: u32,
}

impl Default for EventsConfig {
    fn default() -> Self {
        Self {
            chunk_blocks: d_chunk(),
            max_calls: d_calls(),
        }
    }
}

/// Per-fetcher on/off switches. `None` means on (the default): a fetcher is
/// only ever disabled by an explicit `false`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FetchConfig {
    pub kyber: Option<bool>,
    pub odos: Option<bool>,
    pub across: Option<bool>,
    pub cctp_attestation: Option<bool>,
}
