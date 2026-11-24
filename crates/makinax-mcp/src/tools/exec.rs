//! Sending (read-write build only): the one tool that signs and broadcasts.
//!
//! The executor is built with `AllowAll`: this server requires nothing
//! before a send. The only sender is this process, and it does not
//! coordinate nonces with any other process holding the same key.

use std::sync::Arc;

use makinax::ext::{AllowAll, Executor, ProviderMode, SendOptions};
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::{
    MakinaxService,
    service::{SignerState, error_text, respond},
};
use makinax::ext::CallSpec;

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ExecArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// What to send; `kind` selects the call.
    pub call: CallSpec,
    /// For a CCTP bridge only: the route the order is built against.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    pub cctp_route: Option<serde_json::Value>,
    /// The nonce to sign with. Default: the chain's pending count.
    #[serde(default)]
    pub nonce: Option<u64>,
    /// The gas limit to sign with. Default: estimated at send.
    #[serde(default)]
    pub gas_limit: Option<u64>,
    /// Wait up to this many seconds for the receipt. Default: return as
    /// soon as the node accepts the transaction.
    #[serde(default)]
    pub wait_s: Option<u64>,
}

#[tool_router(router = exec_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "Sign and send a call to the module with the configured signer, and return the transaction hash. Sends only to the module: a call to any other target, or one that calls an owner-restricted function, is refused (owner calls are encoded for the Safe owners with encode_owner_call or build_owner_batch, never sent). The exception is the guardian's `pause()` and `unpause()`: a guardian key sends them here, as raw calls to the module. Sends immediately. No simulation is required. One sender per key: this server does not coordinate with other processes using the same key.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            open_world_hint = true
        )
    )]
    async fn execute_call(&self, Parameters(a): Parameters<ExecArgs>) -> CallToolResult {
        respond(self.execute_call_impl(a).await)
    }
}

impl MakinaxService {
    async fn execute_call_impl(&self, a: ExecArgs) -> Result<serde_json::Value, String> {
        let m = self.module(&a.module)?;
        self.provider_for(&a.module, m)?;
        // Built and checked before the signer is touched: what may be sent
        // does not depend on whether a key is configured.
        let built = self
            .build_for(&a.module, m, &a.call, a.cctp_route.as_ref())
            .await?;
        check_sendable(m.module, &built)?;
        let (wallet, from) = match self.signer.as_ref() {
            SignerState::Ready { wallet, address } => (wallet.clone(), *address),
            SignerState::None => {
                return Err(
                    "no signer is configured: set [signer] in the config to send".to_string(),
                );
            }
            SignerState::Failed(e) => {
                return Err(format!("the configured signer could not be built: {e}"));
            }
        };
        let url = self
            .providers
            .url_for_mode(&m.chain, ProviderMode::Mainnet)
            .map_err(|e| e.to_string())?
            .to_string();
        let executor = Executor::new(Arc::new(AllowAll), wallet, url);
        let sent = executor
            .send(
                &m.chain,
                from,
                &built,
                SendOptions {
                    nonce: a.nonce,
                    gas_limit: a.gas_limit,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        let mut out = json!({
            "tx_hash": sent.tx_hash.to_string(),
            "chain": m.chain,
            "from": format!("{from:?}"),
            "to": format!("{:?}", built.to),
            "label": built.label,
        });
        if let Some(secs) = a.wait_s.filter(|s| *s > 0) {
            match executor
                .wait_receipt(sent.tx_hash, std::time::Duration::from_secs(secs))
                .await
            {
                Ok(r) => {
                    out["status"] = json!(if r.status() { "success" } else { "reverted" });
                    out["block"] = json!(r.block_number);
                    out["gas_used"] = json!(r.gas_used);
                }
                // The transaction was accepted; only the wait failed.
                Err(e) => out["receipt_error"] = json!(error_text(e)),
            }
        }
        Ok(out)
    }
}

/// Refused: a send to anything but the module.
pub(crate) const ONLY_THE_MODULE: &str = "execute_call sends only to the module";
/// Refused: an owner-restricted function, even addressed to the module.
pub(crate) const OWNER_CALLS_ARE_NOT_SENT: &str = "owner calls are encoded for the Safe owners (encode_owner_call / build_owner_batch), never sent";

/// The two owner-list functions a guardian sends itself: `pause()` and
/// `unpause()` are `onlyGuardian` in the contract (the Safe is a permanent
/// guardian, which is why they are in the owner list too). Exempt by exact
/// selector, from the contract bindings; the on-chain modifier decides who
/// may call them.
pub(crate) const GUARDIAN_SENDABLE: [[u8; 4]; 2] = {
    use alloy::sol_types::SolCall;
    use makinax::ext::bindings::IMakinaXModule;
    [
        IMakinaXModule::pauseCall::SELECTOR,
        IMakinaXModule::unpauseCall::SELECTOR,
    ]
};

/// What execute_call may send: a call to the configured module whose
/// selector is not one of the module's owner-restricted functions, except
/// the guardian's `pause()` and `unpause()`. Every built kind targets the
/// module; this is what bounds `raw`.
pub(crate) fn check_sendable(
    module: alloy::primitives::Address,
    built: &makinax::ext::BuiltCall,
) -> Result<(), String> {
    if built.to != module {
        return Err(format!(
            "{ONLY_THE_MODULE} ({module:?}); the call targets {:?}",
            built.to
        ));
    }
    if let Some(selector) = built.data.get(..4) {
        let owner = makinax::ext::owner_function_names().iter().map(|sig| {
            let h = alloy::primitives::keccak256(sig.as_bytes());
            (*sig, [h[0], h[1], h[2], h[3]])
        });
        if let Some(sig) = refused_owner_function(selector, owner) {
            return Err(format!(
                "{OWNER_CALLS_ARE_NOT_SENT}: `{sig}` is owner-restricted"
            ));
        }
    }
    Ok(())
}

/// The owner function `selector` calls, unless it is one of
/// [`GUARDIAN_SENDABLE`] (compared as whole selectors).
fn refused_owner_function<'a>(
    selector: &[u8],
    owner: impl IntoIterator<Item = (&'a str, [u8; 4])>,
) -> Option<&'a str> {
    if GUARDIAN_SENDABLE.iter().any(|g| g[..] == *selector) {
        return None;
    }
    owner
        .into_iter()
        .find(|(_, s)| s[..] == *selector)
        .map(|(sig, _)| sig)
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{Address, Bytes, keccak256};
    use makinax::ext::{BuiltCall, owner_function_names};

    use super::*;

    const PAUSE: [u8; 4] = [0x84, 0x56, 0xcb, 0x59];
    const UNPAUSE: [u8; 4] = [0x3f, 0x4b, 0xa8, 0x3a];

    fn selector(sig: &str) -> [u8; 4] {
        let h = keccak256(sig.as_bytes());
        [h[0], h[1], h[2], h[3]]
    }

    fn to_module(data: Vec<u8>) -> (Address, BuiltCall) {
        let module = Address::repeat_byte(0x22);
        let built = BuiltCall {
            to: module,
            data: Bytes::from(data),
            value: Default::default(),
            label: "test",
            summary_json: String::new(),
            affected_tokens: Vec::new(),
            is_manage_position: false,
            has_real_accounting: false,
        };
        (module, built)
    }

    #[test]
    fn the_exemption_is_pause_and_unpause_by_their_abi_selectors() {
        assert_eq!(GUARDIAN_SENDABLE, [PAUSE, UNPAUSE]);
        assert_eq!(selector("pause()"), PAUSE);
        assert_eq!(selector("unpause()"), UNPAUSE);
    }

    #[test]
    fn pause_and_unpause_are_sendable_and_every_other_owner_function_is_refused() {
        let mut exempt = Vec::new();
        for sig in owner_function_names() {
            let (module, built) = to_module(selector(sig).to_vec());
            match check_sendable(module, &built) {
                Ok(()) => exempt.push(*sig),
                Err(e) => {
                    assert!(e.contains(OWNER_CALLS_ARE_NOT_SENT), "{sig}: {e}");
                    assert!(e.contains(&format!("`{sig}`")), "{sig}: {e}");
                }
            }
        }
        assert_eq!(exempt, ["pause()", "unpause()"]);
    }

    #[test]
    fn pause_with_trailing_bytes_is_still_the_guardian_call() {
        let (module, built) = to_module([&PAUSE[..], &[0u8; 32][..]].concat());
        assert_eq!(check_sendable(module, &built), Ok(()));
    }

    /// Planted owner functions whose selectors sit next to, between, or
    /// share a prefix with the two exempt ones, and whose names contain
    /// "pause": every one is refused. The exemption is those two whole
    /// selectors, not a prefix, a range or a name match.
    #[test]
    fn the_exemption_is_exactly_two_selectors_not_a_prefix_or_range() {
        let planted: Vec<(&str, [u8; 4])> = vec![
            ("pause()", PAUSE),
            ("unpause()", UNPAUSE),
            ("belowPause()", [0x84, 0x56, 0xcb, 0x58]),
            ("abovePause()", [0x84, 0x56, 0xcb, 0x5a]),
            ("pausePrefix()", [0x84, 0x56, 0xcb, 0x00]),
            ("pauseFirstByte()", [0x84, 0x00, 0x00, 0x00]),
            ("belowUnpause()", [0x3f, 0x4b, 0xa8, 0x39]),
            ("aboveUnpause()", [0x3f, 0x4b, 0xa8, 0x3b]),
            ("unpausePrefix()", [0x3f, 0x4b, 0xa8, 0xff]),
            ("betweenTheTwo()", [0x50, 0x00, 0x00, 0x00]),
            ("pause(uint256)", selector("pause(uint256)")),
            ("unpauseFor(address)", selector("unpauseFor(address)")),
        ];
        let mut allowed = Vec::new();
        for (sig, sel) in &planted {
            match refused_owner_function(sel, planted.iter().copied()) {
                None => allowed.push(*sig),
                Some(hit) => assert_eq!(hit, *sig),
            }
        }
        assert_eq!(allowed, ["pause()", "unpause()"]);
    }

    #[test]
    fn a_non_owner_selector_and_short_data_pass() {
        for data in [vec![0x12, 0x34, 0x56, 0x78], vec![0x84, 0x56, 0xcb], vec![]] {
            let (module, built) = to_module(data.clone());
            assert_eq!(check_sendable(module, &built), Ok(()), "{data:?}");
        }
    }

    #[test]
    fn pause_to_another_target_is_still_refused() {
        let (_, built) = to_module(PAUSE.to_vec());
        let e = check_sendable(Address::repeat_byte(0x33), &built).unwrap_err();
        assert!(e.contains(ONLY_THE_MODULE), "{e}");
    }

    /// A send that fails at the transport: the endpoint URL the transport
    /// quotes reaches the client with its key masked. The endpoint is a
    /// keyed URL on a closed loopback port, so nothing is sent anywhere.
    #[tokio::test]
    async fn a_failed_sends_error_carries_no_endpoint_key() {
        use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};
        use rmcp::handler::server::wrapper::Parameters;

        const KEY: &str = "9fKd2Lm4Xp7Qw1Nz8Bt3Yr";
        let module = "0x2222222222222222222222222222222222222222";
        let cfg = serde_json::from_value(serde_json::json!({
            "rpc_urls": { "base": format!("http://127.0.0.1:1/v2/{KEY}") },
            "modules": { "main": {
                "chain": "base",
                "safe": "0x1111111111111111111111111111111111111111",
                "module": module,
            }},
        }))
        .unwrap();
        let mut svc = MakinaxService::new(cfg).await.unwrap();
        let signer = PrivateKeySigner::random();
        svc.signer = Arc::new(SignerState::Ready {
            address: signer.address(),
            wallet: EthereumWallet::from(signer),
        });

        // With no nonce the pending count is read first; with one, the send
        // itself is what fails.
        for nonce in [None, Some(0)] {
            let args: ExecArgs = serde_json::from_value(serde_json::json!({
                "module": "main",
                "call": {"kind": "raw", "to": module, "data": "0x12345678"},
                "nonce": nonce,
                "gas_limit": 100_000,
            }))
            .unwrap();
            let r = svc.execute_call(Parameters(args)).await;
            assert_eq!(r.is_error, Some(true), "{r:?}");
            let text: String = r
                .content
                .iter()
                .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                .collect();
            assert!(!text.contains(KEY), "the key leaked: {text}");
            assert!(
                text.contains("127.0.0.1:1/v2/<redacted>"),
                "the endpoint is still named: {text}"
            );
            assert!(text.starts_with("rpc: "), "{text}");
        }
    }
}
