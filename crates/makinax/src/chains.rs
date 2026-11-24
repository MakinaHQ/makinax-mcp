//! Verified network profiles: the generic identity data any caller needs to
//! resolve a chain key or id — an RPC, a Safe tx-service slug, a Safe web-app
//! prefix, a block explorer host. Policy (which of these chains a caller
//! actually supports, and what it adds to each) is NOT here: a caller
//! composes its own richer type on top of [`ChainProfile`] rather than this
//! crate guessing at it.
//!
//! [`KNOWN`] is every chain identity this crate has verified data for — a
//! recognition table, not a support table. A consumer that also has its own
//! narrower "chains I deploy to" concept filters this list rather than
//! re-authoring a second copy of the rows, so the two can never disagree.

/// A verified chain identity with the small set of external facts a caller
/// needs to route to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainProfile {
    /// Config key (`"mainnet"` not `"ethereum"`).
    pub key: &'static str,
    pub chain_id: u64,
    pub native_symbol: &'static str,
    /// Extra config spellings that canonicalize to [`Self::key`].
    pub aliases: &'static [&'static str],
    /// Measured publicnode URL, or `None` if unmeasured.
    pub public_rpc: Option<&'static str>,
    /// Safe tx-service slug on `api.safe.global`, or `None` if unverified.
    pub tx_service_slug: Option<&'static str>,
    /// Safe web-app routing prefix, or `None` if unverified.
    pub safe_app_prefix: Option<&'static str>,
}

/// Every chain identity this crate has verified data for. Recognition only —
/// a consumer's own "which of these do I deploy to" is layered on top, not
/// encoded here.
pub const KNOWN: &[ChainProfile] = &[
    ChainProfile {
        key: "mainnet",
        chain_id: 1,
        native_symbol: "ETH",
        aliases: &["ethereum"],
        public_rpc: Some("https://ethereum-rpc.publicnode.com"),
        tx_service_slug: Some("eth"),
        safe_app_prefix: Some("eth"),
    },
    ChainProfile {
        key: "base",
        chain_id: 8453,
        native_symbol: "ETH",
        aliases: &[],
        public_rpc: Some("https://base-rpc.publicnode.com"),
        tx_service_slug: Some("base"),
        safe_app_prefix: Some("base"),
    },
    ChainProfile {
        key: "optimism",
        chain_id: 10,
        native_symbol: "ETH",
        aliases: &["op"],
        public_rpc: Some("https://mainnet.optimism.io"),
        tx_service_slug: None,
        safe_app_prefix: None,
    },
    ChainProfile {
        key: "hyperevm",
        chain_id: 999,
        native_symbol: "HYPE",
        aliases: &["hyperliquid", "hypeevm"],
        public_rpc: Some("https://rpc.hyperliquid.xyz/evm"),
        // Measured: `/tx-service/hyper/api/v1/about/` answers with
        // the same shape as the known-good `base` slug (anonymous
        // monthly-quota 429 rather than a 404, which is what a slug that
        // does not exist returns).
        tx_service_slug: Some("hyper"),
        safe_app_prefix: Some("hyper"),
    },
    ChainProfile {
        key: "robinhood",
        chain_id: 4663,
        native_symbol: "ETH",
        aliases: &["robinhood-chain"],
        public_rpc: Some("https://rpc.mainnet.chain.robinhood.com"),
        tx_service_slug: None,
        safe_app_prefix: None,
    },
];

/// Canonical config key (`ethereum` -> `mainnet`), lowercased. An unknown
/// name is lowercased only; callers that need an allowlist refusal build it
/// from [`supported_keys`].
pub fn canonicalize(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    KNOWN
        .iter()
        .find(|c| c.key == lowered || c.aliases.iter().any(|a| *a == lowered))
        .map(|c| c.key.to_string())
        .unwrap_or(lowered)
}

/// The row for a config key or alias, if this crate knows it.
pub fn by_key(raw: &str) -> Option<&'static ChainProfile> {
    let key = canonicalize(raw);
    KNOWN.iter().find(|c| c.key == key)
}

/// The row for an EIP-155 id, if this crate knows it.
pub fn by_id(chain_id: u64) -> Option<&'static ChainProfile> {
    KNOWN.iter().find(|c| c.chain_id == chain_id)
}

/// `by_key` under another name: chain recognition, not deployment capability.
pub fn network_by_key(raw: &str) -> Option<&'static ChainProfile> {
    by_key(raw)
}

/// Config keys in table order.
pub fn supported_keys() -> Vec<&'static str> {
    KNOWN.iter().map(|c| c.key).collect()
}

/// Publicnode URL for a known chain, if measured.
pub fn public_rpc_for(chain: &str) -> Option<&'static str> {
    by_key(chain).and_then(|c| c.public_rpc)
}

/// Safe tx-service slug for a chain id, if verified.
pub fn tx_service_slug(chain_id: u64) -> Option<&'static str> {
    by_id(chain_id).and_then(|c| c.tx_service_slug)
}

/// Verified Safe queue route; Transaction Builder is reached through New Transaction.
pub fn safe_queue_url(chain: &str, safe: alloy::primitives::Address) -> Option<String> {
    if safe.is_zero() {
        return None;
    }
    let prefix = by_key(chain)?.safe_app_prefix?;
    Some(format!(
        "https://app.safe.global/transactions/queue?safe={prefix}:{}",
        safe.to_checksum(None)
    ))
}

/// Safe's supported app route into its published Transaction Builder tool.
pub fn safe_builder_url(chain: &str, safe: alloy::primitives::Address) -> Option<String> {
    if safe.is_zero() {
        return None;
    }
    let prefix = by_key(chain)?.safe_app_prefix?;
    Some(format!(
        "https://app.safe.global/apps/open?safe={prefix}:{}&appUrl=https%3A%2F%2Fapps-portal.safe.global%2Ftx-builder",
        safe.to_checksum(None)
    ))
}

/// A block explorer's contract page, for the chains this crate can name a
/// host for.
pub fn contract_url(chain: &str, address: alloy::primitives::Address) -> Option<String> {
    if address.is_zero() {
        return None;
    }
    let host = match canonicalize(chain).as_str() {
        "mainnet" => "etherscan.io",
        "base" => "basescan.org",
        "optimism" => "optimistic.etherscan.io",
        "hyperevm" => "hyperevmscan.io",
        "robinhood" => "robinhoodchain.blockscout.com",
        _ => return None,
    };
    Some(format!(
        "https://{host}/address/{}#code",
        address.to_checksum(None)
    ))
}

/// Chains the Tenderly simulate API and the Odos quote API serve, by config
/// key. This is a SERVICE table, not the chain table ([`KNOWN`] /
/// [`crate::chain::chain_id_for`]): HyperEVM is absent on purpose because
/// neither service is verified for it, and adding a chain here is a claim
/// about those services, not about which chains exist.
pub fn service_chain_id(chain: &str) -> Option<u64> {
    match chain {
        "mainnet" | "ethereum" => Some(1),
        "base" => Some(8453),
        "arbitrum" | "arbitrum-one" => Some(42161),
        "ink" => Some(57073),
        "optimism" => Some(10),
        "polygon" => Some(137),
        "sepolia" => Some(11155111),
        "base-sepolia" => Some(84532),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_maps_aliases_and_lowercases_unknowns() {
        assert_eq!(canonicalize("ethereum"), "mainnet");
        assert_eq!(canonicalize("BASE"), "base");
        assert_eq!(canonicalize("OPTimism"), "optimism");
        assert_eq!(canonicalize("op"), "optimism");
        assert_eq!(canonicalize("HypeEVM"), "hyperevm");
        assert_eq!(canonicalize("hyperliquid"), "hyperevm");
        assert_eq!(canonicalize("robinhood-chain"), "robinhood");
        assert_eq!(canonicalize("Arbitrum"), "arbitrum");
    }

    #[test]
    fn by_key_and_by_id_agree_on_the_known_table() {
        assert_eq!(by_key("ethereum").map(|c| c.chain_id), Some(1));
        assert_eq!(by_id(8453).map(|c| c.key), Some("base"));
        assert_eq!(by_key("op").map(|c| c.chain_id), Some(10));
        assert_eq!(by_id(999).map(|c| c.key), Some("hyperevm"));
        assert_eq!(by_key("robinhood-chain").map(|c| c.chain_id), Some(4663));
        assert!(by_key("arbitrum").is_none());
        assert!(by_id(42161).is_none());
    }

    #[test]
    fn supported_keys_is_table_order() {
        assert_eq!(
            supported_keys(),
            vec!["mainnet", "base", "optimism", "hyperevm", "robinhood"]
        );
    }

    #[test]
    fn public_rpc_is_measured_for_every_known_row() {
        for c in KNOWN {
            assert!(public_rpc_for(c.key).is_some(), "{}", c.key);
        }
    }

    #[test]
    fn tx_service_slug_is_verified_only_where_measured() {
        assert_eq!(tx_service_slug(1), Some("eth"));
        assert_eq!(tx_service_slug(8453), Some("base"));
        assert_eq!(tx_service_slug(999), Some("hyper"));
        assert_eq!(tx_service_slug(10), None, "optimism has no verified slug");
        assert_eq!(
            tx_service_slug(4663),
            None,
            "robinhood has no verified slug"
        );
        assert_eq!(tx_service_slug(137), None);
    }

    #[test]
    fn safe_urls_are_scoped_to_a_nonzero_safe_on_a_known_chain() {
        let safe: alloy::primitives::Address = "0x0000000000000000000000000000000000000033"
            .parse()
            .unwrap();
        let builder = safe_builder_url("base", safe).unwrap();
        assert!(builder.starts_with("https://app.safe.global/apps/open?safe=base:"));
        assert!(safe_builder_url("base", alloy::primitives::Address::ZERO).is_none());
        assert!(safe_builder_url("not-a-chain", safe).is_none());
        // HyperEVM's Safe app prefix is verified, unlike optimism/robinhood.
        assert!(
            safe_builder_url("hyperevm", safe)
                .unwrap()
                .contains("safe=hyper:")
        );
        assert!(safe_builder_url("optimism", safe).is_none());

        let queue = safe_queue_url("mainnet", safe).unwrap();
        assert!(queue.starts_with("https://app.safe.global/transactions/queue?safe=eth:"));
        assert!(safe_queue_url("mainnet", alloy::primitives::Address::ZERO).is_none());
    }

    #[test]
    fn contract_url_is_only_named_for_known_hosts() {
        let addr: alloy::primitives::Address = "0x0000000000000000000000000000000000000044"
            .parse()
            .unwrap();
        assert!(
            contract_url("mainnet", addr)
                .unwrap()
                .contains("etherscan.io")
        );
        assert!(contract_url("base", addr).unwrap().contains("basescan.org"));
        assert!(
            contract_url("optimism", addr)
                .unwrap()
                .contains("optimistic.etherscan.io")
        );
        assert!(
            contract_url("hyperevm", addr)
                .unwrap()
                .contains("hyperevmscan.io")
        );
        assert!(
            contract_url("robinhood", addr)
                .unwrap()
                .contains("robinhoodchain.blockscout.com")
        );
        assert!(contract_url("arbitrum", addr).is_none());
        assert!(contract_url("base", alloy::primitives::Address::ZERO).is_none());
    }

    /// The service table may lag the chain table but must never contradict
    /// it, and it leaves HyperEVM out on purpose.
    #[test]
    fn the_service_chain_table_agrees_with_the_chain_table_and_omits_hyperevm_on_purpose() {
        for chain in ["mainnet", "ethereum", "base", "optimism"] {
            assert_eq!(
                service_chain_id(chain),
                crate::chain::chain_id_for(chain),
                "{chain}"
            );
        }
        assert_eq!(crate::chain::chain_id_for("hyperevm"), Some(999));
        assert!(
            service_chain_id("hyperevm").is_none(),
            "Tenderly and Odos are not verified for HyperEVM"
        );
    }
}
