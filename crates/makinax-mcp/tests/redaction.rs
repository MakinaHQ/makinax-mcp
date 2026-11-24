//! A tool's error never carries an endpoint's key. The server is given a
//! keyed RPC URL on a closed loopback port, so every chain read fails with
//! a connection error that quotes the whole URL; what reaches the client
//! has the key masked and the host kept.

mod common;

use common::Session;
use makinax::ext::{MakinaxConfig, redact::MARK};
use makinax_mcp::MakinaxService;
use serde_json::json;

/// An obviously synthetic key, in the shape providers use.
const KEY: &str = "9fKd2Lm4Xp7Qw1Nz8Bt3Yr";

fn cfg_with_a_keyed_dead_endpoint() -> MakinaxConfig {
    serde_json::from_value(json!({
        "rpc_urls": { "base": format!("http://127.0.0.1:1/v2/{KEY}") },
        "modules": { "main": {
            "chain": "base",
            "safe": "0x1111111111111111111111111111111111111111",
            "module": "0x2222222222222222222222222222222222222222",
        }},
    }))
    .expect("the config parses")
}

#[track_caller]
fn assert_masked(text: &str) {
    assert!(!text.contains(KEY), "the key leaked: {text}");
    assert!(text.contains(MARK), "nothing was redacted: {text}");
    assert!(text.contains("127.0.0.1"), "the host must survive: {text}");
}

/// The control: the same failure, read straight from a provider on that
/// URL, does quote the key.
#[tokio::test]
async fn the_transport_error_itself_quotes_the_key() {
    use alloy::providers::{Provider, ProviderBuilder};
    let raw = ProviderBuilder::new()
        .connect_http(format!("http://127.0.0.1:1/v2/{KEY}").parse().unwrap())
        .get_block_number()
        .await
        .unwrap_err()
        .to_string();
    assert!(raw.contains(KEY), "{raw}");
}

#[tokio::test]
async fn failed_reads_reach_the_client_with_the_key_masked() {
    let svc = MakinaxService::new(cfg_with_a_keyed_dead_endpoint())
        .await
        .unwrap();
    let s = Session::start(svc).await;
    for (tool, args) in [
        // An error the library built.
        ("get_module_state", json!({"module": "main"})),
        (
            "get_balances",
            json!({"module": "main", "tokens": ["0x3333333333333333333333333333333333333333"]}),
        ),
        (
            "get_oracle_price",
            json!({"module": "main", "base": "0x3333333333333333333333333333333333333333"}),
        ),
        // An error this server words itself, around the provider's.
        (
            "get_module_events",
            json!({"module": "main", "from_block": 0}),
        ),
    ] {
        let r = s.call(tool, args).await;
        assert!(r.is_error, "{tool}: {}", r.text);
        assert_masked(&r.text);
    }
    s.stop().await;
}

/// A simulation that could not reach the chain says so in an error or in
/// the result's own failure field, depending on the build; neither carries
/// the key.
#[tokio::test]
async fn a_simulation_that_cannot_reach_the_chain_carries_no_key() {
    let svc = MakinaxService::new(cfg_with_a_keyed_dead_endpoint())
        .await
        .unwrap();
    let s = Session::start(svc).await;
    let r = s
        .call(
            "simulate_call",
            json!({
                "module": "main",
                "backend": "eth_call",
                "from": "0x1111111111111111111111111111111111111111",
                "call": {"kind": "raw", "to": "0x2222222222222222222222222222222222222222", "data": "0x12345678"},
            }),
        )
        .await;
    assert_masked(&r.text);
    s.stop().await;
}
