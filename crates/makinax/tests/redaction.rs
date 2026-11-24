//! An error built from a transport, provider or HTTP failure never carries
//! an endpoint's key. Endpoint URLs routinely hold the key in their path or
//! query, and a transport error quotes the URL it was talking to; every
//! place this crate turns such an error into text masks the key and keeps
//! the host.
//!
//! Two sources of failure are used, neither needs a network: a mocked RPC
//! whose error message quotes a keyed URL, and a keyed URL on a closed
//! loopback port, whose real connection error quotes the URL itself.

use alloy::{
    primitives::Address,
    providers::{Provider, ProviderBuilder, mock::Asserter},
};
use makinax::{
    Error,
    chain::provider::RootProvider,
    config::{EventsConfig, ModuleConfig},
    util::redact::MARK,
};

/// An obviously synthetic key, in the shape providers use.
const KEY: &str = "9fKd2Lm4Xp7Qw1Nz8Bt3Yr";
const HOST: &str = "rpc.example.com";

const SAFE: Address = Address::with_last_byte(0x0a);
const MODULE: Address = Address::with_last_byte(0x0d);
const TOKEN: Address = Address::with_last_byte(0x11);

/// What a node or a proxy in front of it might answer: its own upstream,
/// key included.
fn keyed_message() -> String {
    format!("upstream https://{HOST}/v2/{KEY} refused the request")
}

/// A provider whose next `n` calls fail with [`keyed_message`].
fn failing(n: usize) -> RootProvider {
    let mock = Asserter::new();
    for _ in 0..n {
        mock.push_failure_msg(keyed_message());
    }
    ProviderBuilder::new().connect_mocked_client(mock).erased()
}

/// The text has lost the key, kept the host, and says it was redacted.
#[track_caller]
fn assert_masked(text: &str) {
    assert!(!text.contains(KEY), "the key leaked: {text}");
    assert!(text.contains(MARK), "nothing was redacted: {text}");
    assert!(
        text.contains(HOST) || text.contains("127.0.0.1"),
        "the host must survive: {text}"
    );
}

/// `Error::Rpc`'s own field, which a caller's mapping takes verbatim, and
/// its rendered message.
#[track_caller]
fn assert_rpc_masked(e: Error) {
    let shown = e.to_string();
    match e {
        Error::Rpc(text) => assert_masked(&text),
        other => panic!("expected Error::Rpc, got {other:?}"),
    }
    assert_masked(&shown);
}

fn module_cfg() -> ModuleConfig {
    ModuleConfig {
        chain: "base".into(),
        safe: SAFE,
        module: MODULE,
        rootfile: None,
        operator_address: None,
        guardian_address: None,
        swappers: Default::default(),
    }
}

/// The mock really does quote the key: without this, every assertion below
/// would pass on a message that never held one.
#[tokio::test]
async fn the_mocked_failure_quotes_the_key() {
    let raw = failing(1).get_block_number().await.unwrap_err().to_string();
    assert!(raw.contains(KEY), "{raw}");
}

#[tokio::test]
async fn module_reads_mask_the_key() {
    use makinax::reads::{
        balances::read_balances,
        module_state::read_module_state,
        oracle::{read_feed_routes, read_price},
    };
    assert_rpc_masked(
        read_module_state(&failing(4), &module_cfg())
            .await
            .unwrap_err(),
    );
    assert_rpc_masked(
        read_balances(&failing(8), SAFE, &[TOKEN])
            .await
            .unwrap_err(),
    );
    assert_rpc_masked(
        read_price(&failing(4), MODULE, TOKEN, None)
            .await
            .unwrap_err(),
    );
    assert_rpc_masked(
        read_feed_routes(&failing(4), MODULE, &[TOKEN], 1)
            .await
            .unwrap_err(),
    );
    assert_rpc_masked(
        makinax::instructions::verify::fetch_onchain_root(&failing(2), MODULE)
            .await
            .unwrap_err(),
    );
}

#[tokio::test]
async fn merkl_reads_mask_the_key() {
    use makinax::reads::merkl::{read_claim_recipient, read_claimed, read_merkle_root};
    assert_rpc_masked(read_merkle_root(&failing(2), MODULE).await.unwrap_err());
    assert_rpc_masked(
        read_claimed(&failing(2), MODULE, SAFE, TOKEN)
            .await
            .unwrap_err(),
    );
    assert_rpc_masked(
        read_claim_recipient(&failing(2), MODULE, SAFE, TOKEN)
            .await
            .unwrap_err(),
    );
}

/// The events read reports a failure two ways: as the error when nothing was
/// read, and inside the page's `note` when something was.
#[tokio::test]
async fn the_events_read_masks_the_key_in_its_error_and_in_its_note() {
    use makinax::reads::events::read_module_events;
    let cfg = EventsConfig {
        chunk_blocks: 10,
        max_calls: 2,
    };
    assert_rpc_masked(
        read_module_events(&failing(2), MODULE, 0, 99, &cfg)
            .await
            .unwrap_err(),
    );

    let mock = Asserter::new();
    mock.push_success(&Vec::<alloy::rpc::types::Log>::new());
    mock.push_failure_msg(keyed_message());
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 0, 99, &cfg)
        .await
        .expect("one window was read, so the rest is a partial page");
    let note = page.note.expect("the page says why it stopped");
    assert!(note.contains("Last error"), "{note}");
    assert_masked(&note);
}

#[tokio::test]
async fn a_declared_check_and_the_head_read_mask_the_key() {
    let e = makinax::owner::dyn_call::run_check(
        &failing(2),
        MODULE,
        "isOperator(address)",
        &[format!("{SAFE:?}")],
        None,
    )
    .await
    .unwrap_err();
    assert_rpc_masked(e);

    match makinax::chain::recent_head(&failing(2), true).await {
        Err(e) => assert_masked(&e.to_string()),
        Ok(head) => panic!("the head read cannot have succeeded: {head}"),
    }
}

/// A swapper slot whose read failed is recorded with the reason.
#[tokio::test]
async fn the_swapper_scan_masks_the_key_in_what_it_could_not_read() {
    use makinax::chain::swappers::{SCAN_MAX_ID, scan};
    let s = scan(&failing(usize::from(SCAN_MAX_ID) + 1), MODULE, &[]).await;
    assert!(!s.unreadable.is_empty());
    for (_, why) in &s.unreadable {
        assert_masked(why);
    }
}

/// A contract call that failed without revert data is reported as
/// infrastructure, with the transport's text as the explanation.
#[tokio::test]
async fn a_decoded_contract_error_masks_the_key() {
    use makinax::chain::{module::IMakinaXModule, taxonomy::decode_contract_error};
    let provider = failing(1);
    let e = IMakinaXModule::new(MODULE, &provider)
        .allowedInstrRoot()
        .call()
        .await
        .unwrap_err();
    assert!(e.to_string().contains(KEY), "{e}");
    let decoded = decode_contract_error(&e);
    assert_eq!(decoded.code, "rpc_error");
    assert_masked(&decoded.explanation);
}

#[tokio::test]
async fn the_cctp_deployment_read_masks_the_key() {
    use makinax::bridges::{
        cctp::{CctpCapabilities, CctpDeployment as Deployment},
        metadata_rpc::verify_endpoint,
    };
    let deployment = Deployment {
        domain: 6,
        usdc: Address::with_last_byte(1),
        token_messenger: Address::with_last_byte(2),
        message_transmitter: Address::with_last_byte(3),
        registry: Address::with_last_byte(4),
        encoder: Address::with_last_byte(5),
        bridge_id: 3,
        capabilities: CctpCapabilities {
            standard: true,
            fast: false,
        },
    };
    assert_rpc_masked(
        verify_endpoint(&failing(2), 8453, &deployment)
            .await
            .unwrap_err(),
    );
}

#[cfg(feature = "write")]
mod write {
    use std::{sync::Arc, time::Duration};

    use alloy::{
        network::{EthereumWallet, TransactionBuilder},
        primitives::{B256, Bytes, TxKind, U256},
        rpc::types::TransactionRequest,
        signers::local::PrivateKeySigner,
    };
    use makinax::{
        build::BuiltCall,
        exec::{AllowAll, Executor, PreparedTx, SendOptions},
    };

    use super::*;

    /// A keyed endpoint nothing listens on: the connection is refused at
    /// once, and the transport's error quotes the whole URL.
    fn dead_keyed_endpoint() -> String {
        format!("http://127.0.0.1:1/v2/{KEY}")
    }

    fn executor() -> (Executor, Address) {
        let signer = PrivateKeySigner::random();
        let from = signer.address();
        let ex = Executor::new(
            Arc::new(AllowAll),
            EthereumWallet::from(signer),
            dead_keyed_endpoint(),
        );
        (ex, from)
    }

    fn built() -> BuiltCall {
        BuiltCall {
            to: MODULE,
            data: Bytes::from(vec![0x12, 0x34, 0x56, 0x78]),
            value: U256::ZERO,
            label: "test",
            summary_json: String::new(),
            affected_tokens: Vec::new(),
            is_manage_position: false,
            has_real_accounting: false,
        }
    }

    /// The control: the same failure through a plain provider does quote
    /// the key, so the executor tests below are about a real leak.
    #[tokio::test]
    async fn a_plain_provider_quotes_the_endpoint_key() {
        let raw = ProviderBuilder::new()
            .connect_http(dead_keyed_endpoint().parse().unwrap())
            .get_block_number()
            .await
            .unwrap_err()
            .to_string();
        assert!(raw.contains(KEY), "{raw}");
    }

    /// `send` with no nonce reads the pending count first: that read's
    /// failure is masked, and still says which call failed.
    #[tokio::test]
    async fn a_failed_nonce_read_masks_the_key() {
        let (ex, from) = executor();
        let e = ex
            .send("base", from, &built(), SendOptions::default())
            .await
            .unwrap_err();
        assert!(
            matches!(&e, Error::Rpc(m) if m.starts_with("get_transaction_count(")),
            "{e:?}"
        );
        assert_rpc_masked(e);
    }

    /// `send` with a nonce, and `send_prepared`: the send's own failure is
    /// masked.
    #[tokio::test]
    async fn a_failed_send_masks_the_key() {
        let (ex, from) = executor();
        let e = ex
            .send(
                "base",
                from,
                &built(),
                SendOptions {
                    nonce: Some(0),
                    gas_limit: Some(100_000),
                },
            )
            .await
            .unwrap_err();
        assert_rpc_masked(e);

        let e = ex
            .send_prepared(PreparedTx {
                chain: "base".into(),
                request: TransactionRequest::default()
                    .with_from(from)
                    .with_kind(TxKind::Call(MODULE))
                    .with_nonce(0),
                call_key: None,
            })
            .await
            .unwrap_err();
        assert_rpc_masked(e);
    }

    /// The receipt wait fails either with the transport's error or by
    /// running out of time; neither text carries the key.
    #[tokio::test]
    async fn a_failed_receipt_wait_carries_no_key() {
        let (ex, _) = executor();
        let e = ex
            .wait_receipt(B256::repeat_byte(7), Duration::from_secs(2))
            .await
            .unwrap_err();
        let text = e.to_string();
        assert!(matches!(e, Error::Rpc(_)), "{e:?}");
        assert!(!text.contains(KEY), "{text}");
        if !text.contains("no receipt for") {
            assert_masked(&text);
        }
    }

    #[test]
    fn the_executors_debug_output_masks_the_key() {
        let (ex, _) = executor();
        let shown = format!("{ex:?}");
        assert!(!shown.contains(KEY), "{shown}");
    }

    #[tokio::test]
    async fn a_gas_estimate_and_a_trace_mask_the_key() {
        use makinax::sim::{gas, trace};
        let tx = TransactionRequest::default().with_to(MODULE);
        assert_rpc_masked(
            gas::budget(&failing(1), &tx, "test")
                .await
                .map(|_| ())
                .unwrap_err(),
        );
        let e = trace::fetch(&failing(1), B256::repeat_byte(7))
            .await
            .map(|_| ())
            .unwrap_err();
        assert!(e.starts_with("debug_traceTransaction: "), "{e}");
        assert_masked(&e);
        let e = trace::fetch_call(&failing(1), &tx).await.unwrap_err();
        assert!(e.starts_with("debug_traceCall: "), "{e}");
        assert_masked(&e);
    }

    /// An owner-batch session attached to an endpoint that cannot be
    /// reached reports the failure as text.
    #[tokio::test]
    async fn an_owner_fork_session_masks_the_key() {
        use makinax::owner::sim::SafeForkSession;
        let e = match SafeForkSession::attach(dead_keyed_endpoint(), SAFE).await {
            Err(e) => e,
            Ok(_) => panic!("nothing listens on the endpoint"),
        };
        assert!(e.starts_with("evm_snapshot: "), "{e}");
        assert_masked(&e);
    }

    #[cfg(feature = "tenderly")]
    #[test]
    fn an_unparseable_tenderly_body_masks_the_key() {
        let body = format!(r#"{{"error":"see https://{HOST}/v2/{KEY}"}} trailing"#);
        let e = makinax::sim::tenderly::parse_simulate_response(&body).unwrap_err();
        assert!(
            matches!(&e, Error::Rpc(m) if m.starts_with("tenderly simulate response parse: ")),
            "{e:?}"
        );
        assert_rpc_masked(e);
    }
}

/// A fetcher pointed at a caller-supplied base URL: a connection failure
/// quotes that URL.
#[cfg(feature = "fetch-across")]
#[tokio::test]
async fn the_across_fetch_masks_a_keyed_base_url() {
    use makinax::fetch::across::{AcrossFeeRequest, fetch_suggested_fees};
    let req = AcrossFeeRequest {
        input_token: TOKEN,
        output_token: TOKEN,
        origin_chain_id: 8453,
        destination_chain_id: 1,
        amount: alloy::primitives::U256::from(1u64),
    };
    let base = format!("http://127.0.0.1:1/v2/{KEY}");
    let e = fetch_suggested_fees(&base, &req, "test").await.unwrap_err();
    assert!(
        matches!(&e, Error::Rpc(m) if m.starts_with("across suggested-fees: ")),
        "{e:?}"
    );
    // The request's own token addresses are not credentials and stay.
    assert!(e.to_string().contains(&format!("{TOKEN:?}")), "{e}");
    assert_rpc_masked(e);
}

#[cfg(feature = "fetch-merkl")]
#[tokio::test]
async fn the_merkl_fetch_masks_a_keyed_base_url() {
    use makinax::fetch::merkl::fetch_rewards;
    let base = format!("http://127.0.0.1:1/v2/{KEY}");
    let e = fetch_rewards(&base, SAFE, 8453, "test").await.unwrap_err();
    assert!(
        matches!(&e, Error::Rpc(m) if m.starts_with("merkl rewards: ")),
        "{e:?}"
    );
    assert_rpc_masked(e);
}
