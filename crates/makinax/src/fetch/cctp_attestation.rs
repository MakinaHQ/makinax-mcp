//! Circle's attestation service ("Iris"), as an ADVISORY input.
//!
//! BINDING TRUTH, restated because this is the module most tempted to break it:
//! Iris never decides anything. It supplies the `(message, attestation)` bytes a
//! claim needs, and nothing else it says gates a valuation, a settlement, or an
//! action. Settlement is decided by the destination-chain `MintAndWithdraw`
//! event; "already claimed" is decided by `usedNonces` on the destination
//! transmitter. Both are chain facts. If Iris is down, late, or wrong, the worst
//! outcome is that a claim is not submitted YET — never that a burn is recorded
//! settled, and never that an already-minted transfer is claimed twice.
//!
//! ONE SHOT PER CALL, deliberately. makina-rs's reference client blocks in a
//! 20-minute polling loop with 5-second sleeps, which is right for a CLI driving
//! one transfer. A caller that tracks pending transfers already has a loop, so
//! this client answers once and returns. Blocking here would hold a poll cycle
//! hostage to Circle's latency and stall every other pending operation behind
//! it. Same constants, different control flow — the constants are Circle's, the
//! control flow is the caller's.

use std::time::Duration;

use alloy::primitives::{B256, Bytes};
use serde::Deserialize;

/// Circle's production attestation API.
pub const IRIS_BASE_URL: &str = "https://iris-api.circle.com";

/// Per-request HTTP timeout. A poll cycle must not stall on a hung connection.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How long Standard-finality attestation actually takes, MEASURED.
///
/// **1,378 seconds (~23 minutes), Base → mainnet, Standard finality** — the first
/// production burn.
/// One observation, on one lane, and named as such: it replaces an assumption
/// (makina-rs's 20-minute poll ceiling, which was a give-up deadline rather than an
/// expectation) with a number that came off the wire.
///
/// Used ONLY to derive the escalation threshold. It never decides whether a
/// transfer settled — that is the destination mint, always.
pub const EXPECTED_ATTESTATION_S: i64 = 1_378;

/// What Iris says about one burn transaction. Nothing here is a settlement
/// verdict; `Complete` only means the bytes needed to submit a claim exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attestation {
    /// Circle has not indexed this transaction (HTTP 404). Normal for the first
    /// seconds-to-minutes after a burn.
    NotObserved,
    /// Indexed but not yet attested — includes Circle's `attestation: "PENDING"`
    /// sentinel string, an empty `messages` array, and (deliberately) HTTP 429.
    /// Rate limiting is not information about the transfer, so it reads as "no
    /// answer yet" rather than as an error worth escalating.
    Pending,
    /// Attested. These are the exact bytes `receiveMessage` takes.
    Complete {
        message: Bytes,
        attestation: Bytes,
        /// Circle's `forwardState`, verbatim when present. On a self-claim this
        /// reads `FAILED` with `INSUFFICIENT_FEE` — which is Circle reporting that
        /// the forwarder could not be paid, i.e. the 1:1 path working exactly as
        /// designed. An operator surface that shows the word FAILED without that
        /// sentence attached will read as a broken transfer, which is why it is
        /// carried rather than dropped.
        forward_state: Option<String>,
        /// Circle's own nonce string, kept for the re-attest endpoint and for
        /// operator-facing messages. NOT used for the idempotency check — that
        /// reads the nonce out of `message` itself (`cctp::nonce_from_message`),
        /// so a wrong or absent value here cannot cause a double claim.
        nonce: Option<String>,
    },
}

impl Attestation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotObserved => "not_observed",
            Self::Pending => "pending",
            Self::Complete { .. } => "complete",
        }
    }
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    messages: Vec<MessageResponse>,
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    status: Option<String>,
    message: Option<String>,
    attestation: Option<String>,
    #[serde(rename = "eventNonce")]
    event_nonce: Option<String>,
    #[serde(rename = "decodedMessage")]
    decoded_message: Option<DecodedMessage>,
    #[serde(rename = "forwardState")]
    forward_state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DecodedMessage {
    nonce: Option<String>,
}

/// Turn one HTTP answer into an [`Attestation`]. Pure, so every branch is
/// testable without a network — including the ones a live test would never
/// reproduce on demand (429, a malformed body, `complete` with no attestation).
///
/// A body that claims `complete` but carries no usable `message`/`attestation` is
/// `Pending`, NOT an error: Circle occasionally reports the status before the
/// payload is queryable, and treating that as a failure would escalate a transfer
/// that is proceeding normally. The only thing that unblocks a claim is bytes.
pub(crate) fn interpret(status: u16, body: &str) -> Attestation {
    match status {
        404 => return Attestation::NotObserved,
        429 => return Attestation::Pending,
        s if !(200..300).contains(&s) => return Attestation::Pending,
        _ => {}
    }
    let Ok(parsed) = serde_json::from_str::<MessagesResponse>(body) else {
        return Attestation::Pending;
    };
    let Some(m) = parsed.messages.into_iter().next() else {
        return Attestation::Pending;
    };
    interpret_message(m)
}

fn interpret_message(m: MessageResponse) -> Attestation {
    if m.status.as_deref() != Some("complete") {
        return Attestation::Pending;
    }
    let (Some(message), Some(attestation)) = (
        m.message.as_deref().and_then(parse_hex),
        m.attestation.as_deref().and_then(parse_hex),
    ) else {
        return Attestation::Pending;
    };
    Attestation::Complete {
        message,
        attestation,
        forward_state: m.forward_state.filter(|f| !f.is_empty()),
        nonce: m
            .decoded_message
            .and_then(|d| d.nonce)
            .or(m.event_nonce)
            .filter(|n| !n.is_empty()),
    }
}

/// Choose the unique message `accept` binds to the caller's recorded burn.
/// Different destinations can share a transaction; response order is not an
/// operation identity. Identical matching burns remain ambiguous until an
/// explicit message identity is available.
///
/// `accept` receives each attested message's bytes and answers whether it is
/// the caller's burn (recipient, amount, route — whatever the caller
/// recorded). Only accepted messages can become `Complete`.
pub fn interpret_matching(status: u16, body: &str, accept: impl Fn(&Bytes) -> bool) -> Attestation {
    if status == 404 {
        return Attestation::NotObserved;
    }
    if !(200..300).contains(&status) {
        return Attestation::Pending;
    }
    let Ok(parsed) = serde_json::from_str::<MessagesResponse>(body) else {
        return Attestation::Pending;
    };
    let mut found: Option<Attestation> = None;
    for item in parsed.messages {
        let attestation = interpret_message(item);
        if let Attestation::Complete { message, .. } = &attestation
            && accept(message)
        {
            if let Some(Attestation::Complete { message: prior, .. }) = &found {
                if prior == message {
                    continue;
                } // duplicated API entry
                return Attestation::Pending;
            }
            found = Some(attestation);
        }
    }
    found.unwrap_or(Attestation::Pending)
}

/// `"PENDING"` (Circle's sentinel) and anything else non-hex yields `None`, which
/// the caller reads as "no bytes yet".
fn parse_hex(v: &str) -> Option<Bytes> {
    let trimmed = v.trim();
    if !trimmed.starts_with("0x") || trimmed.len() <= 2 {
        return None;
    }
    alloy::primitives::hex::decode(&trimmed[2..])
        .ok()
        .map(Bytes::from)
}

/// The messages endpoint for one burn tx.
pub(crate) fn messages_url(base_url: &str, source_domain: u32, tx_hash: B256) -> String {
    format!("{base_url}/v2/messages/{source_domain}?transactionHash={tx_hash}")
}

/// Ask Iris about one burn, once.
///
/// Every failure mode — DNS, TLS, timeout, rate limit, garbage body — collapses
/// to `Pending`, because none of them is information about the transfer. A
/// caller's own escalation clock is what should turn a persistently unanswered
/// burn into something loud; a single unanswered request must not.
pub async fn fetch_attestation(base_url: &str, source_domain: u32, tx_hash: B256) -> Attestation {
    let (status, body) = match request(base_url, source_domain, tx_hash).await {
        Some(answer) => answer,
        None => return Attestation::Pending,
    };
    interpret(status, &body)
}

/// [`fetch_attestation`], choosing the message `accept` binds to the
/// caller's burn (see [`interpret_matching`]).
pub async fn fetch_attestation_matching(
    base_url: &str,
    source_domain: u32,
    tx_hash: B256,
    accept: impl Fn(&Bytes) -> bool,
) -> Attestation {
    let (status, body) = match request(base_url, source_domain, tx_hash).await {
        Some(answer) => answer,
        None => return Attestation::Pending,
    };
    interpret_matching(status, &body, accept)
}

/// One GET against the messages endpoint: `(status, body)`, or `None` when
/// no answer arrived at all.
async fn request(base_url: &str, source_domain: u32, tx_hash: B256) -> Option<(u16, String)> {
    let client = match reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "iris: could not build http client");
            return None;
        }
    };
    let url = messages_url(base_url, source_domain, tx_hash);
    let resp = match client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(
                error = %crate::util::redact::text(&e.to_string()),
                url = %crate::util::redact::url(&url),
                "iris: request failed; treating as pending"
            );
            return None;
        }
    };
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    Some((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_response_yields_the_claim_bytes() {
        let body = r#"{"messages":[{"status":"complete","message":"0x0000000100000000000000061d20","attestation":"0xabcd","decodedMessage":{"nonce":"42"}}]}"#;
        match interpret(200, body) {
            Attestation::Complete {
                message,
                attestation,
                nonce,
                ..
            } => {
                assert_eq!(attestation.as_ref(), &[0xab, 0xcd]);
                assert_eq!(message[0..4], [0, 0, 0, 1], "version word survives");
                assert_eq!(nonce.as_deref(), Some("42"));
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    /// Circle's pending sentinel is the STRING "PENDING" in a hex field — not
    /// hex, not an error, just no bytes yet.
    #[test]
    fn the_pending_sentinel_is_not_mistaken_for_bytes() {
        let body = r#"{"messages":[{"status":"pending","message":"0x1234","attestation":"PENDING","eventNonce":"42"}]}"#;
        assert_eq!(interpret(200, body), Attestation::Pending);
        // …and even a `complete` status with the sentinel is pending, because
        // what unblocks a claim is bytes, not a status string.
        let lying =
            r#"{"messages":[{"status":"complete","message":"0x1234","attestation":"PENDING"}]}"#;
        assert_eq!(interpret(200, lying), Attestation::Pending);
    }

    /// 404 is distinguishable from pending: it means Circle has not INDEXED the
    /// burn, which is the normal state for the first minutes and is also what a
    /// wrong source domain looks like.
    #[test]
    fn not_observed_is_distinct_from_pending() {
        assert_eq!(interpret(404, ""), Attestation::NotObserved);
        assert_eq!(interpret(200, r#"{"messages":[]}"#), Attestation::Pending);
    }

    /// Nothing Iris can do makes a burn look settled or makes a claim look
    /// impossible. Rate limits, outages and garbage all read as "no answer yet".
    #[test]
    fn every_failure_mode_is_merely_pending() {
        for (status, body) in [
            (429u16, "rate limited"),
            (500, "<html>bad gateway</html>"),
            (200, "not json at all"),
            (200, r#"{"messages":[{"status":"complete"}]}"#), // complete, no bytes
            (200, r#"{"unexpected":"shape"}"#),
        ] {
            assert_eq!(
                interpret(status, body),
                Attestation::Pending,
                "status {status} body {body:?}"
            );
        }
    }

    #[test]
    fn the_messages_url_carries_the_source_domain_and_hash() {
        let h: B256 = "0x8d5af04d82bbf836349554e29b02eb3d21f30860bad224ee2ae850ef92d514a3"
            .parse()
            .unwrap();
        // Base's domain is 6 (measured via localDomain()), so a Base→mainnet burn
        // is queried under 6, not under the destination's 0.
        let u = messages_url(IRIS_BASE_URL, 6, h);
        assert!(
            u.starts_with("https://iris-api.circle.com/v2/messages/6?"),
            "{u}"
        );
        assert!(u.contains("0x8d5af04d82bbf836"), "{u}");
    }

    /// Only a message the caller accepts can complete; two different accepted
    /// messages are ambiguous and stay pending; a duplicated entry is one.
    #[test]
    fn matching_completes_only_on_the_accepted_message() {
        let body = r#"{"messages":[
            {"status":"complete","message":"0x01","attestation":"0xaa"},
            {"status":"complete","message":"0x02","attestation":"0xbb"}]}"#;
        let only_two = |m: &Bytes| m.as_ref() == [2u8];
        match interpret_matching(200, body, only_two) {
            Attestation::Complete { attestation, .. } => {
                assert_eq!(attestation.as_ref(), &[0xbb]);
            }
            other => panic!("expected the accepted message, got {other:?}"),
        }
        assert_eq!(
            interpret_matching(200, body, |_: &Bytes| false),
            Attestation::Pending,
            "nothing accepted, nothing completes"
        );
        assert_eq!(
            interpret_matching(200, body, |_: &Bytes| true),
            Attestation::Pending,
            "two different accepted messages are ambiguous"
        );
        let dup = r#"{"messages":[
            {"status":"complete","message":"0x01","attestation":"0xaa"},
            {"status":"complete","message":"0x01","attestation":"0xaa"}]}"#;
        assert!(matches!(
            interpret_matching(200, dup, |_: &Bytes| true),
            Attestation::Complete { .. }
        ));
        assert_eq!(
            interpret_matching(404, "", |_: &Bytes| true),
            Attestation::NotObserved
        );
        assert_eq!(
            interpret_matching(429, "", |_: &Bytes| true),
            Attestation::Pending
        );
    }
}
