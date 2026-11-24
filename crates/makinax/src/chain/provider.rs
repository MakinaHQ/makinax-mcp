use std::{collections::HashMap, sync::Arc};

use alloy::{
    network::Ethereum,
    providers::{DynProvider, Provider, ProviderBuilder},
};

use crate::{
    config::MakinaxConfig,
    error::{Error, Result},
};

pub type RootProvider = DynProvider<Ethereum>;

/// Routing mode for chain providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    /// Production RPC URLs (`[rpc_urls.*]`).
    Mainnet,
    /// Dev-mode RPC URLs (`[dev_rpc_urls.*]`), typically Tenderly virtual testnets.
    Dev,
}

#[derive(Clone)]
struct ChainEndpoint {
    provider: RootProvider,
    /// Endpoints in preference order (primary first). For a single-endpoint
    /// chain this is one element; for a failover list, all of them.
    urls: Vec<String>,
}

impl ChainEndpoint {
    fn primary(&self) -> &str {
        self.urls.first().map(|s| s.as_str()).unwrap_or("")
    }
}

/// One Provider per (chain, mode). Cheap-clonable.
#[derive(Clone)]
pub struct Providers {
    mainnet: Arc<HashMap<String, ChainEndpoint>>,
    dev: Arc<HashMap<String, ChainEndpoint>>,
}

impl Providers {
    /// Build from an explicit (mainnet, dev) pair of endpoint-list maps.
    /// Both `from_config` and a consumer's own richer constructor — one that
    /// also knows a product-specific "dev RPC" config section — build from
    /// this, so there is one place that builds a provider map rather than
    /// two that could drift.
    pub async fn from_endpoint_maps(
        mainnet: impl Iterator<Item = (String, Vec<String>)>,
        dev: impl Iterator<Item = (String, Vec<String>)>,
    ) -> Result<Self> {
        Ok(Self {
            mainnet: Arc::new(build_map(mainnet, "rpc_urls")?),
            dev: Arc::new(build_map(dev, "dev_rpc_urls")?),
        })
    }

    /// Providers for every `[rpc_urls.*]` entry in a consumer's config file.
    /// This crate is stateless and knows nothing about a "dev mode" RPC
    /// section (that is a consumer concept, e.g. redirecting writes to a
    /// Tenderly virtual testnet) — a consumer that needs one builds its own
    /// constructor from [`Self::from_endpoint_maps`].
    pub async fn from_config(cfg: &MakinaxConfig) -> Result<Self> {
        Self::from_endpoint_maps(
            cfg.rpc_urls.iter().map(|(c, e)| (c.clone(), e.list())),
            std::iter::empty(),
        )
        .await
    }

    /// The configured endpoints for a chain (primary first) — for health_check
    /// observability. `None` if the chain has no `[rpc_urls]` entry.
    pub fn endpoints(&self, chain: &str) -> Option<&[String]> {
        self.mainnet.get(chain).map(|e| e.urls.as_slice())
    }

    /// Mainnet provider for the chain. Reads always use this.
    pub fn get(&self, chain: &str) -> Result<&RootProvider> {
        self.mainnet
            .get(chain)
            .map(|e| &e.provider)
            .ok_or_else(|| Error::Rpc(format!("no rpc_urls.{} configured", chain)))
    }

    /// Raw RPC URL string for a given (chain, mode). Used by writes that need to
    /// build a wallet-attached signing provider.
    pub fn url_for_mode(&self, chain: &str, mode: ProviderMode) -> Result<&str> {
        let map = match mode {
            ProviderMode::Mainnet => &self.mainnet,
            ProviderMode::Dev => &self.dev,
        };
        map.get(chain)
            .map(|e| e.primary())
            .ok_or_else(|| Error::Rpc(format!("no URL for chain `{}` in mode {:?}", chain, mode)))
    }

    pub fn has_dev_rpc(&self, chain: &str) -> bool {
        self.dev.contains_key(chain)
    }
}

/// Receipt-poll cadence for a chain, in milliseconds.
///
/// **alloy defaults every non-local transport to 7000ms** (`RpcClient::new(_,
/// is_local=false)` → `AtomicU64::new(7000)`), and we never overrode it. On
/// Base, where blocks are ~2s, that means a receipt is available for up to 7
/// seconds before `get_receipt()` notices: pure dead time inside a send, which
/// a latency profile measured at 11.3s with no explanation for most of it.
///
/// Half the block time, floored at 500ms: fast enough to catch the receipt in
/// the block after inclusion, slow enough not to hammer a keyed endpoint. The
/// wait is bounded and short, so the extra polls are a handful of calls, not a
/// sustained load.
pub(crate) fn receipt_poll_interval_ms(chain: &str) -> u64 {
    let block_ms: u64 = match chain {
        // ~2s
        "base" | "optimism" | "arbitrum" => 2_000,
        // ~12s
        "mainnet" | "ethereum" => 12_000,
        // Unknown chain: keep alloy's behaviour rather than guessing fast.
        _ => 14_000,
    };
    (block_ms / 2).max(500)
}

/// Build a (possibly failover-backed) provider from an ordered endpoint list.
/// Public so any binary gets the same transport-level failover as the rest of
/// this crate: infra errors only, never a contract revert.
pub fn provider_from_endpoints(urls: &[String], label: &str) -> Result<RootProvider> {
    build_provider(urls, label, "*")
}

/// Every chain-RPC request gets a hard 30s deadline. `reqwest::Client::new()`
/// carries NO request timeout, so a stalled-but-connected endpoint parked its
/// caller forever, and a caller that reads serially then stalls every later
/// read behind the hung one, with no self-recovery. 30s is generous for any JSON-RPC call
/// this crate makes (receipt WAITS poll in bounded loops; this bounds each
/// request, not the wait). Every outbound HTTP client a caller builds should
/// carry a timeout — chain RPC was the one transport that got missed here.
pub fn http_client() -> Result<alloy::transports::http::reqwest::Client> {
    static CLIENT: std::sync::OnceLock<alloy::transports::http::reqwest::Client> =
        std::sync::OnceLock::new();
    Ok(CLIENT
        .get_or_init(|| {
            alloy::transports::http::reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default()
        })
        .clone())
}

fn build_map(
    entries: impl Iterator<Item = (String, Vec<String>)>,
    label: &str,
) -> Result<HashMap<String, ChainEndpoint>> {
    let mut map = HashMap::new();
    for (chain, urls) in entries {
        let provider = build_provider(&urls, label, &chain)?;
        map.insert(chain, ChainEndpoint { provider, urls });
    }
    Ok(map)
}

/// Build one provider for a chain from its ordered endpoint list. A single
/// endpoint uses a plain HTTP transport; two or more are wrapped in alloy's
/// native `FallbackService`, which hedges each read across the endpoints and
/// takes the first answer. A contract revert comes back as a valid JSON-RPC
/// response every endpoint agrees on, so it never triggers failover (same
/// discipline as `is_fork_infra_failure`): the failure is surfaced, not masked.
///
/// Two answers a public fallback gives faster than the primary are not
/// answers (see `hedge.rs`): a JSON-RPC error saying the endpoint cannot
/// serve the request (archive, pruned, its own rate limit) becomes a transport
/// failure so the hedge moves on, and a receipt read prefers any endpoint's
/// receipt over another's `null`. A live incident lost a payment transaction
/// to exactly those two: the fallback's archive rejection and its `null` won
/// the race against the primary's mined receipts.
fn build_provider(urls: &[String], label: &str, chain: &str) -> Result<RootProvider> {
    use alloy::transports::http::{ReqwestTransport, reqwest::Url};
    let mut parsed: Vec<Url> = Vec::with_capacity(urls.len());
    for u in urls {
        parsed.push(
            u.parse::<Url>()
                .map_err(|e| Error::Rpc(format!("invalid {}.{}: {}", label, chain, e)))?,
        );
    }
    match parsed.len() {
        0 => Err(Error::Rpc(format!("{}.{} has no endpoints", label, chain))),
        1 => {
            let transport =
                ReqwestTransport::with_client(http_client()?, parsed.into_iter().next().unwrap());
            let client = alloy::rpc::client::RpcClient::new(transport, false).with_poll_interval(
                std::time::Duration::from_millis(receipt_poll_interval_ms(chain)),
            );
            Ok(ProviderBuilder::new().connect_client(client).erased())
        }
        n => {
            let http = http_client()?;
            let transports: Vec<super::hedge::Answerable<ReqwestTransport>> = parsed
                .into_iter()
                .map(|u| {
                    let label = u.host_str().unwrap_or("endpoint").to_string();
                    super::hedge::Answerable::new(
                        ReqwestTransport::with_client(http.clone(), u),
                        label,
                    )
                })
                .collect();
            // active_transport_count = n (all endpoints): each read is hedged
            // across every endpoint and the FIRST SUCCESS wins, so a dead/flaky
            // endpoint never blocks a read (seamless per-request failover). A
            // contract revert comes back as the SAME valid response from every
            // endpoint, so it is surfaced, never masked as a transport failure.
            // Reads are idempotent; broadcasts use the primary endpoint only.
            // Receipt-class reads go through the hedge that prefers presence.
            let fallback = alloy::transports::layers::FallbackService::new(transports.clone(), n);
            let service = super::hedge::ReceiptHedge::new(transports, fallback);
            let client = alloy::rpc::client::RpcClient::new(service, false).with_poll_interval(
                std::time::Duration::from_millis(receipt_poll_interval_ms(chain)),
            );
            Ok(ProviderBuilder::new().connect_client(client).erased())
        }
    }
}

#[cfg(test)]
mod poll_cadence_tests {
    use super::receipt_poll_interval_ms;

    /// alloy defaults EVERY non-local transport to 7000ms
    /// (`alloy-rpc-client/src/client.rs`: `if is_local { 250 } else { 7000 }`)
    /// and this crate never overrode it. On Base — ~2s blocks — that leaves a
    /// receipt sitting available for up to 7 seconds before `get_receipt()`
    /// notices. Pure dead time inside the 11.3s `execute_action` the first
    /// latency profile could not explain.
    #[test]
    fn the_cadence_tracks_the_chain_not_the_default() {
        const ALLOY_DEFAULT_MS: u64 = 7_000;

        // Fast L2s: sub-block, so the receipt is caught in the block after
        // inclusion rather than up to 7s later.
        for chain in ["base", "optimism", "arbitrum"] {
            let ms = receipt_poll_interval_ms(chain);
            assert_eq!(ms, 1_000, "{chain}");
            assert!(ms < ALLOY_DEFAULT_MS, "{chain} must beat the default");
        }

        // Mainnet's 12s blocks do not need 1s polling; half a block is plenty
        // and costs ~6 calls per block instead of 12.
        assert_eq!(receipt_poll_interval_ms("mainnet"), 6_000);

        // An UNKNOWN chain keeps the conservative cadence rather than guessing
        // fast — a wrong fast guess hammers someone's keyed endpoint.
        assert!(
            receipt_poll_interval_ms("some-new-rollup") >= ALLOY_DEFAULT_MS,
            "an unknown chain must not be polled faster than the default on a guess"
        );

        // Never hot-loop, whatever the block time.
        assert!(receipt_poll_interval_ms("base") >= 500);
    }
}

#[cfg(test)]
mod from_config_tests {
    use super::*;
    use crate::config::{Endpoints, MakinaxConfig};

    #[tokio::test]
    async fn from_config_builds_a_provider_for_every_configured_chain() {
        let mut cfg = MakinaxConfig::default();
        cfg.rpc_urls
            .insert("base".into(), Endpoints::One("http://127.0.0.1:1".into()));
        let providers = Providers::from_config(&cfg).await.unwrap();
        assert!(providers.get("base").is_ok());
        assert!(providers.get("mainnet").is_err());
        // This crate is stateless and has no "dev RPC" config section of its
        // own — from_config never populates one.
        assert!(!providers.has_dev_rpc("base"));
    }

    #[tokio::test]
    async fn from_config_refuses_an_unparseable_endpoint() {
        let mut cfg = MakinaxConfig::default();
        cfg.rpc_urls
            .insert("base".into(), Endpoints::One("not a url".into()));
        assert!(Providers::from_config(&cfg).await.is_err());
    }
}
