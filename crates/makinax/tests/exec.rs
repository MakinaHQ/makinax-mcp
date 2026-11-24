//! `exec`: every send runs a pre-send hook first, and a prepared request is
//! sent exactly as given.
//!
//! The anvil tests are `#[ignore]`d because a plain `cargo test` host may not
//! have foundry. CI runs them with `--ignored` in the job that installs it,
//! where a missing anvil is a hard failure rather than a silent skip.
#![cfg(feature = "write")]

use std::sync::{Arc, Mutex};

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    node_bindings::{Anvil, AnvilInstance},
    primitives::{Address, B256, Bytes, TxKind, U256},
    providers::{Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
};
use makinax::{
    Error,
    build::BuiltCall,
    exec::{AllowAll, Executor, HookVerdict, PreSendHook, PreparedTx, SendOptions},
};

fn key0(anvil: &AnvilInstance) -> (EthereumWallet, Address) {
    let signer = PrivateKeySigner::from(anvil.keys()[0].clone());
    let addr = signer.address();
    (EthereumWallet::from(signer), addr)
}

fn simple_transfer(anvil: &AnvilInstance) -> PreparedTx {
    PreparedTx {
        chain: "local".into(),
        request: TransactionRequest::default()
            .with_from(anvil.addresses()[0])
            .with_kind(TxKind::Call(anvil.addresses()[1]))
            .with_value(U256::from(1u64))
            .with_nonce(0),
        call_key: None,
    }
}

async fn nonce_of(anvil: &AnvilInstance, who: Address) -> u64 {
    ProviderBuilder::new()
        .connect_http(anvil.endpoint_url())
        .get_transaction_count(who)
        .await
        .unwrap()
}

struct No;
#[async_trait::async_trait]
impl PreSendHook for No {
    async fn before_send(&self, _: &PreparedTx) -> HookVerdict {
        HookVerdict::Refuse("no".into())
    }
}

/// Records what it was shown, then allows.
#[derive(Default)]
struct Recorder(Mutex<Vec<PreparedTx>>);
#[async_trait::async_trait]
impl PreSendHook for Recorder {
    async fn before_send(&self, tx: &PreparedTx) -> HookVerdict {
        self.0.lock().unwrap().push(tx.clone());
        HookVerdict::Allow
    }
}

#[tokio::test]
#[ignore = "needs anvil (foundry); CI runs it with --ignored in findings-suite"]
async fn a_refusing_hook_stops_the_send_before_the_chain_sees_it() {
    let anvil = Anvil::new().spawn();
    let (wallet, from) = key0(&anvil);
    let ex = Executor::new(Arc::new(No), wallet, anvil.endpoint());
    let e = ex.send_prepared(simple_transfer(&anvil)).await.unwrap_err();
    assert!(matches!(e, Error::HookRefused(ref r) if r == "no"), "{e:?}");
    assert_eq!(nonce_of(&anvil, from).await, 0, "nothing was sent");
    // The builder path goes through the same hook.
    let built = BuiltCall {
        to: anvil.addresses()[1],
        data: Bytes::new(),
        value: U256::from(1u64),
        label: "transfer",
        summary_json: "{}".into(),
        affected_tokens: vec![],
        is_manage_position: false,
        has_real_accounting: false,
    };
    let e = ex
        .send("local", from, &built, SendOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::HookRefused(_)), "{e:?}");
    assert_eq!(nonce_of(&anvil, from).await, 0, "nothing was sent");
}

#[tokio::test]
#[ignore = "needs anvil (foundry); CI runs it with --ignored in findings-suite"]
async fn allow_all_sends_and_uses_the_given_nonce() {
    let anvil = Anvil::new().spawn();
    let (wallet, from) = key0(&anvil);
    let ex = Executor::new(Arc::new(AllowAll), wallet, anvil.endpoint());
    let built = BuiltCall {
        to: anvil.addresses()[1],
        data: Bytes::new(),
        value: U256::from(1u64),
        label: "transfer",
        summary_json: "{}".into(),
        affected_tokens: vec![],
        is_manage_position: false,
        has_real_accounting: false,
    };
    let sent = ex
        .send(
            "local",
            from,
            &built,
            SendOptions {
                nonce: Some(0),
                gas_limit: None,
            },
        )
        .await
        .unwrap();
    let receipt = ex
        .wait_receipt(sent.tx_hash, std::time::Duration::from_secs(30))
        .await
        .unwrap();
    assert!(receipt.status());
    assert_eq!(receipt.transaction_hash, sent.tx_hash);
    assert_eq!(nonce_of(&anvil, from).await, 1);

    // A nonce the chain already consumed is not replaced by `pending`: the
    // send goes out with it and the node refuses it.
    let e = ex
        .send(
            "local",
            from,
            &built,
            SendOptions {
                nonce: Some(0),
                gas_limit: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Rpc(_)), "{e:?}");
    assert_eq!(nonce_of(&anvil, from).await, 1);
}

/// The request a caller prepared is the transaction that goes out: nonce,
/// gas limit, fees, from, to, data and value are not re-filled. And the hook
/// is shown the request and the call key it was given.
#[tokio::test]
#[ignore = "needs anvil (foundry); CI runs it with --ignored in findings-suite"]
async fn send_prepared_sends_exactly_the_request_it_is_given() {
    let anvil = Anvil::new().spawn();
    let (wallet, from) = key0(&anvil);
    let hook = Arc::new(Recorder::default());
    let ex = Executor::new(hook.clone(), wallet, anvil.endpoint());
    let to = anvil.addresses()[2];
    let data = Bytes::from(vec![0xde, 0xad, 0xbe, 0xef]);
    let request = TransactionRequest::default()
        .with_from(from)
        .with_kind(TxKind::Call(to))
        .with_input(data.clone())
        .with_value(U256::from(7u64))
        .with_nonce(0)
        .with_gas_limit(54_321)
        .with_max_fee_per_gas(9_000_000_000)
        .with_max_priority_fee_per_gas(1_234_567);
    let key = B256::repeat_byte(0x42);
    let sent = ex
        .send_prepared(PreparedTx {
            chain: "local".into(),
            request: request.clone(),
            call_key: Some(key),
        })
        .await
        .unwrap();

    let seen = hook.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].call_key, Some(key));
    assert_eq!(seen[0].chain, "local");
    assert_eq!(seen[0].request, request);

    let plain = ProviderBuilder::new().connect_http(anvil.endpoint_url());
    let tx = plain
        .get_transaction_by_hash(sent.tx_hash)
        .await
        .unwrap()
        .expect("the node has the transaction");
    use alloy::consensus::Transaction as _;
    assert_eq!(tx.inner.signer(), from);
    assert_eq!(tx.nonce(), 0);
    assert_eq!(tx.gas_limit(), 54_321);
    assert_eq!(tx.max_fee_per_gas(), 9_000_000_000);
    assert_eq!(tx.max_priority_fee_per_gas(), Some(1_234_567));
    assert_eq!(tx.to(), Some(to));
    assert_eq!(tx.input(), &data);
    assert_eq!(tx.value(), U256::from(7u64));
}

/// The executor signs the same bytes alloy's wallet-attached provider
/// (`ProviderBuilder::new().wallet(..)`) signs for the same request: the
/// same request sent both ways to two identical fresh chains produces the
/// same transaction hash, with the fillers (fees, chain id) filling only what
/// the request left unset.
#[tokio::test]
#[ignore = "needs anvil (foundry); CI runs it with --ignored in findings-suite"]
async fn signs_the_same_bytes_as_a_plain_wallet_provider() {
    let (a, b) = (Anvil::new().spawn(), Anvil::new().spawn());
    let (wallet, from) = key0(&a);
    let request = TransactionRequest::default()
        .with_from(from)
        .with_kind(TxKind::Call(a.addresses()[1]))
        .with_input(Bytes::from(vec![0x01, 0x02]))
        .with_value(U256::from(3u64))
        .with_nonce(0)
        .with_gas_limit(60_000);
    let direct = *ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_http(a.endpoint_url())
        .send_transaction(request.clone())
        .await
        .unwrap()
        .tx_hash();
    let ex = Executor::new(Arc::new(AllowAll), wallet, b.endpoint());
    let sent = ex
        .send_prepared(PreparedTx {
            chain: "local".into(),
            request,
            call_key: None,
        })
        .await
        .unwrap();
    assert_eq!(
        sent.tx_hash, direct,
        "the executor must sign identical bytes"
    );
}

/// A send failure carries the transport's own text, unprefixed, exactly as a
/// plain wallet provider reports the same failure, so a caller that words
/// its own message around it sees the text it always saw.
#[tokio::test]
#[ignore = "needs anvil (foundry); CI runs it with --ignored in findings-suite"]
async fn a_send_failure_carries_the_transports_own_text() {
    let anvil = Anvil::new().spawn();
    let (wallet, _from) = key0(&anvil);
    // Too little gas for a plain transfer: the node refuses it at submission.
    let mut bad = simple_transfer(&anvil);
    bad.request = bad.request.with_gas_limit(1_000);
    let direct = ProviderBuilder::new()
        .wallet(wallet.clone())
        .connect_http(anvil.endpoint_url())
        .send_transaction(bad.request.clone())
        .await
        .map(|_| ())
        .unwrap_err()
        .to_string();
    let ex = Executor::new(Arc::new(AllowAll), wallet, anvil.endpoint());
    match ex.send_prepared(bad).await {
        Err(Error::Rpc(text)) => assert_eq!(text, direct),
        other => panic!("expected the transport's error, got {other:?}"),
    }
}

/// An unusable URL is a clear error, not a panic, and never a send.
#[tokio::test]
async fn an_unusable_url_refuses_every_call() {
    let wallet = EthereumWallet::from(PrivateKeySigner::random());
    let ex = Executor::new(Arc::new(AllowAll), wallet, "not a url".into());
    let tx = PreparedTx {
        chain: "local".into(),
        request: TransactionRequest::default(),
        call_key: None,
    };
    assert!(matches!(ex.send_prepared(tx).await, Err(Error::Config(_))));
    assert!(matches!(
        ex.wait_receipt(B256::ZERO, std::time::Duration::from_millis(1))
            .await,
        Err(Error::Config(_))
    ));
}

/// The hook runs before anything else, and a refusal is final even when the
/// executor could not have sent anyway.
#[tokio::test]
async fn the_hook_runs_first() {
    let wallet = EthereumWallet::from(PrivateKeySigner::random());
    let ex = Executor::new(Arc::new(No), wallet, "not a url".into());
    let tx = PreparedTx {
        chain: "local".into(),
        request: TransactionRequest::default(),
        call_key: None,
    };
    assert!(matches!(
        ex.send_prepared(tx).await,
        Err(Error::HookRefused(r)) if r == "no"
    ));
}
