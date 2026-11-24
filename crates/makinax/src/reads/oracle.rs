//! Oracle reads: `getReferencePrice` / `getPrice` against the module's own oracle interface.
//!
//! The MakinaXModule embeds the OracleRegistry methods (per `IOracleRegistry.sol`).
//! We call them directly on the module address.

use alloy::{
    network::Ethereum,
    primitives::{Address, U256},
    providers::Provider,
};
use serde::Serialize;

use crate::{chain::module::IOracleRegistry, error::Result};

#[derive(Debug, Clone, Serialize)]
pub struct OraclePrice {
    pub base: Address,
    pub quote: Option<Address>,
    pub registered: bool,
    pub price: Option<String>, // u256 as decimal string
}

pub async fn read_price<P: Provider<Ethereum>>(
    provider: &P,
    module: Address,
    base: Address,
    quote: Option<Address>,
) -> Result<OraclePrice> {
    let oracle = IOracleRegistry::new(module, provider);

    let registered = oracle
        .isFeedRouteRegistered(base)
        .call()
        .await
        .map_err(|e| crate::error::Error::rpc(format!("isFeedRouteRegistered: {}", e)))?;

    if !registered {
        return Ok(OraclePrice {
            base,
            quote,
            registered: false,
            price: None,
        });
    }

    let price: U256 = match quote {
        None => oracle
            .getReferencePrice(base)
            .call()
            .await
            .map_err(|e| crate::error::Error::rpc(format!("getReferencePrice: {}", e)))?,
        Some(q) => oracle
            .getPrice(base, q)
            .call()
            .await
            .map_err(|e| crate::error::Error::rpc(format!("getPrice: {}", e)))?,
    };

    Ok(OraclePrice {
        base,
        quote,
        registered: true,
        price: Some(price.to_string()),
    })
}

/// One token's registered feed route, read from the module's own getters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeedRoute {
    pub token: Address,
    pub feed1: Address,
    /// `Address::ZERO` when the route has a single leg.
    pub feed2: Address,
    /// `getFeedStaleThreshold(feed1)`, in seconds.
    pub stale_threshold1_s: u64,
    /// `getFeedStaleThreshold(feed2)`, in seconds; `None` for a single leg.
    pub stale_threshold2_s: Option<u64>,
}

/// The feed routes registered for `tokens`, at one block.
///
/// The module keeps its routes in a private mapping with per-token getters
/// only, so the set cannot be enumerated: the caller names the candidate
/// tokens (e.g. [`super::balances::registered_tokens`] and the accounting
/// currency), and a token without a route is simply absent from the result.
/// Event history is never scanned. A staleness threshold is keyed by FEED,
/// so two tokens sharing an aggregator report the same value.
///
/// Any failed read is an error naming the call; nothing is defaulted.
pub async fn read_feed_routes<P: Provider<Ethereum>>(
    provider: &P,
    module: Address,
    tokens: &[Address],
    at_block: u64,
) -> Result<Vec<FeedRoute>> {
    let oracle = IOracleRegistry::new(module, provider);
    let rpc =
        |call: String, e: alloy::contract::Error| crate::error::Error::rpc(format!("{call}: {e}"));
    let threshold = |call: String, v: U256| {
        u64::try_from(v).map_err(|_| {
            crate::error::Error::Rpc(format!("{call}: value {v} does not fit in 64 bits"))
        })
    };
    let mut out = Vec::new();
    for token in tokens {
        let registered = oracle
            .isFeedRouteRegistered(*token)
            .block(at_block.into())
            .call()
            .await
            .map_err(|e| rpc(format!("isFeedRouteRegistered({token})"), e))?;
        if !registered {
            continue;
        }
        let route = oracle
            .getFeedRoute(*token)
            .block(at_block.into())
            .call()
            .await
            .map_err(|e| rpc(format!("getFeedRoute({token})"), e))?;
        // A registered route always has a non-zero feed1 (that is what
        // `isFeedRouteRegistered` checks); feed2 is zero for a single leg.
        let stale = |feed: Address| {
            let call = format!("getFeedStaleThreshold({feed})");
            let read = oracle.getFeedStaleThreshold(feed).block(at_block.into());
            async move {
                let v = read.call().await.map_err(|e| rpc(call.clone(), e))?;
                threshold(call, v)
            }
        };
        let stale_threshold1_s = stale(route.feed1).await?;
        let stale_threshold2_s = if route.feed2 == Address::ZERO {
            None
        } else {
            Some(stale(route.feed2).await?)
        };
        out.push(FeedRoute {
            token: *token,
            feed1: route.feed1,
            feed2: route.feed2,
            stale_threshold1_s,
            stale_threshold2_s,
        });
    }
    Ok(out)
}
