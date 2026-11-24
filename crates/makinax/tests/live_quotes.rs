#![cfg(all(feature = "fetch-kyber", feature = "fetch-odos"))]
//! Live quote smoke test — real Odos + Kyber quotes, NO execution. Sanity
//! band: the implied ETH price from the best quote must sit within ±3% of
//! Chainlink ETH/USD. A single adapter failing (Odos keyless tier rate-limits
//! aggressively) is a warning; BOTH failing fails the test.
//!
//! Run: cargo test -p makinax --test live_quotes -- --ignored --nocapture
//! (`LIVE_RPC_MAINNET` overrides the public mainnet RPC used for the
//! Chainlink read.)

use std::sync::Arc;

use alloy::{
    primitives::{Address, U256},
    providers::ProviderBuilder,
    sol,
};
use makinax::fetch::{
    kyber::Kyber,
    odos::Odos,
    quote::{QuoteAdapter, QuoteRequest, best_quote},
};

const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
/// A placeholder module address: the aggregators quote and build for any
/// sender/recipient (nothing is sent), so no real deployment is named here.
const MODULE: &str = "0x1111111111111111111111111111111111111111";
const CHAINLINK_ETH_USD: &str = "0x5f4eC3Df9cbd43714FE2740f5E3616155c5b8419";

sol! {
    #[sol(rpc)]
    interface IAggregator {
        function latestAnswer() external view returns (int256);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live aggregator + RPC calls; run with --ignored"]
async fn live_quotes_sanity_band() {
    let adapters: Vec<Arc<dyn QuoteAdapter>> =
        vec![Arc::new(Odos::default()), Arc::new(Kyber::default())];
    let req = QuoteRequest {
        chain: "mainnet",
        input_token: USDC.parse().unwrap(),
        output_token: WETH.parse().unwrap(),
        input_amount: U256::from(100_000_000u64), // 100 USDC
        slippage_bps: 50,
        module: MODULE.parse().unwrap(),
    };
    let (best, summaries) = best_quote(&adapters, &req)
        .await
        .expect("both adapters failed");
    println!(
        "summaries: {}",
        serde_json::to_string_pretty(&summaries).unwrap()
    );
    for s in &summaries {
        if let Some(e) = &s.error {
            println!("WARN: adapter {} failed live: {}", s.adapter, e);
        }
    }
    assert!(
        summaries.iter().any(|s| s.chosen),
        "best-selection exercised"
    );
    assert!(!best.data.is_empty());
    assert_ne!(best.execution_target, Address::ZERO);
    assert!(best.min_out < best.expected_out);

    // Implied price: 100 USDC bought expected_out WETH ⇒ ETH/USD ≈ 100 / (out/1e18)
    let out_eth = u128::try_from(best.expected_out).unwrap() as f64 / 1e18;
    let implied = 100.0 / out_eth;

    let rpc = std::env::var("LIVE_RPC_MAINNET")
        .unwrap_or_else(|_| "https://ethereum-rpc.publicnode.com".to_string());
    let provider = ProviderBuilder::new().connect_http(rpc.parse().unwrap());
    let feed = IAggregator::new(CHAINLINK_ETH_USD.parse().unwrap(), &provider);
    let answer = feed.latestAnswer().call().await.expect("chainlink read");
    let chainlink = i128::try_from(answer).unwrap() as f64 / 1e8;

    let dev = ((implied - chainlink) / chainlink).abs();
    println!(
        "best={} implied ETH/USD={:.2} chainlink={:.2} deviation={:.3}%",
        best.adapter,
        implied,
        chainlink,
        dev * 100.0
    );
    assert!(
        dev <= 0.03,
        "implied price {:.2} deviates {:.2}% from Chainlink {:.2}",
        implied,
        dev * 100.0,
        chainlink
    );
    let _ = IAggregator::latestAnswerCall {}; // keep SolCall import exercised
}
