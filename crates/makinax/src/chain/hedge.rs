//! What a hedged read may accept from a fallback endpoint.
//!
//! The provider hedges every read across all configured endpoints and takes
//! the first answer that comes back `Ok` at the transport layer. Two answers
//! a public fallback gives faster than the primary are not answers at all:
//!
//! - a JSON-RPC ERROR at HTTP 200 saying the endpoint cannot serve the
//!   request ("archive request", "pruned", "missing trie node", a method it
//!   does not have, its own rate limit). The transport counted that as a
//!   success and the read failed with the fallback's words while the primary
//!   had the real answer;
//! - `null` for `eth_getTransactionReceipt` from an endpoint that simply does
//!   not have the transaction yet (or prunes it), which reads as "not mined"
//!   and sent the credential preparation into a rebroadcast loop that ended in
//!   "Preparation unknown" while both transactions had mined.
//!
//! [`Answerable`] turns the first class into a transport failure so the hedge
//! moves on. [`ReceiptHedge`] asks every endpoint for a receipt and prefers a
//! receipt over `null`, and the primary's `null` over a fallback's. Nothing
//! here changes what a contract revert looks like: that is a valid answer
//! every endpoint agrees on, and it is surfaced, never masked.
use alloy::rpc::json_rpc::{RequestPacket, ResponsePacket, ResponsePayload};
use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
use std::task::{Context, Poll};
use tower::Service;

/// JSON-RPC error responses that mean "this endpoint cannot answer", not "the
/// answer is no". Read off the code and the message the endpoint itself sent.
pub(crate) fn cannot_answer(code: i64, message: &str) -> bool {
    // A method the endpoint does not serve: another endpoint may.
    if code == -32601 {
        return true;
    }
    // Our own malformed request or a parse failure: every endpoint would
    // refuse it the same way, so this is the caller's problem, not routing.
    if code == -32600 || code == -32602 || code == -32700 {
        return false;
    }
    // Infura-style "limit exceeded" (-32005) is the endpoint's own limit.
    if code == -32005 {
        return true;
    }
    // Only the archive / pruned / rate-limit phrasing, never a
    // broad word a revert reason could carry ("allowance exceeded").
    let m = message.to_ascii_lowercase();
    [
        "archive",
        "pruned",
        "missing trie node",
        "historical state",
        "unsupported block",
        "block range",
        "state not available",
        "rate limit",
        "too many requests",
        // Acceptance's captured phrasings; specific enough not to match a revert
        // reason, and reverts never reach this test (see `error_is_the_answer`).
        "over capacity",
        "beyond the last",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// Methods whose error responses carry the CHAIN's answer (a revert reason,
/// a nonce or funds refusal): their text is never read for routing, so a
/// revert that happens to mention a limit reaches the caller as a revert.
fn error_is_the_answer(method: &str) -> bool {
    matches!(
        method,
        "eth_call" | "eth_estimateGas" | "eth_sendRawTransaction" | "eth_sendTransaction"
    )
}

/// The typed transport failure for an endpoint that cannot answer: which
/// endpoints refused (by role and host) and what they said. Callers that
/// need to name the endpoint (the credential screen's "your fallback RPC
/// cannot read receipts") downcast to this instead of parsing the message.
#[derive(Debug, Clone)]
pub struct CannotAnswer {
    pub method: String,
    /// `(role, host, the endpoint's own words)`; role is "primary" or
    /// "fallback" once the hedge has placed the endpoint, "" before.
    pub refusals: Vec<(String, String, String)>,
}

impl CannotAnswer {
    /// The roles that refused, deduplicated, in endpoint order: "primary",
    /// "fallback", or "primary and fallback".
    pub fn roles(&self) -> String {
        let mut roles: Vec<&str> = Vec::new();
        for (role, _, _) in &self.refusals {
            if !role.is_empty() && !roles.contains(&role.as_str()) {
                roles.push(role);
            }
        }
        roles.join(" and ")
    }
}

impl std::fmt::Display for CannotAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cannot answer {}: ", self.method)?;
        for (i, (role, host, why)) in self.refusals.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            if role.is_empty() {
                write!(f, "{host}: {why}")?;
            } else {
                write!(f, "{role} {host}: {why}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for CannotAnswer {}

/// The typed refusal behind a transport error, when there is one.
pub fn cannot_answer_error(error: &TransportError) -> Option<&CannotAnswer> {
    match error {
        alloy::transports::RpcError::Transport(TransportErrorKind::Custom(inner)) => {
            inner.downcast_ref::<CannotAnswer>()
        }
        _ => None,
    }
}

fn first_cannot_answer(packet: &ResponsePacket) -> Option<(i64, String)> {
    packet
        .iter_errors()
        .find(|e| cannot_answer(e.code, &e.message))
        .map(|e| (e.code, e.message.to_string()))
}

/// One transport whose "I cannot answer" errors become transport failures.
#[derive(Clone, Debug)]
pub(crate) struct Answerable<S> {
    inner: S,
    label: String,
}

impl<S> Answerable<S> {
    pub fn new(inner: S, label: impl Into<String>) -> Self {
        Self {
            inner,
            label: label.into(),
        }
    }
}

impl<S> Service<RequestPacket> for Answerable<S>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let method = match &req {
            RequestPacket::Single(r) => r.method().to_string(),
            RequestPacket::Batch(b) => b
                .first()
                .map(|r| r.method().to_string())
                .unwrap_or_default(),
        };
        let label = self.label.clone();
        let fut = self.inner.call(req);
        Box::pin(async move {
            let packet = fut.await?;
            if error_is_the_answer(&method) {
                return Ok(packet);
            }
            if let Some((code, message)) = first_cannot_answer(&packet) {
                return Err(TransportErrorKind::custom(CannotAnswer {
                    method,
                    refusals: vec![(
                        String::new(),
                        label,
                        crate::util::redact::text(&format!("JSON-RPC {code}: {message}")),
                    )],
                }));
            }
            Ok(packet)
        })
    }
}

/// Methods whose `null` answer from a fallback must not beat the primary's.
fn prefers_presence(method: &str) -> bool {
    matches!(
        method,
        "eth_getTransactionReceipt" | "eth_getTransactionByHash"
    )
}

/// The fields that say WHICH thing was read. Both answers must carry them, or
/// "they agree about everything they both mentioned" is a statement about
/// nothing.
fn identity_keys(method: &str) -> &'static [&'static str] {
    match method {
        "eth_getTransactionReceipt" => &["transactionHash", "blockHash", "blockNumber", "status"],
        "eth_getTransactionByHash" => &["hash", "blockHash", "blockNumber"],
        _ => &[],
    }
}

/// A KEY ONE SIDE OMITS IS SILENCE, NOT DISAGREEMENT. A key both sides carry
/// with different values is disagreement, and still refuses.
fn values_agree(x: &serde_json::Value, y: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (x, y) {
        (Value::Object(xo), Value::Object(yo)) => xo
            .iter()
            .all(|(k, v)| yo.get(k).is_none_or(|w| values_agree(v, w))),
        (Value::Array(xa), Value::Array(ya)) => {
            xa.len() == ya.len() && xa.iter().zip(ya).all(|(v, w)| values_agree(v, w))
        }
        _ => x == y,
    }
}

/// The agreement decision, split from the transport so it is testable without
/// a chain or a server.
fn answers_agree(method: &str, x: &serde_json::Value, y: &serde_json::Value) -> bool {
    let keys = identity_keys(method);
    // NOT `all()` alone: over an empty list that is vacuously true, which would
    // hand every other method the leniency below on the strength of having no
    // identity to check. A method with no identity keys is compared strictly.
    let identified = !keys.is_empty()
        && keys.iter().all(|k| {
            x.get(k).is_some_and(|v| !v.is_null()) && y.get(k).is_some_and(|v| !v.is_null())
        });
    if identified {
        values_agree(x, y)
    } else {
        // Without the identity there is nothing to be lenient about: two
        // objects that do not both say which transaction they describe are
        // compared strictly, as before.
        x == y
    }
}

/// Two success payloads are the same answer when everything BOTH of them
/// claims agrees, and both claim the identity of the thing being read.
///
/// This was whole-value equality, and that is the defect that reported an
/// ERC20 approval as failed after it had been mined. Providers differ in
/// which OPTIONAL fields they return — one public node includes a transaction's
/// `blockTimestamp` and a gateway omits it, twenty-one fields against twenty —
/// so demanding every field match read one provider's silence as the two of
/// them disagreeing about the user's transaction.
///
/// Fixing it at that field name would fix today and leave the mechanism
/// standing, because the trigger is "an optional field one provider includes",
/// not any dialect anyone chose, and providers keep adding fields.
fn same_answer(method: &str, a: &ResponsePacket, b: &ResponsePacket) -> bool {
    match (a.single_payload(), b.single_payload()) {
        (Some(ResponsePayload::Success(x)), Some(ResponsePayload::Success(y))) => {
            let parse = |v: &str| serde_json::from_str::<serde_json::Value>(v).ok();
            match (parse(x.get()), parse(y.get())) {
                (Some(x), Some(y)) => answers_agree(method, &x, &y),
                _ => x.get() == y.get(),
            }
        }
        _ => false,
    }
}

/// The fields a receipt conflict is described by: block hash, block number and status.
fn answer_summary(packet: &ResponsePacket) -> String {
    let Some(ResponsePayload::Success(v)) = packet.single_payload() else {
        return "no answer".into();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(v.get()) else {
        return "unparsable answer".into();
    };
    let field = |k: &str| {
        value
            .get(k)
            .and_then(|f| f.as_str())
            .unwrap_or("?")
            .to_string()
    };
    format!(
        "block {} (#{}) status {}",
        field("blockHash"),
        field("blockNumber"),
        field("status")
    )
}

fn is_null_success(packet: &ResponsePacket) -> bool {
    matches!(
        packet.single_payload(),
        Some(ResponsePayload::Success(v)) if v.get().trim() == "null"
    )
}

/// The provider's transport for two or more endpoints: receipt-class reads
/// ask every endpoint and prefer a present answer; everything else goes to
/// the hedging `fallback` service built over the same endpoints.
#[derive(Clone, Debug)]
pub(crate) struct ReceiptHedge<S, F> {
    /// Primary first, then the fallbacks, in configuration order.
    endpoints: Vec<S>,
    fallback: F,
}

impl<S, F> ReceiptHedge<S, F> {
    pub fn new(endpoints: Vec<S>, fallback: F) -> Self {
        Self {
            endpoints,
            fallback,
        }
    }
}

impl<S, F> Service<RequestPacket> for ReceiptHedge<S, F>
where
    S: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    F: Service<RequestPacket, Response = ResponsePacket, Error = TransportError>
        + Clone
        + Send
        + 'static,
    F::Future: Send + 'static,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.fallback.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        let receipt_class = match &req {
            RequestPacket::Single(r) => prefers_presence(r.method()),
            RequestPacket::Batch(_) => false,
        };
        if !receipt_class || self.endpoints.len() < 2 {
            return Box::pin(self.fallback.call(req));
        }
        let method = match &req {
            RequestPacket::Single(r) => r.method().to_string(),
            RequestPacket::Batch(_) => String::new(),
        };
        let futures: Vec<_> = self
            .endpoints
            .iter()
            .cloned()
            .map(|mut endpoint| {
                let req = req.clone();
                async move { endpoint.call(req).await }
            })
            .collect();
        Box::pin(async move {
            let answers = futures::future::join_all(futures).await;
            // A present answer from any endpoint wins, provided every present
            // answer agrees: two endpoints that return receipts
            // for the same hash with a different block or status are a
            // conflict the caller must see, never a pick in endpoint order.
            let present: Vec<&ResponsePacket> = answers
                .iter()
                .filter_map(|a| a.as_ref().ok().filter(|p| !is_null_success(p)))
                .collect();
            if let Some(first) = present.first() {
                if let Some(other) = present
                    .iter()
                    .skip(1)
                    .find(|p| !same_answer(&method, first, p))
                {
                    return Err(TransportErrorKind::custom_str(&format!(
                        "endpoints disagree on {method}: {} versus {}; refusing to pick one",
                        answer_summary(first),
                        answer_summary(other)
                    )));
                }
                return Ok((*first).clone());
            }
            // Otherwise the primary's own null, then any null, then every
            // refusal, named by role, so the caller can say which endpoint
            // could not read the receipt.
            let mut refusals = Vec::new();
            let mut last_error = None;
            for (index, answer) in answers.into_iter().enumerate() {
                let role = if index == 0 { "primary" } else { "fallback" };
                match answer {
                    Ok(packet) => return Ok(packet),
                    Err(e) => {
                        match cannot_answer_error(&e) {
                            Some(typed) => {
                                for (_, host, why) in &typed.refusals {
                                    refusals.push((role.to_string(), host.clone(), why.clone()));
                                }
                            }
                            None => refusals.push((
                                role.to_string(),
                                String::from("endpoint"),
                                crate::util::redact::text(&e.to_string()),
                            )),
                        }
                        last_error = Some(e);
                    }
                }
            }
            if refusals.is_empty() {
                return Err(last_error.unwrap_or_else(|| {
                    TransportErrorKind::custom_str("no endpoint answered the receipt read")
                }));
            }
            Err(TransportErrorKind::custom(CannotAnswer {
                method,
                refusals,
            }))
        })
    }
}

#[cfg(test)]
mod agreement {
    use super::*;
    use serde_json::json;

    /// THE DEFECT, from the build that shipped it: a token approval was
    /// mined and set the allowance, and a caller reported the transaction as
    /// unavailable, because the two endpoints a new user is given by default
    /// answered with twenty-one fields and twenty. The field is not the point
    /// and is not named here: the mechanism is an optional field one provider
    /// includes, and providers keep adding them.
    #[test]
    fn one_providers_extra_field_is_silence_and_a_changed_field_is_disagreement() {
        let hash = "0x11";
        let lean = json!({"hash":hash,"blockHash":"0xbb","blockNumber":"0x2a","from":"0xa","to":"0xb","value":"0x1"});
        let mut rich = lean.clone();
        rich["blockTimestamp"] = json!("0x68cb");
        assert!(
            answers_agree("eth_getTransactionByHash", &lean, &rich),
            "an optional field one endpoint includes is not a conflict"
        );
        assert!(
            answers_agree("eth_getTransactionByHash", &rich, &lean),
            "and it is symmetric"
        );

        // A field BOTH carry with different values is a real conflict and must
        // still refuse: that is the case this leniency must not swallow.
        let mut moved = lean.clone();
        moved["blockNumber"] = json!("0x2b");
        assert!(!answers_agree("eth_getTransactionByHash", &lean, &moved));
        let mut elsewhere = lean.clone();
        elsewhere["to"] = json!("0xc");
        assert!(!answers_agree(
            "eth_getTransactionByHash",
            &lean,
            &elsewhere
        ));

        // Receipts: a differing status is the one that decides whether money
        // moved, and it is compared.
        let ok = json!({"transactionHash":hash,"blockHash":"0xbb","blockNumber":"0x2a","status":"0x1","logs":[]});
        let mut reverted = ok.clone();
        reverted["status"] = json!("0x0");
        assert!(!answers_agree("eth_getTransactionReceipt", &ok, &reverted));
        let mut with_extra = ok.clone();
        with_extra["effectiveGasPrice"] = json!("0x7");
        assert!(answers_agree("eth_getTransactionReceipt", &ok, &with_extra));

        // Nested: an optional field inside a log entry behaves the same way,
        // and a changed topic still disagrees.
        let logged = json!({"transactionHash":hash,"blockHash":"0xbb","blockNumber":"0x2a","status":"0x1",
            "logs":[{"address":"0xa","topics":["0xt"]}]});
        let mut logged_rich = logged.clone();
        logged_rich["logs"][0]["removed"] = json!(false);
        assert!(answers_agree(
            "eth_getTransactionReceipt",
            &logged,
            &logged_rich
        ));
        let mut logged_other = logged.clone();
        logged_other["logs"][0]["topics"] = json!(["0xu"]);
        assert!(!answers_agree(
            "eth_getTransactionReceipt",
            &logged,
            &logged_other
        ));

        // WITHOUT the identity there is no leniency: an answer that does not
        // say which transaction it describes is compared strictly.
        let partial = json!({"from":"0xa"});
        let mut partial_rich = partial.clone();
        partial_rich["blockTimestamp"] = json!("0x1");
        assert!(!answers_agree(
            "eth_getTransactionByHash",
            &partial,
            &partial_rich
        ));

        // A method with no identity keys keeps the old strict behaviour.
        assert!(!answers_agree("eth_getBalance", &lean, &rich));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::{Provider, ProviderBuilder};
    use alloy::rpc::client::RpcClient;
    use alloy::transports::http::{ReqwestTransport, reqwest::Url};
    use axum::{Json, Router, body::Bytes, extract::State, routing::any};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone)]
    struct Endpoint {
        /// The answer for eth_getTransactionReceipt: a JSON-RPC result or an error.
        receipt: serde_json::Value,
        error: Option<(i64, &'static str)>,
        delay: Duration,
        http_status: u16,
        seen: Arc<Mutex<Vec<String>>>,
    }

    async fn handler(
        State(e): State<Endpoint>,
        body: Bytes,
    ) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let method = req["method"].as_str().unwrap_or("").to_string();
        e.seen.lock().unwrap().push(method.clone());
        tokio::time::sleep(e.delay).await;
        if e.http_status != 200 {
            return (
                axum::http::StatusCode::from_u16(e.http_status).unwrap(),
                Json(serde_json::json!({"error": "down"})),
            );
        }
        let id = req["id"].clone();
        let body = match (method.as_str(), &e.error) {
            ("eth_getTransactionReceipt", Some((code, message))) => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
            }
            ("eth_getTransactionReceipt", None) => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":e.receipt})
            }
            ("eth_chainId", _) => serde_json::json!({"jsonrpc":"2.0","id":id,"result":"0x2105"}),
            ("eth_blockNumber", _) => {
                serde_json::json!({"jsonrpc":"2.0","id":id,"result":"0x310d360"})
            }
            _ => serde_json::json!({"jsonrpc":"2.0","id":id,"result":"0x1"}),
        };
        (axum::http::StatusCode::OK, Json(body))
    }

    async fn serve(e: Endpoint) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, Router::new().fallback(any(handler)).with_state(e))
                .await
                .unwrap();
        });
        (origin, task)
    }

    fn receipt_json() -> serde_json::Value {
        serde_json::json!({
            "transactionHash": format!("0x{}", "ab".repeat(32)),
            "transactionIndex": "0x0", "blockHash": format!("0x{}", "cd".repeat(32)),
            "blockNumber": "0x310d360", "from": format!("0x{}", "11".repeat(20)), "to": format!("0x{}", "22".repeat(20)),
            "cumulativeGasUsed": "0x5208", "gasUsed": "0x5208", "contractAddress": null, "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)), "status": "0x1", "effectiveGasPrice": "0x1", "type": "0x2"
        })
    }

    fn endpoint(
        receipt: serde_json::Value,
        error: Option<(i64, &'static str)>,
        delay_ms: u64,
        http_status: u16,
    ) -> Endpoint {
        Endpoint {
            receipt,
            error,
            delay: Duration::from_millis(delay_ms),
            http_status,
            seen: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn provider(urls: &[String]) -> alloy::providers::DynProvider {
        let transports: Vec<_> = urls
            .iter()
            .map(|u| {
                Answerable::new(
                    ReqwestTransport::with_client(
                        alloy::transports::http::reqwest::Client::new(),
                        u.parse::<Url>().unwrap(),
                    ),
                    u.clone(),
                )
            })
            .collect();
        let fallback =
            alloy::transports::layers::FallbackService::new(transports.clone(), transports.len());
        let service = ReceiptHedge::new(transports, fallback);
        ProviderBuilder::new()
            .connect_client(RpcClient::new(service, false))
            .erased()
    }

    const HASH: alloy::primitives::B256 = alloy::primitives::B256::repeat_byte(0xab);

    /// The observed case: the fallback rejects the receipt read as an archive
    /// request, faster than the primary answers. The read returns the primary's
    /// receipt, never the fallback's error.
    #[tokio::test]
    async fn a_fast_archive_rejection_never_beats_the_primary_receipt() {
        let primary = endpoint(receipt_json(), None, 150, 200);
        let fallback = endpoint(
            serde_json::Value::Null,
            Some((
                -32000,
                "archive request; this endpoint serves only recent state",
            )),
            0,
            200,
        );
        let (p, pt) = serve(primary.clone()).await;
        let (f, ft) = serve(fallback.clone()).await;
        let receipt = provider(&[p, f])
            .get_transaction_receipt(HASH)
            .await
            .unwrap();
        pt.abort();
        ft.abort();
        assert_eq!(receipt.map(|r| r.status()), Some(true));
        assert!(
            fallback
                .seen
                .lock()
                .unwrap()
                .contains(&"eth_getTransactionReceipt".to_string()),
            "the fallback was asked and lost"
        );
    }

    /// A fallback that does not have the transaction answers `null` first.
    /// The primary's receipt still wins; a rebroadcast loop never starts.
    #[tokio::test]
    async fn a_fast_null_from_a_fallback_never_beats_the_primary_receipt() {
        let (p, pt) = serve(endpoint(receipt_json(), None, 150, 200)).await;
        let (f, ft) = serve(endpoint(serde_json::Value::Null, None, 0, 200)).await;
        let receipt = provider(&[p, f])
            .get_transaction_receipt(HASH)
            .await
            .unwrap();
        pt.abort();
        ft.abort();
        assert!(receipt.is_some());
    }

    /// Not mined anywhere: `null` is the honest answer. The primary down and
    /// the fallback saying `null`: still `null`, not an error, so a pending
    /// transaction keeps being polled instead of reported unreadable.
    #[tokio::test]
    async fn null_everywhere_is_null_and_a_down_primary_yields_the_fallback_null() {
        let (p, pt) = serve(endpoint(serde_json::Value::Null, None, 0, 200)).await;
        let (f, ft) = serve(endpoint(serde_json::Value::Null, None, 0, 200)).await;
        assert!(
            provider(&[p, f])
                .get_transaction_receipt(HASH)
                .await
                .unwrap()
                .is_none()
        );
        pt.abort();
        ft.abort();
        let (down, dt) = serve(endpoint(serde_json::Value::Null, None, 0, 503)).await;
        let (f2, f2t) = serve(endpoint(serde_json::Value::Null, None, 0, 200)).await;
        assert!(
            provider(&[down, f2])
                .get_transaction_receipt(HASH)
                .await
                .unwrap()
                .is_none()
        );
        dt.abort();
        f2t.abort();
    }

    /// Every endpoint refuses: the read fails as a transport error carrying
    /// the endpoint's words, so the caller reports "unreadable", not "absent".
    #[tokio::test]
    async fn a_receipt_no_endpoint_can_serve_is_an_error_not_an_absence() {
        let (p, pt) = serve(endpoint(
            serde_json::Value::Null,
            Some((-32000, "missing trie node")),
            0,
            200,
        ))
        .await;
        let (f, ft) = serve(endpoint(
            serde_json::Value::Null,
            Some((-32000, "archive request")),
            0,
            200,
        ))
        .await;
        let err = provider(&[p, f])
            .get_transaction_receipt(HASH)
            .await
            .unwrap_err();
        pt.abort();
        ft.abort();
        assert!(
            err.to_string()
                .contains("cannot answer eth_getTransactionReceipt"),
            "{err}"
        );
        let typed = cannot_answer_error(&err).expect("typed refusal");
        assert_eq!(typed.roles(), "primary and fallback");
        assert!(err.to_string().contains("missing trie node"), "{err}");
        assert!(err.to_string().contains("archive request"), "{err}");
    }

    /// Other reads keep the hedge: a fallback's "cannot answer" error is a
    /// transport failure, so the primary's block number is returned; and an
    /// answer every endpoint gives the same way (here a benign stub) is
    /// returned as is.
    #[tokio::test]
    async fn other_reads_skip_an_endpoint_that_cannot_answer() {
        let (p, pt) = serve(endpoint(receipt_json(), None, 100, 200)).await;
        let (f, ft) = serve(endpoint(
            serde_json::Value::Null,
            Some((-32005, "rate limit exceeded")),
            0,
            200,
        ))
        .await;
        let prov = provider(&[p, f]);
        assert_eq!(prov.get_block_number().await.unwrap(), 0x310d360);
        assert_eq!(prov.get_chain_id().await.unwrap(), 8453);
        pt.abort();
        ft.abort();
    }

    /// A revert reason that mentions a limit is the chain's
    /// answer, not a routing signal; eth_call returns it as a revert from the
    /// endpoint that answered first, never as "cannot answer".
    #[tokio::test]
    async fn a_revert_reason_mentioning_a_limit_reaches_the_caller_as_a_revert() {
        #[derive(Clone)]
        struct Reverting;
        async fn revert(body: Bytes) -> Json<serde_json::Value> {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let id = req["id"].clone();
            Json(match req["method"].as_str().unwrap_or("") {
                "eth_call" | "eth_estimateGas" => {
                    serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"execution reverted: allowance exceeded"}})
                }
                "eth_chainId" => serde_json::json!({"jsonrpc":"2.0","id":id,"result":"0x2105"}),
                _ => serde_json::json!({"jsonrpc":"2.0","id":id,"result":"0x1"}),
            })
        }
        let mut origins = Vec::new();
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            origins.push(format!("http://{}", listener.local_addr().unwrap()));
            tasks.push(tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new().fallback(any(revert)).with_state(Reverting),
                )
                .await
                .unwrap();
            }));
        }
        let prov = provider(&origins);
        let call = alloy::rpc::types::TransactionRequest::default()
            .to(alloy::primitives::Address::repeat_byte(0x22))
            .input(alloy::rpc::types::TransactionInput::new(
                vec![0u8; 4].into(),
            ));
        let err = prov.call(call).await.unwrap_err();
        for t in tasks {
            t.abort();
        }
        let text = err.to_string();
        assert!(text.contains("allowance exceeded"), "{text}");
        assert!(!text.contains("cannot answer"), "{text}");
    }

    /// When no endpoint can be reached, the receipt read names each
    /// endpoint's failure. A connection error quotes the endpoint URL; the
    /// key in it is masked, in the typed refusal and in the rendered error.
    #[tokio::test]
    async fn unreachable_keyed_endpoints_are_named_without_their_keys() {
        const KEY: &str = "9fKd2Lm4Xp7Qw1Nz8Bt3Yr";
        let dead = format!("http://127.0.0.1:1/v2/{KEY}");
        let err = provider(&[dead.clone(), dead])
            .get_transaction_receipt(HASH)
            .await
            .unwrap_err();
        let typed = cannot_answer_error(&err).expect("every endpoint refused");
        assert_eq!(typed.refusals.len(), 2);
        for (_, _, why) in &typed.refusals {
            assert!(!why.contains(KEY), "{why}");
            assert!(why.contains("127.0.0.1:1/v2/<redacted>"), "{why}");
        }
        let text = err.to_string();
        assert!(!text.contains(KEY), "{text}");
    }

    /// Two endpoints that both hold a receipt for the hash but
    /// disagree on its block are a conflict; the read fails instead of
    /// picking the first in configuration order.
    #[tokio::test]
    async fn receipts_that_disagree_across_endpoints_are_a_conflict_not_a_pick() {
        let mut other = receipt_json();
        other["blockHash"] = serde_json::json!(format!("0x{}", "ef".repeat(32)));
        let (p, pt) = serve(endpoint(receipt_json(), None, 0, 200)).await;
        let (f, ft) = serve(endpoint(other, None, 0, 200)).await;
        let err = provider(&[p, f])
            .get_transaction_receipt(HASH)
            .await
            .unwrap_err();
        pt.abort();
        ft.abort();
        let text = err.to_string();
        assert!(text.contains("endpoints disagree"), "{text}");
        assert!(text.contains("refusing to pick one"), "{text}");
        // Agreeing receipts (same JSON) are still one answer.
        let (p2, p2t) = serve(endpoint(receipt_json(), None, 0, 200)).await;
        let (f2, f2t) = serve(endpoint(receipt_json(), None, 30, 200)).await;
        assert!(
            provider(&[p2, f2])
                .get_transaction_receipt(HASH)
                .await
                .unwrap()
                .is_some()
        );
        p2t.abort();
        f2t.abort();
    }

    #[test]
    fn the_cannot_answer_class_is_read_off_the_endpoints_own_words() {
        for (code, message) in [
            (
                -32000,
                "archive request; this endpoint serves only recent state",
            ),
            (-32000, "missing trie node 0xabc (path ) <nil>"),
            (-32000, "Unsupported block number"),
            (-32000, "historical state is not available for this block"),
            (-32005, "Rate limit exceeded"),
            (-32000, "rate limit reached"),
            (
                -32601,
                "the method eth_getTransactionReceipt does not exist/is not available",
            ),
            (-32000, "block range too large; exceeds the limit"),
        ] {
            assert!(cannot_answer(code, message), "{code} {message}");
        }
        for (code, message) in [
            (3, "execution reverted: Ownable: caller is not the owner"),
            (-32000, "execution reverted"),
            (-32000, "execution reverted: allowance exceeded"),
            (-32000, "gas required exceeds allowance"),
            (-32000, "insufficient capacity in the pool"),
            (-32602, "invalid argument 0: hex string has length 40"),
            (-32000, "nonce too low"),
            (-32000, "already known"),
            (-32000, "insufficient funds for gas * price + value"),
            (-32700, "parse error"),
        ] {
            assert!(!cannot_answer(code, message), "{code} {message}");
        }
    }
}
#[cfg(test)]
mod acceptance_e2_cannot_answer {
    //! Acceptance adversarial. cannot_answer must classify only an
    //! endpoint's "I cannot serve this" as a routing failure, and never a
    //! contract revert (a valid answer every endpoint agrees on). The needle
    //! list is broad, so it can over-match a revert whose message happens to
    //! contain "exceeded"/"beyond"/"capacity"/"unavailable" — a masked revert.
    use super::cannot_answer;

    #[test]
    fn endpoint_capability_refusals_route_on() {
        for m in [
            "this is an archive request",
            "missing trie node 0xabc",
            "the block has been pruned",
            "historical state not available",
            "rate limit exceeded",
            "429 too many requests",
            "over capacity, retry later",
            "requested block is beyond the last available",
        ] {
            assert!(cannot_answer(-32000, m), "should route on: {m}");
        }
        assert!(cannot_answer(
            -32601,
            "the method eth_getTransactionReceipt does not exist"
        ));
    }

    #[test]
    fn our_own_malformed_request_is_not_a_routing_failure() {
        for code in [-32600, -32602, -32700] {
            assert!(
                !cannot_answer(code, "invalid params"),
                "code {code} is the caller's problem"
            );
        }
    }

    #[test]
    fn a_plain_revert_is_surfaced_not_routed() {
        // No capability word: correctly a real answer today.
        assert!(!cannot_answer(3, "execution reverted"));
        assert!(!cannot_answer(
            3,
            "execution reverted: ERC20: transfer amount exceeds balance"
        ));
        assert!(!cannot_answer(-32000, "out of gas"));
    }

    // FINDING (the needle list above): a revert whose
    // message contains a needle word is currently classified as a routing
    // failure and replayed, masking the revert. Fix target: an execution revert
    // reaches the caller unchanged, no matter which words it contains.
    #[test]

    fn a_revert_containing_a_needle_word_must_not_route() {
        assert!(!cannot_answer(3, "execution reverted: allowance exceeded"));
        assert!(!cannot_answer(3, "execution reverted: value beyond cap"));
        assert!(!cannot_answer(3, "execution reverted: pool at capacity"));
        assert!(!cannot_answer(3, "execution reverted: feature unavailable"));
    }
}
