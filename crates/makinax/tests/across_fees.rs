#![cfg(feature = "fetch-across")]
//! The Across fee client's parser, against a recorded `suggested-fees`
//! response (USDC Base -> mainnet, 100 USDC). No network.

use alloy::primitives::{Address, U256};
use makinax::fetch::across::{AcrossFeeRequest, parse_suggested_fees};

const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
const USDC_MAINNET: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

fn recorded() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/across_suggested_fees.json")).unwrap()
}

fn req() -> AcrossFeeRequest {
    AcrossFeeRequest {
        input_token: USDC_BASE.parse().unwrap(),
        output_token: USDC_MAINNET.parse().unwrap(),
        origin_chain_id: 8453,
        destination_chain_id: 1,
        amount: U256::from(100_000_000u64),
    }
}

#[test]
fn parses_the_recorded_response() {
    let q = parse_suggested_fees(&recorded(), &req()).unwrap();
    assert_eq!(q.total_relay_fee, U256::from(192_059u64));
    assert_eq!(q.output_amount, U256::from(99_807_941u64));
    assert_eq!(
        q.output_amount + q.total_relay_fee,
        req().amount,
        "output + fee = input for a same-token lane"
    );
    assert!(!q.is_amount_too_low);
    assert_eq!(
        q.spoke_pool,
        "0x09aea4b2242abC8bb4BB78D537A67a245A7bEC64"
            .parse::<Address>()
            .unwrap(),
        "the Base SpokePool, as `bridges::across::spoke_pool_for(8453)` also names it"
    );
    assert_eq!(
        Some(q.spoke_pool),
        makinax::bridges::across::spoke_pool_for(8453)
    );
    assert_eq!(q.quote_timestamp, 1_790_325_251);
    assert_eq!(q.fill_deadline, Some(1_790_332_451));
    assert_eq!(q.exclusivity_deadline, 12);
    assert_eq!(q.min_deposit, U256::from(728_527u64));
    assert_eq!(q.max_deposit, U256::from(167_977_393_044u64));
    assert_eq!(q.estimated_fill_time_s, Some(2));
}

/// A quote for a different token or chain than asked is refused, naming the
/// field, never reused.
#[test]
fn a_quote_for_something_else_is_refused_naming_the_field() {
    let mut other_token = req();
    other_token.output_token = Address::repeat_byte(0x11);
    let e = parse_suggested_fees(&recorded(), &other_token).unwrap_err();
    assert!(e.to_string().contains("outputToken"), "{e}");

    let mut other_input = req();
    other_input.input_token = Address::repeat_byte(0x22);
    let e = parse_suggested_fees(&recorded(), &other_input).unwrap_err();
    assert!(e.to_string().contains("inputToken"), "{e}");

    let mut other_chain = req();
    other_chain.destination_chain_id = 10;
    let e = parse_suggested_fees(&recorded(), &other_chain).unwrap_err();
    assert!(e.to_string().contains("outputToken.chainId"), "{e}");
}

/// A missing `outputAmount` is refused, naming the field: it is not
/// derivable from the fee when the tokens' decimals differ.
#[test]
fn a_missing_output_amount_is_refused_by_name() {
    let mut v = recorded();
    v.as_object_mut().unwrap().remove("outputAmount");
    let e = parse_suggested_fees(&v, &req()).unwrap_err();
    assert!(e.to_string().contains("outputAmount"), "{e}");
}

/// Output-token units are not compared with input-token units: an
/// 18-decimal output for a 6-decimal input is reported as the API states it.
#[test]
fn an_output_in_other_decimals_is_reported_not_refused() {
    let mut v = recorded();
    v["outputAmount"] = serde_json::json!("99807941000000000000");
    let q = parse_suggested_fees(&v, &req()).unwrap();
    assert_eq!(q.output_amount, U256::from(99_807_941_000_000_000_000u128));
}

/// Each required field is required.
#[test]
fn every_required_field_is_checked() {
    for pointer in [
        "totalRelayFee",
        "isAmountTooLow",
        "timestamp",
        "spokePoolAddress",
        "exclusiveRelayer",
        "exclusivityDeadline",
        "limits",
    ] {
        let mut v = recorded();
        v.as_object_mut().unwrap().remove(pointer);
        assert!(
            parse_suggested_fees(&v, &req()).is_err(),
            "{pointer} missing must refuse"
        );
    }
    let mut v = recorded();
    v["spokePoolAddress"] = serde_json::json!("not an address");
    assert!(parse_suggested_fees(&v, &req()).is_err());
}

/// `fetch_suggested_fees` end to end over HTTP, against a loopback server
/// that serves the recorded response at the documented path.
#[tokio::test]
async fn fetch_suggested_fees_reads_the_served_response() {
    use axum::{Json, Router, routing::get};
    let body = recorded();
    let app = Router::new().route(
        "/api/suggested-fees",
        get(move || {
            let body = body.clone();
            async move { Json(body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{addr}/api");
    let q = makinax::ext::fetch_suggested_fees(&base, &req(), "makinax-test")
        .await
        .unwrap();
    assert_eq!(q.total_relay_fee, U256::from(192_059u64));
    assert_eq!(q.output_amount, U256::from(99_807_941u64));
}
