//! Odos SOR adapter (free tier, keyless — but aggressively rate-limited:
//! 429s are surfaced as structured errors and must never sink the other
//! adapter). POST /sor/quote/v2 with userAddr = MODULE → pathId; POST
//! /sor/assemble → transaction {to, data}. Parse functions are pure and
//! tested against recorded fixtures.

use alloy::primitives::{Address, Bytes, U256};
use serde_json::Value;

use super::http_client;
use super::quote::{DEFAULT_CLIENT_NAME, Quote, QuoteAdapter, QuoteRequest, min_out_for};
use crate::error::{Error, Result};

const BASE: &str = "https://api.odos.xyz";

fn err(stage: &str, detail: impl std::fmt::Display) -> Error {
    Error::rpc(format!("odos {}: {}", stage, detail))
}

/// Extract (path_id, expected_out, price_impact_pct) from /sor/quote/v2.
pub(crate) fn parse_quote_response(v: &Value) -> Result<(String, U256, Option<f64>)> {
    if let Some(detail) = v.get("detail").and_then(Value::as_str) {
        // Odos error shape ({"detail": "Rate limit exceeded..."}).
        return Err(err("quote", detail));
    }
    let path_id = v
        .get("pathId")
        .and_then(Value::as_str)
        .ok_or_else(|| err("quote", "missing pathId"))?
        .to_string();
    let out: U256 = v
        .get("outAmounts")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or_else(|| err("quote", "missing outAmounts[0]"))?
        .parse()
        .map_err(|e| err("quote", format!("bad outAmounts[0]: {}", e)))?;
    let impact = v.get("priceImpact").and_then(Value::as_f64).map(|p| -p);
    Ok((path_id, out, impact))
}

/// Extract (to, data) from /sor/assemble.
pub(crate) fn parse_assemble_response(v: &Value) -> Result<(Address, Bytes)> {
    if let Some(detail) = v.get("detail").and_then(Value::as_str) {
        return Err(err("assemble", detail));
    }
    let tx = v
        .get("transaction")
        .ok_or_else(|| err("assemble", "missing transaction"))?;
    let to: Address = tx
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| err("assemble", "missing transaction.to"))?
        .parse()
        .map_err(|e| err("assemble", format!("bad transaction.to: {}", e)))?;
    let data = tx
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| err("assemble", "missing transaction.data"))?;
    let data = alloy::primitives::hex::decode(data.trim_start_matches("0x"))
        .map(Bytes::from)
        .map_err(|e| err("assemble", format!("bad calldata hex: {}", e)))?;
    Ok((to, data))
}

/// An assemble field this adapter needs and the response lacks.
fn missing(field: &str) -> Error {
    Error::FetchRefused(format!("odos assemble: missing or invalid {field}"))
}

/// What the assembled transaction swaps: (`inputTokens[0].tokenAddress`,
/// `outputTokens[0].tokenAddress`, `inputTokens[0].amount`), as the vendor
/// answered — never copied from the request, so
/// [`crate::fetch::validate`] compares the answer with the question.
pub(crate) fn parse_assemble_identity(v: &Value) -> Result<(Address, Address, U256)> {
    let text = |list: &str, key: &str| {
        let path = format!("{list}[0].{key}");
        v.get(list)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|t| t.get(key))
            .and_then(Value::as_str)
            .ok_or_else(|| missing(&path))
            .map(|s| (s.to_string(), path))
    };
    let token = |list: &str| -> Result<Address> {
        let (s, path) = text(list, "tokenAddress")?;
        s.parse().map_err(|_| missing(&path))
    };
    let (amount, path) = text("inputTokens", "amount")?;
    let amount: U256 = amount.parse().map_err(|_| missing(&path))?;
    Ok((token("inputTokens")?, token("outputTokens")?, amount))
}

/// A quote field this adapter needs and the response lacks.
fn missing_in_quote(field: &str) -> Error {
    Error::FetchRefused(format!("odos quote: missing or invalid {field}"))
}

/// What the quote response priced: (`inTokens[0]`, `outTokens[0]`,
/// `inAmounts[0]`), as the vendor answered.
pub(crate) fn parse_quote_identity(v: &Value) -> Result<(Address, Address, U256)> {
    let first = |list: &str| {
        v.get(list)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
            .ok_or_else(|| missing_in_quote(&format!("{list}[0]")))
    };
    let token = |list: &str| -> Result<Address> {
        first(list)?
            .parse()
            .map_err(|_| missing_in_quote(&format!("{list}[0]")))
    };
    let amount: U256 = first("inAmounts")?
        .parse()
        .map_err(|_| missing_in_quote("inAmounts[0]"))?;
    Ok((token("inTokens")?, token("outTokens")?, amount))
}

/// Who the assembled transaction is sent by: `transaction.from`, as the
/// vendor answered.
pub(crate) fn parse_assemble_sender(v: &Value) -> Result<Address> {
    v.get("transaction")
        .and_then(|tx| tx.get("from"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| missing("transaction.from"))
}

/// The quote the two responses describe for `req`. Tokens and input amount
/// come from the assemble response, the expected output from the quote
/// response; the two must describe the same swap (same tokens, same input
/// amount), and the assembled transaction's sender must be the module, the
/// address both requests named. Addresses are compared as addresses, so
/// letter case does not matter. Pure: tested against recorded responses.
///
/// The calldata itself is not decoded: this checks what the assemble
/// response states about its transaction, not the bytes.
pub(crate) fn quote_from_responses(
    quote: &Value,
    assemble: &Value,
    req: &QuoteRequest<'_>,
) -> Result<Quote> {
    use crate::fetch::validate::check_field;
    let (_path_id, expected_out, impact) = parse_quote_response(quote)?;
    let (priced_in, priced_out, priced_amount) = parse_quote_identity(quote)?;
    let (to, data) = parse_assemble_response(assemble)?;
    let (input_token, output_token, input_amount) = parse_assemble_identity(assemble)?;
    check_field(
        "assemble.inputTokens[0].tokenAddress",
        priced_in,
        input_token,
    )?;
    check_field(
        "assemble.outputTokens[0].tokenAddress",
        priced_out,
        output_token,
    )?;
    check_field(
        "assemble.inputTokens[0].amount",
        priced_amount,
        input_amount,
    )?;
    check_field(
        "assemble.transaction.from",
        req.module,
        parse_assemble_sender(assemble)?,
    )?;
    Ok(Quote {
        adapter: "odos",
        input_token,
        output_token,
        input_amount,
        expected_out,
        min_out: min_out_for(expected_out, req.slippage_bps),
        slippage_bps: req.slippage_bps as u16,
        data,
        execution_target: to,
        approval_target: to,
        price_impact_pct: impact,
        valid_hint_s: 30,
    })
}

/// The Odos adapter. `client_name` is sent as the HTTP `User-Agent`.
pub struct Odos {
    client_name: String,
}

impl Odos {
    pub fn new(client_name: impl Into<String>) -> Self {
        Self {
            client_name: client_name.into(),
        }
    }
}

impl Odos {
    /// The headers every request carries (`User-Agent` = the client name).
    pub fn headers(&self) -> reqwest::header::HeaderMap {
        super::client_headers(&self.client_name)
    }
}

impl Default for Odos {
    fn default() -> Self {
        Self::new(DEFAULT_CLIENT_NAME)
    }
}

#[async_trait::async_trait]
impl QuoteAdapter for Odos {
    fn name(&self) -> &'static str {
        "odos"
    }

    async fn quote(&self, req: &QuoteRequest<'_>) -> Result<Quote> {
        let chain_id = crate::chains::service_chain_id(req.chain)
            .ok_or_else(|| err("quote", format!("unsupported chain `{}`", req.chain)))?;
        let client = http_client(&self.client_name);

        let quote_body = serde_json::json!({
            "chainId": chain_id,
            "inputTokens": [{"tokenAddress": format!("{:?}", req.input_token), "amount": req.input_amount.to_string()}],
            "outputTokens": [{"tokenAddress": format!("{:?}", req.output_token), "proportion": 1}],
            "userAddr": format!("{:?}", req.module),
            "slippageLimitPercent": req.slippage_bps as f64 / 100.0,
            "disableRFQs": true,
            "compact": true,
        });
        let resp = client
            .post(format!("{}/sor/quote/v2", BASE))
            .json(&quote_body)
            .send()
            .await
            .map_err(|e| err("quote", e))?;
        if resp.status().as_u16() == 429 {
            return Err(err(
                "quote",
                "rate limited (429) — keyless tier; retry shortly",
            ));
        }
        let quote: Value = resp.json().await.map_err(|e| err("quote json", e))?;
        let (path_id, _expected_out, _impact) = parse_quote_response(&quote)?;

        let v: Value = client
            .post(format!("{}/sor/assemble", BASE))
            .json(&serde_json::json!({
                "userAddr": format!("{:?}", req.module),
                "pathId": path_id,
                "simulate": false,
            }))
            .send()
            .await
            .map_err(|e| err("assemble", e))?
            .json()
            .await
            .map_err(|e| err("assemble json", e))?;
        quote_from_responses(&quote, &v, req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Option<Value> {
        let p = format!(
            "{}/tests/fixtures/quotes/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    #[test]
    fn parses_recorded_quote_response() {
        // Fixture may be the recorded 429 body if Odos was rate-limiting at
        // capture time — the parse must handle BOTH shapes without panicking.
        let v = fixture("odos_quote.json").expect("fixture present");
        match parse_quote_response(&v) {
            Ok((path_id, out, _impact)) => {
                assert!(!path_id.is_empty());
                assert!(out > U256::from(10u64).pow(U256::from(16u64)));
            }
            Err(e) => {
                assert!(
                    e.to_string().to_lowercase().contains("rate limit"),
                    "unexpected: {}",
                    e
                );
            }
        }
    }

    #[test]
    fn parses_recorded_assemble_response() {
        if let Some(v) = fixture("odos_assemble.json") {
            match parse_assemble_response(&v) {
                Ok((to, data)) => {
                    assert_ne!(to, Address::ZERO);
                    assert!(data.len() > 4);
                }
                Err(e) => assert!(e.to_string().to_lowercase().contains("rate limit"), "{}", e),
            }
        }
        // Synthetic schema check regardless of live capture availability.
        let synth = serde_json::json!({
            "transaction": {"to": "0xCf5540fFFCdC3d510B18bFcA6d2b9987b0772559", "data": "0x1234abcd", "value": "0"}
        });
        let (to, data) = parse_assemble_response(&synth).unwrap();
        assert_eq!(
            format!("{:?}", to),
            "0xcf5540fffcdc3d510b18bfca6d2b9987b0772559"
        );
        assert_eq!(data.len(), 4);
    }

    /// The recorded request: 100 USDC -> WETH on mainnet, for the module
    /// address the responses were recorded with (`transaction.from`).
    fn recorded_req() -> QuoteRequest<'static> {
        QuoteRequest {
            chain: "mainnet",
            input_token: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"
                .parse()
                .unwrap(),
            output_token: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"
                .parse()
                .unwrap(),
            input_amount: U256::from(100_000_000u64),
            slippage_bps: 50,
            module: "0x5afe00000000000000000000000000000000b001"
                .parse()
                .unwrap(),
        }
    }

    /// The quote's identities are the VENDOR's answer: the recorded responses
    /// pass the validator, and an assembled transaction for another token or
    /// amount is refused by field name.
    #[test]
    fn an_assembly_for_another_token_or_amount_is_refused() {
        use crate::fetch::validate::{SwapExpectation, validate_swap_quote};
        let req = recorded_req();
        let quote = fixture("odos_quote.json").expect("fixture present");
        let assemble = fixture("odos_assemble.json").expect("fixture present");
        let q = quote_from_responses(&quote, &assemble, &req).unwrap();
        let want = SwapExpectation {
            module: req.module,
            swapper_target: q.execution_target,
            input_token: req.input_token,
            output_token: req.output_token,
            input_amount: req.input_amount,
        };
        validate_swap_quote(&q, &want).unwrap();

        let other = serde_json::json!("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913");
        for (list, key, priced, value, field) in [
            (
                "outputTokens",
                "tokenAddress",
                "outTokens",
                other.clone(),
                "output_token",
            ),
            (
                "inputTokens",
                "tokenAddress",
                "inTokens",
                other,
                "input_token",
            ),
            (
                "inputTokens",
                "amount",
                "inAmounts",
                serde_json::json!("100000001"),
                "input_amount",
            ),
        ] {
            // Both responses answer for the other swap: the pair agrees, so
            // it is the comparison with the request that refuses it.
            let mut bent = assemble.clone();
            bent[list][0][key] = value.clone();
            let mut quote = quote.clone();
            quote[priced][0] = value;
            let q = quote_from_responses(&quote, &bent, &req).unwrap();
            let e = validate_swap_quote(&q, &want).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{e:?}");
            assert!(
                e.to_string().contains(&format!("{field}: expected ")),
                "{e}"
            );
        }
    }

    #[test]
    fn an_assembly_without_its_identities_is_refused() {
        let req = recorded_req();
        let quote = fixture("odos_quote.json").expect("fixture present");
        for (list, key) in [
            ("inputTokens", "tokenAddress"),
            ("outputTokens", "tokenAddress"),
            ("inputTokens", "amount"),
        ] {
            let mut bent = fixture("odos_assemble.json").expect("fixture present");
            bent[list][0].as_object_mut().unwrap().remove(key);
            let e = quote_from_responses(&quote, &bent, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{e:?}");
            assert!(e.to_string().contains(&format!("{list}[0].{key}")), "{e}");
        }
    }

    #[test]
    fn schema_drift_is_an_error_not_a_panic() {
        assert!(
            parse_quote_response(&serde_json::json!({"detail": "Rate limit exceeded"})).is_err()
        );
        assert!(parse_quote_response(&serde_json::json!({})).is_err());
        assert!(parse_assemble_response(&serde_json::json!({"transaction": {}})).is_err());
        assert!(parse_assemble_response(&serde_json::json!(null)).is_err());
    }

    /// The calldata comes from the assemble response, the price from the
    /// quote response: an assembled transaction for another token or
    /// amount than the one priced, or sent by another address than the
    /// module, is refused by field name and never becomes a quote.
    #[test]
    fn an_assembly_that_differs_from_the_quote_or_the_module_is_refused() {
        let req = recorded_req();
        let quote = fixture("odos_quote.json").expect("fixture present");
        let assemble = fixture("odos_assemble.json").expect("fixture present");
        let other = serde_json::json!("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913");
        for (pointer, value) in [
            ("/inputTokens/0/tokenAddress", other.clone()),
            ("/outputTokens/0/tokenAddress", other.clone()),
            ("/inputTokens/0/amount", serde_json::json!("100000001")),
            ("/transaction/from", other),
        ] {
            let mut bent = assemble.clone();
            *bent.pointer_mut(pointer).unwrap() = value;
            let e = quote_from_responses(&quote, &bent, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{pointer}: {e:?}");
            let field = pointer[1..].replace("/0/", "[0].").replace('/', ".");
            assert!(
                e.to_string()
                    .contains(&format!("assemble.{field}: expected ")),
                "{pointer}: {e}"
            );
        }
        // The same disagreement from the other side: the quote priced
        // another swap than the one assembled.
        let mut priced = quote.clone();
        priced["inAmounts"][0] = serde_json::json!("100000001");
        let e = quote_from_responses(&priced, &assemble, &req).unwrap_err();
        assert_eq!(
            e.to_string(),
            "fetched data refused: assemble.inputTokens[0].amount: expected 100000001, got 100000000"
        );
        // A request for another module than the one the transaction is
        // sent by.
        let mut elsewhere = recorded_req();
        elsewhere.module = Address::repeat_byte(9);
        let e = quote_from_responses(&quote, &assemble, &elsewhere).unwrap_err();
        assert!(
            e.to_string()
                .contains("assemble.transaction.from: expected "),
            "{e}"
        );
    }

    /// Without what it priced, or without its sender, the pair cannot be
    /// matched and is refused.
    #[test]
    fn a_quote_without_its_identities_or_an_assembly_without_its_sender_is_refused() {
        let req = recorded_req();
        let assemble = fixture("odos_assemble.json").expect("fixture present");
        for list in ["inTokens", "outTokens", "inAmounts"] {
            let mut bent = fixture("odos_quote.json").expect("fixture present");
            bent.as_object_mut().unwrap().remove(list);
            let e = quote_from_responses(&bent, &assemble, &req).unwrap_err();
            assert_eq!(
                e.to_string(),
                format!("fetched data refused: odos quote: missing or invalid {list}[0]")
            );
            let mut bent = fixture("odos_quote.json").expect("fixture present");
            bent[list][0] = serde_json::json!("not a value");
            let e = quote_from_responses(&bent, &assemble, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{list}: {e:?}");
        }
        let quote = fixture("odos_quote.json").expect("fixture present");
        let mut bent = assemble.clone();
        bent["transaction"].as_object_mut().unwrap().remove("from");
        let e = quote_from_responses(&quote, &bent, &req).unwrap_err();
        assert_eq!(
            e.to_string(),
            "fetched data refused: odos assemble: missing or invalid transaction.from"
        );
    }

    /// The recorded pair agrees, whatever the letter case of its addresses.
    #[test]
    fn the_recorded_pair_agrees_in_any_letter_case() {
        let req = recorded_req();
        let assemble = fixture("odos_assemble.json").expect("fixture present");
        let mut quote = fixture("odos_quote.json").expect("fixture present");
        quote["inTokens"][0] = serde_json::json!("0xA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48");
        let q = quote_from_responses(&quote, &assemble, &req).unwrap();
        assert_eq!(q.input_token, req.input_token);
        assert_eq!(q.input_amount, req.input_amount);
    }

    /// A transport failure's text can quote a URL; a key in it is masked.
    #[test]
    fn an_error_text_masks_an_endpoint_key() {
        let e = err(
            "quote",
            "error sending request for url (https://proxy.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr)",
        );
        assert_eq!(
            e.to_string(),
            "rpc: odos quote: error sending request for url (https://proxy.example.com/v2/<redacted>)"
        );
    }
}
