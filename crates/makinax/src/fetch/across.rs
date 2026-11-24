//! Across bridge fee quotes: a thin client for the Across API's
//! `suggested-fees` endpoint.
//!
//! Advisory, like every fetched value: it prices a transfer and names the
//! origin SpokePool, but the on-chain `minOutputAmount` floor is what binds a
//! send, and settlement is decided by the destination fill event (see
//! [`crate::bridges::across`]).
//!
//! Parsing is strict. Every amount must be a decimal integer string, every
//! address must parse, and when the response echoes the tokens it quoted
//! (`inputToken` / `outputToken`), they must be the tokens and chains that
//! were asked for — a quote for something else is refused, never reused.

use alloy::primitives::{Address, U256};
use serde_json::Value;

use crate::error::{Error, Result};

/// The Across production API.
pub const ACROSS_API_URL: &str = "https://app.across.to/api";

/// What to price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcrossFeeRequest {
    pub input_token: Address,
    pub output_token: Address,
    pub origin_chain_id: u64,
    pub destination_chain_id: u64,
    /// Raw input-token units.
    pub amount: U256,
}

/// The parsed `suggested-fees` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcrossFeeQuote {
    /// Total relay fee in raw input-token units (`totalRelayFee.total`).
    pub total_relay_fee: U256,
    /// What the relayer is expected to deliver, raw OUTPUT-token units (the
    /// API's `outputAmount`; required — it is not derivable from the fee
    /// when the two tokens have different decimals).
    pub output_amount: U256,
    /// Across refuses to relay an amount this small.
    pub is_amount_too_low: bool,
    /// The quote timestamp (unix seconds) a deposit must carry.
    pub quote_timestamp: u64,
    /// The origin-chain SpokePool the quote is for.
    pub spoke_pool: Address,
    pub exclusive_relayer: Address,
    /// Exclusivity window, as the API states it (seconds offset).
    pub exclusivity_deadline: u64,
    /// Suggested fill deadline (unix seconds), when the API provides one.
    pub fill_deadline: Option<u64>,
    /// Deposit limits, raw input-token units.
    pub min_deposit: U256,
    pub max_deposit: U256,
    /// Estimated fill time, when the API provides one.
    pub estimated_fill_time_s: Option<u64>,
}

fn refused(detail: impl std::fmt::Display) -> Error {
    Error::FetchRefused(format!("across suggested-fees: {detail}"))
}

/// The request URL.
pub(crate) fn suggested_fees_url(base_url: &str, req: &AcrossFeeRequest) -> String {
    format!(
        "{base_url}/suggested-fees?inputToken={:?}&outputToken={:?}&originChainId={}&destinationChainId={}&amount={}",
        req.input_token,
        req.output_token,
        req.origin_chain_id,
        req.destination_chain_id,
        req.amount
    )
}

/// A decimal integer, given either as a JSON string or a JSON number.
fn uint(v: &Value, field: &str) -> Result<U256> {
    match v {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s
            .parse::<U256>()
            .map_err(|e| refused(format!("{field}: {e}"))),
        Value::Number(n) => n
            .as_u64()
            .map(U256::from)
            .ok_or_else(|| refused(format!("{field} is not a non-negative integer: {n}"))),
        Value::Null => Err(refused(format!("missing {field}"))),
        other => Err(refused(format!(
            "{field} is not a decimal integer: {other}"
        ))),
    }
}

fn u64_field(v: &Value, field: &str) -> Result<u64> {
    let n = uint(v, field)?;
    u64::try_from(n).map_err(|_| refused(format!("{field} does not fit in u64: {n}")))
}

fn address(v: &Value, field: &str) -> Result<Address> {
    v.as_str()
        .ok_or_else(|| refused(format!("missing {field}")))?
        .parse()
        .map_err(|e| refused(format!("{field}: {e}")))
}

/// When the response names the token it quoted, it must be the one asked for.
fn check_echo(v: &Value, field: &str, token: Address, chain_id: u64) -> Result<()> {
    let Some(echo) = v.get(field) else {
        return Ok(());
    };
    let quoted = address(&echo["address"], &format!("{field}.address"))?;
    if quoted != token {
        return Err(refused(format!(
            "{field} is {quoted:?}, but {token:?} was requested"
        )));
    }
    if !echo["chainId"].is_null() {
        let quoted_chain = u64_field(&echo["chainId"], &format!("{field}.chainId"))?;
        if quoted_chain != chain_id {
            return Err(refused(format!(
                "{field}.chainId is {quoted_chain}, but chain {chain_id} was requested"
            )));
        }
    }
    Ok(())
}

/// Parse a `suggested-fees` response for `req`. Pure: tested against a
/// recorded response, never the network.
pub fn parse_suggested_fees(v: &Value, req: &AcrossFeeRequest) -> Result<AcrossFeeQuote> {
    if !v.is_object() {
        return Err(refused("response is not a JSON object"));
    }
    if let Some(message) = v.get("message").and_then(Value::as_str)
        && v.get("totalRelayFee").is_none()
    {
        // The API's error shape: {"type": "...", "code": "...", "message": "..."}.
        return Err(refused(message));
    }
    check_echo(v, "inputToken", req.input_token, req.origin_chain_id)?;
    check_echo(v, "outputToken", req.output_token, req.destination_chain_id)?;

    let total_relay_fee = uint(&v["totalRelayFee"]["total"], "totalRelayFee.total")?;
    // Reported as the API states it, in OUTPUT-token units. No bound is
    // checked against the input amount: the two tokens may have different
    // decimals, and the on-chain `minOutputAmount` is what binds a send.
    let output_amount = uint(&v["outputAmount"], "outputAmount")?;
    let is_amount_too_low = v["isAmountTooLow"]
        .as_bool()
        .ok_or_else(|| refused("missing isAmountTooLow"))?;
    let optional_u64 = |field: &str| -> Result<Option<u64>> {
        match v.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(x) => u64_field(x, field).map(Some),
        }
    };
    Ok(AcrossFeeQuote {
        total_relay_fee,
        output_amount,
        is_amount_too_low,
        quote_timestamp: u64_field(&v["timestamp"], "timestamp")?,
        spoke_pool: address(&v["spokePoolAddress"], "spokePoolAddress")?,
        exclusive_relayer: address(&v["exclusiveRelayer"], "exclusiveRelayer")?,
        exclusivity_deadline: u64_field(&v["exclusivityDeadline"], "exclusivityDeadline")?,
        fill_deadline: optional_u64("fillDeadline")?,
        min_deposit: uint(&v["limits"]["minDeposit"], "limits.minDeposit")?,
        max_deposit: uint(&v["limits"]["maxDeposit"], "limits.maxDeposit")?,
        estimated_fill_time_s: optional_u64("estimatedFillTimeSec")?,
    })
}

/// Ask the Across API to price one transfer.
pub async fn fetch_suggested_fees(
    base_url: &str,
    req: &AcrossFeeRequest,
    client_name: &str,
) -> Result<AcrossFeeQuote> {
    let resp = super::http_client(client_name)
        .get(suggested_fees_url(base_url, req))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| Error::rpc(format!("across suggested-fees: {e}")))?;
    let status = resp.status().as_u16();
    let v: Value = resp
        .json()
        .await
        .map_err(|e| Error::rpc(format!("across suggested-fees json (HTTP {status}): {e}")))?;
    parse_suggested_fees(&v, req)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    const USDC_MAINNET: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

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
    fn the_url_carries_every_request_field() {
        let u = suggested_fees_url(ACROSS_API_URL, &req());
        assert!(
            u.starts_with("https://app.across.to/api/suggested-fees?"),
            "{u}"
        );
        for needle in [
            "inputToken=0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
            "outputToken=0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            "originChainId=8453",
            "destinationChainId=1",
            "amount=100000000",
        ] {
            assert!(
                u.to_lowercase().contains(&needle.to_lowercase()),
                "{needle}: {u}"
            );
        }
    }

    #[test]
    fn schema_drift_is_an_error_not_a_panic() {
        for bad in [
            serde_json::json!(null),
            serde_json::json!([1, 2]),
            serde_json::json!({}),
            serde_json::json!({"type": "AcrossApiError", "code": "AMOUNT_TOO_LOW", "message": "Sent amount is too low"}),
            serde_json::json!({"totalRelayFee": {"total": "-1"}}),
            serde_json::json!({"totalRelayFee": {"total": "1.5"}}),
        ] {
            assert!(parse_suggested_fees(&bad, &req()).is_err(), "{bad}");
        }
    }
}
