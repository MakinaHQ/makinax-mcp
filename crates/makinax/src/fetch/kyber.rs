//! KyberSwap aggregator adapter (free, keyless).
//! GET /{chain}/api/v1/routes → routeSummary + router; POST /route/build with
//! sender/recipient = MODULE → calldata. Parse functions are pure and tested
//! against recorded fixtures (tests/fixtures/quotes/kyber_*.json).

use alloy::primitives::{Address, Bytes, U256};
use serde_json::Value;

use super::http_client;
use super::quote::{DEFAULT_CLIENT_NAME, Quote, QuoteAdapter, QuoteRequest, min_out_for};
use crate::error::{Error, Result};

const BASE: &str = "https://aggregator-api.kyberswap.com";

pub(crate) fn chain_slug(chain: &str) -> Option<&'static str> {
    match chain {
        "mainnet" | "ethereum" => Some("ethereum"),
        "base" => Some("base"),
        "hyperevm" => Some("hyperevm"),
        "arbitrum" | "arbitrum-one" => Some("arbitrum"),
        "optimism" => Some("optimism"),
        "polygon" => Some("polygon"),
        _ => None,
    }
}

fn err(stage: &str, detail: impl std::fmt::Display) -> Error {
    Error::rpc(format!("kyber {}: {}", stage, detail))
}

/// Extract (routeSummary, routerAddress, amountOut) from the routes response.
pub(crate) fn parse_route_response(v: &Value) -> Result<(Value, Address, U256)> {
    if v.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(err(
            "routes",
            v.get("message")
                .and_then(Value::as_str)
                .unwrap_or("non-zero code"),
        ));
    }
    let data = v.get("data").ok_or_else(|| err("routes", "missing data"))?;
    let summary = data
        .get("routeSummary")
        .cloned()
        .ok_or_else(|| err("routes", "missing routeSummary"))?;
    let router: Address = data
        .get("routerAddress")
        .and_then(Value::as_str)
        .ok_or_else(|| err("routes", "missing routerAddress"))?
        .parse()
        .map_err(|e| err("routes", format!("bad routerAddress: {}", e)))?;
    let out: U256 = summary
        .get("amountOut")
        .and_then(Value::as_str)
        .ok_or_else(|| err("routes", "missing amountOut"))?
        .parse()
        .map_err(|e| err("routes", format!("bad amountOut: {}", e)))?;
    Ok((summary, router, out))
}

/// Extract (calldata, routerAddress, amountOut, price_impact_pct) from build.
pub(crate) fn parse_build_response(v: &Value) -> Result<(Bytes, Address, U256, Option<f64>)> {
    if v.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(err(
            "build",
            v.get("message")
                .and_then(Value::as_str)
                .unwrap_or("non-zero code"),
        ));
    }
    let data = v.get("data").ok_or_else(|| err("build", "missing data"))?;
    let calldata_hex = data
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| err("build", "missing calldata"))?;
    let calldata = alloy::primitives::hex::decode(calldata_hex.trim_start_matches("0x"))
        .map(Bytes::from)
        .map_err(|e| err("build", format!("bad calldata hex: {}", e)))?;
    let router: Address = data
        .get("routerAddress")
        .and_then(Value::as_str)
        .ok_or_else(|| err("build", "missing routerAddress"))?
        .parse()
        .map_err(|e| err("build", format!("bad routerAddress: {}", e)))?;
    let out: U256 = data
        .get("amountOut")
        .and_then(Value::as_str)
        .ok_or_else(|| err("build", "missing amountOut"))?
        .parse()
        .map_err(|e| err("build", format!("bad amountOut: {}", e)))?;
    let impact = match (
        data.get("amountInUsd").and_then(as_f64_flex),
        data.get("amountOutUsd").and_then(as_f64_flex),
    ) {
        (Some(inn), Some(out_usd)) if inn > 0.0 => Some(((inn - out_usd) / inn) * 100.0),
        _ => None,
    };
    Ok((calldata, router, out, impact))
}

/// A route summary field this adapter needs and the response lacks.
fn missing(field: &str) -> Error {
    Error::FetchRefused(format!("kyber routes: missing or invalid {field}"))
}

/// What the route actually swaps: (`tokenIn`, `tokenOut`, `amountIn`) from
/// `routeSummary`, as the vendor answered — never copied from the request,
/// so [`crate::fetch::validate`] compares the answer with the question.
pub(crate) fn parse_route_identity(summary: &Value) -> Result<(Address, Address, U256)> {
    let field = |name: &str| {
        summary
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| missing(&format!("routeSummary.{name}")))
    };
    let token = |name: &str| -> Result<Address> {
        field(name)?
            .parse()
            .map_err(|_| missing(&format!("routeSummary.{name}")))
    };
    let amount_in: U256 = field("amountIn")?
        .parse()
        .map_err(|_| missing("routeSummary.amountIn"))?;
    Ok((token("tokenIn")?, token("tokenOut")?, amount_in))
}

/// A build response field this adapter needs and the response lacks.
fn missing_in_build(field: &str) -> Error {
    Error::FetchRefused(format!("kyber build: missing or invalid {field}"))
}

/// What the build response says about the swap its calldata performs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildIdentity {
    /// `data.amountIn`. The build response always carries it.
    pub amount_in: U256,
    /// `data.tokenIn` / `data.tokenOut`, when the response states them.
    pub token_in: Option<Address>,
    pub token_out: Option<Address>,
    /// `data.sender` / `data.recipient`, when the response states them.
    pub sender: Option<Address>,
    pub recipient: Option<Address>,
}

/// Read [`BuildIdentity`] from the build response, as the vendor answered.
/// `amountIn` is required. The other fields are read when present; one
/// that is present but is not an address is refused, not skipped.
pub(crate) fn parse_build_identity(v: &Value) -> Result<BuildIdentity> {
    let data = v.get("data").ok_or_else(|| missing_in_build("data"))?;
    let amount_in: U256 = data
        .get("amountIn")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| missing_in_build("amountIn"))?;
    let address = |name: &str| -> Result<Option<Address>> {
        match data.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(stated) => stated
                .as_str()
                .and_then(|s| s.parse().ok())
                .map(Some)
                .ok_or_else(|| missing_in_build(name)),
        }
    };
    Ok(BuildIdentity {
        amount_in,
        token_in: address("tokenIn")?,
        token_out: address("tokenOut")?,
        sender: address("sender")?,
        recipient: address("recipient")?,
    })
}

/// Refuse a build response that is not for the routed swap: its input
/// amount must be the route summary's, exactly; a token it states must be
/// the route summary's; a sender or recipient it states must be `module`,
/// the address the build request named for both. Addresses are compared as
/// addresses, so letter case does not matter.
fn check_build_matches_route(
    build: &BuildIdentity,
    (token_in, token_out, amount_in): (Address, Address, U256),
    module: Address,
) -> Result<()> {
    use crate::fetch::validate::check_field;
    check_field("build.amountIn", amount_in, build.amount_in)?;
    for (field, expected, stated) in [
        ("build.tokenIn", token_in, build.token_in),
        ("build.tokenOut", token_out, build.token_out),
        ("build.sender", module, build.sender),
        ("build.recipient", module, build.recipient),
    ] {
        if let Some(stated) = stated {
            check_field(field, expected, stated)?;
        }
    }
    Ok(())
}

/// The quote the two responses describe for `req`. Tokens and input amount
/// come from the route summary; the calldata and its router come from the
/// build response, which must be for the same swap: the same router, the
/// same input amount, and the same tokens, sender and recipient wherever it
/// states them ([`check_build_matches_route`]). Pure: tested against
/// recorded responses.
///
/// The calldata itself is not decoded. Kyber's build response states the
/// input amount and the router but not the tokens or the recipient, so for
/// those this checks the request that was sent ([`Kyber::build_body`]) and
/// any field the response does carry, not the bytes.
pub(crate) fn quote_from_responses(
    route: &Value,
    build: &Value,
    req: &QuoteRequest<'_>,
) -> Result<Quote> {
    let (summary, route_router, _route_out) = parse_route_response(route)?;
    let (input_token, output_token, input_amount) = parse_route_identity(&summary)?;
    let (calldata, router, expected_out, impact) = parse_build_response(build)?;
    crate::fetch::validate::check_field("execution_target", route_router, router)?;
    check_build_matches_route(
        &parse_build_identity(build)?,
        (input_token, output_token, input_amount),
        req.module,
    )?;
    Ok(Quote {
        adapter: "kyber",
        input_token,
        output_token,
        input_amount,
        expected_out,
        min_out: min_out_for(expected_out, req.slippage_bps),
        slippage_bps: req.slippage_bps as u16,
        data: calldata,
        execution_target: router,
        approval_target: router,
        price_impact_pct: impact,
        valid_hint_s: 30,
    })
}

fn as_f64_flex(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// The Kyber adapter. `client_name` is sent as the HTTP `User-Agent` and as the build request's `source`.
pub struct Kyber {
    client_name: String,
}

impl Kyber {
    pub fn new(client_name: impl Into<String>) -> Self {
        Self {
            client_name: client_name.into(),
        }
    }
}

impl Kyber {
    /// The headers every request carries (`User-Agent` = the client name).
    pub fn headers(&self) -> reqwest::header::HeaderMap {
        super::client_headers(&self.client_name)
    }

    /// The `/route/build` request body: sender AND recipient are the module,
    /// `source` is the client name. Pure, so what goes on the wire is
    /// testable without a network.
    pub fn build_body(&self, route_summary: &Value, req: &QuoteRequest<'_>) -> Value {
        serde_json::json!({
            "routeSummary": route_summary,
            "sender": format!("{:?}", req.module),
            "recipient": format!("{:?}", req.module),
            "slippageTolerance": req.slippage_bps,
            "source": self.client_name,
        })
    }
}

impl Default for Kyber {
    fn default() -> Self {
        Self::new(DEFAULT_CLIENT_NAME)
    }
}

#[async_trait::async_trait]
impl QuoteAdapter for Kyber {
    fn name(&self) -> &'static str {
        "kyber"
    }

    async fn quote(&self, req: &QuoteRequest<'_>) -> Result<Quote> {
        let slug = chain_slug(req.chain)
            .ok_or_else(|| err("routes", format!("unsupported chain `{}`", req.chain)))?;
        let client = http_client(&self.client_name);

        let routes_url = format!(
            "{}/{}/api/v1/routes?tokenIn={:?}&tokenOut={:?}&amountIn={}",
            BASE, slug, req.input_token, req.output_token, req.input_amount
        );
        let route: Value = client
            .get(&routes_url)
            .send()
            .await
            .map_err(|e| err("routes", e))?
            .json()
            .await
            .map_err(|e| err("routes json", e))?;
        let (summary, _router, _route_out) = parse_route_response(&route)?;

        let build_body = self.build_body(&summary, req);
        let v: Value = client
            .post(format!("{}/{}/api/v1/route/build", BASE, slug))
            .json(&build_body)
            .send()
            .await
            .map_err(|e| err("build", e))?
            .json()
            .await
            .map_err(|e| err("build json", e))?;
        quote_from_responses(&route, &v, req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_hyperevm_without_changing_other_chain_mappings() {
        for (chain, slug) in [
            ("mainnet", "ethereum"),
            ("ethereum", "ethereum"),
            ("base", "base"),
            ("hyperevm", "hyperevm"),
            ("arbitrum", "arbitrum"),
            ("arbitrum-one", "arbitrum"),
            ("optimism", "optimism"),
            ("polygon", "polygon"),
        ] {
            assert_eq!(chain_slug(chain), Some(slug));
        }
        // HyperCore is not an EVM quote chain; don't silently alias unknown names.
        for chain in ["hypercore", "unknown", "", "999"] {
            assert_eq!(chain_slug(chain), None);
        }
    }

    fn fixture(name: &str) -> Value {
        let p = format!(
            "{}/tests/fixtures/quotes/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        );
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn parses_recorded_route_response() {
        let (summary, router, out) = parse_route_response(&fixture("kyber_route.json")).unwrap();
        assert_eq!(
            format!("{:?}", router),
            "0x6131b5fae19ea4f9d964eac0408e4408b66337b5"
        );
        assert!(
            out > U256::from(10u64).pow(U256::from(16u64)),
            "≈0.057 WETH for 100 USDC"
        );
        assert!(
            summary.get("checksum").is_some(),
            "routeSummary passed through verbatim"
        );
    }

    #[test]
    fn parses_recorded_build_response() {
        let (data, router, out, impact) =
            parse_build_response(&fixture("kyber_build.json")).unwrap();
        assert!(data.len() > 100);
        assert_eq!(
            format!("{:?}", router),
            "0x6131b5fae19ea4f9d964eac0408e4408b66337b5"
        );
        assert_eq!(out, U256::from(57_561_865_502_960_421u64));
        let impact = impact.expect("USD legs present in fixture");
        assert!(
            impact.abs() < 3.0,
            "sane impact for a 100 USDC swap: {}",
            impact
        );
    }

    /// The recorded request: 100 USDC -> WETH on mainnet.
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
            module: Address::repeat_byte(9),
        }
    }

    fn expectation(req: &QuoteRequest<'_>) -> crate::fetch::validate::SwapExpectation {
        crate::fetch::validate::SwapExpectation {
            module: req.module,
            swapper_target: crate::chain::swappers::KYBER_ROUTER,
            input_token: req.input_token,
            output_token: req.output_token,
            input_amount: req.input_amount,
        }
    }

    /// The quote's identities are the VENDOR's answer: the recorded responses
    /// pass the validator, and a route summary for another token or amount
    /// is refused by field name.
    #[test]
    fn a_route_for_another_token_or_amount_is_refused() {
        use crate::fetch::validate::validate_swap_quote;
        let req = recorded_req();
        let (route, build) = (fixture("kyber_route.json"), fixture("kyber_build.json"));
        let q = quote_from_responses(&route, &build, &req).unwrap();
        validate_swap_quote(&q, &expectation(&req)).unwrap();

        for (key, value, field) in [
            (
                "tokenOut",
                "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "output_token",
            ),
            (
                "tokenIn",
                "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                "input_token",
            ),
            ("amountIn", "100000001", "input_amount"),
        ] {
            let mut bent = route.clone();
            bent["data"]["routeSummary"][key] = serde_json::json!(value);
            // Both responses answer for the other swap: the pair agrees, so
            // it is the comparison with the request that refuses it. (The
            // build response states the amount, not the tokens.)
            let mut build = build.clone();
            if key == "amountIn" {
                build["data"][key] = serde_json::json!(value);
            }
            let q = quote_from_responses(&bent, &build, &req).unwrap();
            let e = validate_swap_quote(&q, &expectation(&req)).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{e:?}");
            assert!(
                e.to_string().contains(&format!("{field}: expected ")),
                "{e}"
            );
        }
    }

    #[test]
    fn a_route_without_its_identities_or_with_another_router_is_refused() {
        let req = recorded_req();
        let build = fixture("kyber_build.json");
        for key in ["tokenIn", "tokenOut", "amountIn"] {
            let mut bent = fixture("kyber_route.json");
            bent["data"]["routeSummary"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            let e = quote_from_responses(&bent, &build, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{e:?}");
            assert!(
                e.to_string().contains(&format!("routeSummary.{key}")),
                "{e}"
            );
        }
        let mut other = fixture("kyber_build.json");
        other["data"]["routerAddress"] =
            serde_json::json!("0x00000000000000000000000000000000000000ee");
        let e = quote_from_responses(&fixture("kyber_route.json"), &other, &req).unwrap_err();
        assert!(e.to_string().contains("execution_target: expected "), "{e}");
    }

    #[test]
    fn schema_drift_is_an_error_not_a_panic() {
        assert!(
            parse_route_response(&serde_json::json!({"code": 4008, "message": "route not found"}))
                .is_err()
        );
        assert!(parse_route_response(&serde_json::json!({"code": 0})).is_err());
        assert!(parse_build_response(&serde_json::json!({"code": 0, "data": {}})).is_err());
        assert!(parse_build_response(&serde_json::json!([1, 2, 3])).is_err());
    }

    /// The calldata comes from the build response, the swap's identity from
    /// the route summary: a build response for another amount, token,
    /// sender or recipient is refused by field name, and never becomes a
    /// quote.
    #[test]
    fn a_build_for_another_swap_is_refused() {
        let req = recorded_req();
        let route = fixture("kyber_route.json");
        let other = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
        for (key, value, field) in [
            ("amountIn", "100000001", "build.amountIn"),
            ("tokenIn", other, "build.tokenIn"),
            ("tokenOut", other, "build.tokenOut"),
            ("sender", other, "build.sender"),
            ("recipient", other, "build.recipient"),
        ] {
            let mut bent = fixture("kyber_build.json");
            bent["data"][key] = serde_json::json!(value);
            let e = quote_from_responses(&route, &bent, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{key}: {e:?}");
            assert!(
                e.to_string().contains(&format!("{field}: expected ")),
                "{key}: {e}"
            );
        }
    }

    /// The exact refusal text, once: `<field>: expected <route>, got <build>`.
    #[test]
    fn the_refusal_names_the_field_and_both_values() {
        let mut bent = fixture("kyber_build.json");
        bent["data"]["amountIn"] = serde_json::json!("100000001");
        let e =
            quote_from_responses(&fixture("kyber_route.json"), &bent, &recorded_req()).unwrap_err();
        assert_eq!(
            e.to_string(),
            "fetched data refused: build.amountIn: expected 100000000, got 100000001"
        );
    }

    /// A build response that does not state its input amount cannot be
    /// matched to the route and is refused; so is a stated identity that is
    /// not readable.
    #[test]
    fn a_build_without_its_amount_or_with_an_unreadable_identity_is_refused() {
        let req = recorded_req();
        let route = fixture("kyber_route.json");
        let mut bent = fixture("kyber_build.json");
        bent["data"].as_object_mut().unwrap().remove("amountIn");
        let e = quote_from_responses(&route, &bent, &req).unwrap_err();
        assert_eq!(
            e.to_string(),
            "fetched data refused: kyber build: missing or invalid amountIn"
        );
        for (key, value) in [
            ("amountIn", serde_json::json!("many")),
            ("amountIn", serde_json::json!(100000000)),
            ("tokenIn", serde_json::json!("usdc")),
            ("tokenOut", serde_json::json!(7)),
            ("recipient", serde_json::json!("0x1234")),
        ] {
            let mut bent = fixture("kyber_build.json");
            bent["data"][key] = value;
            let e = quote_from_responses(&route, &bent, &req).unwrap_err();
            assert!(matches!(e, Error::FetchRefused(_)), "{key}: {e:?}");
            assert!(
                e.to_string()
                    .contains(&format!("kyber build: missing or invalid {key}")),
                "{key}: {e}"
            );
        }
    }

    /// Identities the build response states and that agree are accepted
    /// whatever their letter case, and the recorded response states only
    /// its amount.
    #[test]
    fn a_build_that_states_the_same_swap_passes_in_any_letter_case() {
        let req = recorded_req();
        let route = fixture("kyber_route.json");
        let recorded = parse_build_identity(&fixture("kyber_build.json")).unwrap();
        assert_eq!(
            recorded,
            BuildIdentity {
                amount_in: U256::from(100_000_000u64),
                token_in: None,
                token_out: None,
                sender: None,
                recipient: None,
            }
        );
        let mut stated = fixture("kyber_build.json");
        stated["data"]["tokenIn"] = serde_json::json!("0xA0B86991C6218B36C1D19D4A2E9EB0CE3606EB48");
        stated["data"]["tokenOut"] =
            serde_json::json!("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
        stated["data"]["sender"] = serde_json::json!(format!("{:?}", req.module));
        stated["data"]["recipient"] = serde_json::json!(req.module.to_string());
        let q = quote_from_responses(&route, &stated, &req).unwrap();
        assert_eq!(q.input_token, req.input_token);
        assert_eq!(q.output_token, req.output_token);
    }

    /// What the build request asks for: the route summary as it was
    /// answered, and the module as both sender and recipient.
    #[test]
    fn the_build_request_names_the_module_as_sender_and_recipient() {
        let req = recorded_req();
        let (summary, _, _) = parse_route_response(&fixture("kyber_route.json")).unwrap();
        let body = Kyber::new("test-client").build_body(&summary, &req);
        let module = format!("{:?}", req.module);
        assert_eq!(body["sender"], serde_json::json!(module));
        assert_eq!(body["recipient"], serde_json::json!(module));
        assert_eq!(body["routeSummary"], summary);
        assert_eq!(body["slippageTolerance"], serde_json::json!(50));
    }

    /// A transport failure's text can quote a URL; a key in it is masked.
    #[test]
    fn an_error_text_masks_an_endpoint_key() {
        let e = err(
            "routes",
            "error sending request for url (https://proxy.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr)",
        );
        assert_eq!(
            e.to_string(),
            "rpc: kyber routes: error sending request for url (https://proxy.example.com/v2/<redacted>)"
        );
    }
}
