//! Sending: the one place this crate signs and broadcasts a transaction.
//!
//! There is no way to send without a [`PreSendHook`]. [`Executor::new`] takes
//! one, there is no `Default` and no hook-less constructor, and the hook runs
//! before anything is signed: a [`HookVerdict::Refuse`] returns
//! [`Error::HookRefused`] and the chain never sees the transaction.
//!
//! This crate enforces nothing itself. Whatever a caller requires before a
//! send (a simulation, a limit, an approval) belongs in its hook. [`AllowAll`]
//! exists for a caller that has decided it needs none, and has to be named to
//! be used.
//!
//! The signing provider is alloy's standard one: `ProviderBuilder::new()`
//! with the given wallet attached, over an HTTP transport whose every request
//! carries the crate-wide deadline (`chain::provider::http_client`). The
//! recommended fillers fill only what the request leaves unset. A request
//! that already carries a nonce, a gas limit or fees is sent with exactly
//! those.

use std::{sync::Arc, time::Duration};

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, TxKind},
    providers::{DynProvider, PendingTransactionBuilder, Provider, ProviderBuilder},
    rpc::types::{TransactionReceipt, TransactionRequest},
};

use crate::{
    build::BuiltCall,
    error::{Error, Result},
};

/// A transaction request the caller assembled, plus what a hook needs to
/// judge it.
#[derive(Debug, Clone)]
pub struct PreparedTx {
    /// The chain name the request is for (a key of the caller's RPC config).
    pub chain: String,
    /// Sent exactly as given; only fields left unset are filled.
    pub request: TransactionRequest,
    /// An opaque key the caller's hook can match on (for example the hash
    /// of the call a simulation was issued for). `None` when the caller has
    /// no such key.
    pub call_key: Option<B256>,
}

/// A hook's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookVerdict {
    Allow,
    /// Refused, with the reason. Nothing is signed or sent.
    Refuse(String),
}

/// Runs before every send. Refusing is final for that send.
#[async_trait::async_trait]
pub trait PreSendHook: Send + Sync {
    async fn before_send(&self, tx: &PreparedTx) -> HookVerdict;
}

/// Explicit allow-everything hook. A caller must choose it on purpose; it is
/// never applied implicitly.
#[derive(Debug, Clone, Copy)]
pub struct AllowAll;

#[async_trait::async_trait]
impl PreSendHook for AllowAll {
    async fn before_send(&self, _: &PreparedTx) -> HookVerdict {
        HookVerdict::Allow
    }
}

/// What [`Executor::send`] does beyond the call itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct SendOptions {
    /// The nonce to sign with. `None`: the chain's `pending` count.
    pub nonce: Option<u64>,
    /// The gas limit to sign with. `None`: estimated at send.
    pub gas_limit: Option<u64>,
}

/// A transaction the node accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub tx_hash: B256,
}

/// Signs and broadcasts, always behind a [`PreSendHook`].
pub struct Executor {
    hook: Arc<dyn PreSendHook>,
    rpc_url: String,
    /// Built once, so a receipt wait watches the same provider the send went
    /// through. `Err` holds the reason the URL was unusable; every call then
    /// refuses with it before any network request.
    provider: std::result::Result<DynProvider, String>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor")
            .field("rpc_url", &crate::util::redact::url(&self.rpc_url))
            .finish_non_exhaustive()
    }
}

/// An RPC client whose every request carries the crate-wide deadline
/// (`chain::provider::http_client`, process-wide). Falls back to the default
/// client only if building the timed one failed, which it cannot with its
/// options.
fn timed_rpc_client(url: reqwest::Url) -> alloy::rpc::client::RpcClient {
    let http = crate::chain::provider::http_client()
        .map(|c| alloy::transports::http::ReqwestTransport::with_client(c, url.clone()))
        .unwrap_or_else(|_| alloy::transports::http::ReqwestTransport::new(url));
    alloy::rpc::client::RpcClient::new(http, false)
}

impl Executor {
    /// An executor that signs with `wallet` against `rpc_url`, and asks
    /// `hook` before every send. There is deliberately no constructor
    /// without a hook.
    pub fn new(hook: Arc<dyn PreSendHook>, wallet: EthereumWallet, rpc_url: String) -> Self {
        let provider = rpc_url
            .parse::<reqwest::Url>()
            .map(|parsed| {
                ProviderBuilder::new()
                    .wallet(wallet)
                    .connect_client(timed_rpc_client(parsed))
                    .erased()
            })
            .map_err(|e| format!("executor rpc url: {e}"));
        Self {
            hook,
            rpc_url,
            provider,
        }
    }

    fn provider(&self) -> Result<&DynProvider> {
        self.provider
            .as_ref()
            .map_err(|reason| Error::Config(reason.clone()))
    }

    /// Build a request for `built` from `from`, apply `opts`, run the hook,
    /// send. The nonce is `opts.nonce`, or the chain's `pending` count.
    pub async fn send(
        &self,
        chain: &str,
        from: Address,
        built: &BuiltCall,
        opts: SendOptions,
    ) -> Result<Sent> {
        let provider = self.provider()?;
        let nonce = match opts.nonce {
            Some(n) => n,
            None => provider
                .get_transaction_count(from)
                .pending()
                .await
                .map_err(|e| Error::rpc(format!("get_transaction_count({from:?}): {e}")))?,
        };
        let mut request = TransactionRequest::default()
            .with_from(from)
            .with_kind(TxKind::Call(built.to))
            .with_input(built.data.clone())
            .with_value(built.value)
            .with_nonce(nonce);
        if let Some(limit) = opts.gas_limit {
            request = request.with_gas_limit(limit);
        }
        self.send_prepared(PreparedTx {
            chain: chain.to_string(),
            request,
            call_key: None,
        })
        .await
    }

    /// For a caller that assembled the request itself: run the hook, then
    /// send exactly this request.
    ///
    /// Errors: [`Error::HookRefused`] when the hook refused (nothing was
    /// signed or sent); [`Error::Rpc`] carrying the transport's own error
    /// text, unprefixed, when the send itself failed. Credentials in an
    /// endpoint URL the transport quotes are masked
    /// ([`crate::util::redact::text`]); the text is otherwise the
    /// transport's. A send failure is not proof the node did not accept the
    /// transaction.
    pub async fn send_prepared(&self, tx: PreparedTx) -> Result<Sent> {
        // The hook runs first, unconditionally: a hook that tracks what it
        // admitted sees every attempt, whatever happens after it.
        if let HookVerdict::Refuse(reason) = self.hook.before_send(&tx).await {
            return Err(Error::HookRefused(reason));
        }
        let pending = self
            .provider()?
            .send_transaction(tx.request)
            .await
            .map_err(Error::rpc)?;
        Ok(Sent {
            tx_hash: *pending.tx_hash(),
        })
    }

    /// Wait up to `timeout` for the receipt of `tx_hash`, watching through
    /// the provider the send used.
    pub async fn wait_receipt(
        &self,
        tx_hash: B256,
        timeout: Duration,
    ) -> Result<TransactionReceipt> {
        let provider = self.provider()?;
        let pending = PendingTransactionBuilder::new(provider.root().clone(), tx_hash);
        match tokio::time::timeout(timeout, pending.get_receipt()).await {
            Ok(Ok(receipt)) => Ok(receipt),
            Ok(Err(e)) => Err(Error::rpc(format!("receipt for {tx_hash:?}: {e}"))),
            Err(_) => Err(Error::Rpc(format!(
                "no receipt for {tx_hash:?} within {}s",
                timeout.as_secs()
            ))),
        }
    }
}
