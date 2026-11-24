//! Fetchers: off-chain data a caller may want before it builds a call —
//! swap quotes, bridge fee quotes, CCTP attestations, Merkl rewards.
//!
//! Every fetched value is ADVISORY. A quote prices a call and an attestation
//! supplies claim bytes, but what binds a call is its on-chain floor, and
//! what settles a transfer is a chain event. Before a fetched quote or a
//! bridge order is encoded, [`validate`] checks it against what was asked
//! for (target, recipient, token, amount, chain).
//!
//! Each vendor client is its own cargo feature (all on by default):
//! `fetch-kyber`, `fetch-odos`, `fetch-across`, `fetch-cctp`, `fetch-merkl`
//! (only the request; [`merkl`]'s parsing and encoding are always built). The swap-quote
//! trait and selection ([`quote`]) come with either swap fetcher, or alone
//! with the `quote` feature for a caller that brings its own adapters.

#[cfg(feature = "fetch-across")]
pub mod across;
#[cfg(feature = "fetch-cctp")]
pub mod cctp_attestation;
#[cfg(feature = "fetch-kyber")]
pub mod kyber;
pub mod merkl;
#[cfg(feature = "fetch-odos")]
pub mod odos;
#[cfg(feature = "quote")]
pub mod quote;
pub mod validate;

#[cfg(feature = "fetch-cctp")]
pub use cctp_attestation::{Attestation, fetch_attestation};
#[cfg(feature = "quote")]
pub use quote::{
    Quote, QuoteAdapter, QuoteRequest, QuoteSummary, best_quote, default_adapters, quote_all,
};

use crate::config::FetchConfig;

/// Per-request timeout of the shared fetch client.
#[cfg(any(
    feature = "fetch-kyber",
    feature = "fetch-odos",
    feature = "fetch-across",
    feature = "fetch-merkl"
))]
pub(crate) const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The headers the shared client sends on every request: `User-Agent` =
/// the client name (omitted if the name is not a valid header value).
#[cfg(any(
    feature = "fetch-kyber",
    feature = "fetch-odos",
    feature = "fetch-across",
    feature = "fetch-merkl"
))]
pub(crate) fn client_headers(client_name: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Ok(v) = reqwest::header::HeaderValue::from_str(client_name) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }
    headers
}

/// Shared HTTP client with sane defaults for vendor calls, one per client
/// name (sent as `User-Agent`). One process-wide pool per name — a new
/// Client per request leaves idle sockets in TIME_WAIT.
#[cfg(any(
    feature = "fetch-kyber",
    feature = "fetch-odos",
    feature = "fetch-across",
    feature = "fetch-merkl"
))]
pub(crate) fn http_client(user_agent: &str) -> reqwest::Client {
    use std::{collections::HashMap, sync::Mutex};
    static CLIENTS: std::sync::OnceLock<Mutex<HashMap<String, reqwest::Client>>> =
        std::sync::OnceLock::new();
    let mut clients = CLIENTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    clients
        .entry(user_agent.to_string())
        .or_insert_with(|| {
            reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .default_headers(client_headers(user_agent))
                .pool_max_idle_per_host(8)
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .tcp_keepalive(std::time::Duration::from_secs(30))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

/// Whether the Across fee fetcher may be used: compiled in (`fetch-across`)
/// and not switched off in config (`None` = on).
pub fn across_enabled(cfg: &FetchConfig) -> bool {
    cfg!(feature = "fetch-across") && cfg.across != Some(false)
}

/// Whether the CCTP attestation fetcher may be used: compiled in
/// (`fetch-cctp`) and not switched off in config (`None` = on).
pub fn cctp_attestation_enabled(cfg: &FetchConfig) -> bool {
    cfg!(feature = "fetch-cctp") && cfg.cctp_attestation != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fetcher_is_off_only_by_an_explicit_false_or_its_feature() {
        let on = FetchConfig::default();
        assert_eq!(across_enabled(&on), cfg!(feature = "fetch-across"));
        assert_eq!(cctp_attestation_enabled(&on), cfg!(feature = "fetch-cctp"));
        let off = FetchConfig {
            across: Some(false),
            cctp_attestation: Some(false),
            ..Default::default()
        };
        assert!(!across_enabled(&off));
        assert!(!cctp_attestation_enabled(&off));
    }
}
