//! `balanceOf(safe)` for the tokens a module's rootfile touches.
//!
//! The scan universe is the rootfile's declared affected tokens, plus any
//! tokens a caller's own command decoders measure (see
//! [`registered_tokens_with`]), plus the oracle-feed-registered tokens. An
//! empty affected-token list does not mean an instruction moves no token.
//! This selects read observations only; it does not grant a route or
//! permission.

use std::collections::BTreeSet;

use alloy::{
    network::Ethereum,
    primitives::{Address, U256},
    providers::Provider,
};
use serde::Serialize;

use crate::{
    chain::module::IERC20,
    error::Result,
    instructions::{Action, library::InstructionLibrary},
};

/// A token's already-known `(symbol, decimals)`, or `None` to read them.
pub(crate) type TokenMeta = Option<(Option<String>, Option<u8>)>;

#[derive(Debug, Clone, Serialize)]
pub struct TokenBalance {
    pub token: Address,
    pub symbol: Option<String>,
    pub decimals: Option<u8>,
    pub balance: String, // u256 as decimal string for JSON safety
    pub balance_hex: String,
}

/// The non-zero tokens `actions` declare as affected, plus whatever `extra`
/// returns for each action (a caller's own exact command decoders, e.g. the
/// token a transfer command moves when its affected-token list is empty).
/// Sorted and deduplicated.
pub fn root_tokens<'a, F, I>(
    actions: impl IntoIterator<Item = &'a Action>,
    extra: F,
) -> BTreeSet<Address>
where
    F: Fn(&Action) -> I,
    I: IntoIterator<Item = Address>,
{
    let mut set: BTreeSet<Address> = actions
        .into_iter()
        .flat_map(|a| {
            a.instruction
                .affected_tokens
                .iter()
                .copied()
                .chain(extra(a))
                .collect::<Vec<_>>()
        })
        .collect();
    set.remove(&Address::ZERO);
    set
}

/// The rootfile's declared affected tokens (non-zero, sorted, unique).
pub fn registered_tokens(lib: &InstructionLibrary) -> Vec<Address> {
    registered_tokens_with(lib, |_| Vec::new())
}

/// [`registered_tokens`], plus the tokens `extra` measures for each action.
pub(crate) fn registered_tokens_with<F, I>(lib: &InstructionLibrary, extra: F) -> Vec<Address>
where
    F: Fn(&Action) -> I,
    I: IntoIterator<Item = Address>,
{
    root_tokens(lib.iter(), extra).into_iter().collect()
}

/// Balance-scan universe = rootfile tokens ∪ oracle-feed-registered tokens
/// (the sets don't always overlap), each tagged with its provenance so a
/// reader knows whether instructions exist for a token ("rootfile"/"both") or
/// whether swap-to-accounting-currency is its only available action
/// ("feed_only").
pub fn scan_universe(
    rootfile: &BTreeSet<Address>,
    feed: &BTreeSet<Address>,
) -> Vec<(Address, &'static str)> {
    let mut out: Vec<(Address, &'static str)> = Vec::with_capacity(rootfile.len() + feed.len());
    for t in rootfile.union(feed) {
        let registered_in = match (rootfile.contains(t), feed.contains(t)) {
            (true, true) => "both",
            (true, false) => "rootfile",
            _ => "feed_only",
        };
        out.push((*t, registered_in));
    }
    out
}

pub async fn read_balances<P: Provider<Ethereum>>(
    provider: &P,
    safe: Address,
    tokens: &[Address],
) -> Result<Vec<TokenBalance>> {
    let with_meta: Vec<(Address, TokenMeta)> = tokens.iter().map(|t| (*t, None)).collect();
    read_balances_cached(provider, safe, &with_meta).await
}

/// Like `read_balances`, but tokens carrying `Some(cached)` metadata skip the
/// on-chain symbol()/decimals() calls (they never change, so a caller that
/// has read them once can pass them back).
pub async fn read_balances_cached<P: Provider<Ethereum>>(
    provider: &P,
    safe: Address,
    tokens: &[(Address, TokenMeta)],
) -> Result<Vec<TokenBalance>> {
    read_balances_cached_at(provider, safe, tokens, None).await
}

/// Reuse the raw balance collector at one immutable observation block. Metadata
/// stays best-effort; every requested balance must succeed. `None` reads at
/// the latest block.
pub async fn read_balances_cached_at<P: Provider<Ethereum>>(
    provider: &P,
    safe: Address,
    tokens: &[(Address, TokenMeta)],
    at_block: Option<u64>,
) -> Result<Vec<TokenBalance>> {
    let mut out = Vec::with_capacity(tokens.len());
    for (token, cached) in tokens {
        out.push(match cached {
            Some((symbol, decimals)) => {
                let erc20 = IERC20::new(*token, provider);
                let call = erc20.balanceOf(safe);
                let call = match at_block {
                    Some(block) => call.block(block.into()),
                    None => call,
                };
                let bal: U256 = call.call().await.map_err(|e| {
                    crate::error::Error::rpc(format!("balanceOf({}): {}", token, e))
                })?;
                TokenBalance {
                    token: *token,
                    symbol: symbol.clone(),
                    decimals: *decimals,
                    balance: bal.to_string(),
                    balance_hex: format!("0x{:x}", bal),
                }
            }
            None => read_one(provider, safe, *token, at_block).await?,
        });
    }
    Ok(out)
}

async fn read_one<P: Provider<Ethereum>>(
    provider: &P,
    safe: Address,
    token: Address,
    at_block: Option<u64>,
) -> Result<TokenBalance> {
    let erc20 = IERC20::new(token, provider);

    let call = erc20.balanceOf(safe);
    let call = match at_block {
        Some(block) => call.block(block.into()),
        None => call,
    };
    let bal: U256 = call
        .call()
        .await
        .map_err(|e| crate::error::Error::rpc(format!("balanceOf({}): {}", token, e)))?;

    // best-effort: ignore failures for symbol/decimals (some tokens lack them)
    let symbol_call = erc20.symbol();
    let symbol_call = match at_block {
        Some(block) => symbol_call.block(block.into()),
        None => symbol_call,
    };
    let symbol = symbol_call.call().await.ok();
    let decimals_call = erc20.decimals();
    let decimals_call = match at_block {
        Some(block) => decimals_call.block(block.into()),
        None => decimals_call,
    };
    let decimals = decimals_call.call().await.ok();

    Ok(TokenBalance {
        token,
        symbol,
        decimals,
        balance: bal.to_string(),
        balance_hex: format!("0x{:x}", bal),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn strict_pinned_raw_reads_never_request_valuation_and_fail_closed() {
        use alloy::providers::ProviderBuilder;
        use axum::{Json, Router, routing::post};
        use serde_json::{Value, json};
        use std::sync::{Arc, Mutex};
        for fail_second in [false, true] {
            let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
            let capture = seen.clone();
            let app=Router::new().route("/",post(move |Json(request):Json<Value>|{
                let capture=capture.clone();async move {
                    capture.lock().unwrap().push(request.clone());
                    let index=capture.lock().unwrap().len();
                    if fail_second&&index==2 {Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"balance unavailable"}}))}
                    else {Json(json!({"jsonrpc":"2.0","id":request["id"],"result":format!("0x{:064x}",10)}))}
                }
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let provider = ProviderBuilder::new()
                .connect_http(format!("http://{addr}").parse().unwrap())
                .erased();
            let tokens = [
                (Address::with_last_byte(6), Some((None, None))),
                (
                    Address::with_last_byte(7),
                    Some((Some("TOKEN".into()), Some(6))),
                ),
            ];
            let result =
                read_balances_cached_at(&provider, Address::with_last_byte(1), &tokens, Some(7))
                    .await;
            if fail_second {
                assert!(result.is_err());
            } else {
                let rows = result.unwrap();
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].balance, "10");
                assert_eq!(rows[0].decimals, None);
            }
            let requests = seen.lock().unwrap();
            assert_eq!(requests.len(), 2);
            for request in requests.iter() {
                assert_eq!(request["method"], "eth_call");
                assert_eq!(request["params"][1], "0x7");
                let tx = &request["params"][0];
                let data = tx
                    .get("input")
                    .or_else(|| tx.get("data"))
                    .unwrap()
                    .as_str()
                    .unwrap();
                assert!(data.starts_with("0x70a08231"));
                assert!(data.ends_with("0000000000000000000000000000000000000001"));
            }
            task.abort();
        }
    }

    // A declared-only rootfile: the one affected token is the universe, and
    // reading it leaves the root unchanged.
    const DECLARED_ROOT: &str = r#"
[instructions.ops.supply.USDC]
position_id = "1200"
description = "Balance discovery"
is_debt = false
group_id = "0"
instruction_type = 0
affected_tokens = ["0xb88339cb7199b77e23db6e890353e22632ba630f", "0x0000000000000000000000000000000000000000"]
position_tokens = []
commands = ["0xa9059cbb010001ff000000ffb88339cb7199b77e23db6e890353e22632ba630f"]
state = ["0x0000000000000000000000000000000000000000000000000000000000000033", "0x0000000000000000000000000000000000000000000000000000000000000000"]
bitmap = "170141183460469231731687303715884105728"
[[instructions.ops.supply.USDC.inputs_slots]]
index = 1
name = "amount"
type = "Uint"
"#;

    #[tokio::test]
    async fn registered_tokens_are_the_declared_ones_and_extra_adds_measured_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.rootfile.toml");
        std::fs::write(&path, DECLARED_ROOT).unwrap();
        let lib = InstructionLibrary::load(&crate::config::RootfileSource::Local(path))
            .await
            .unwrap();
        let root_before = lib.root();
        let token: Address = "0xb88339cb7199b77e23db6e890353e22632ba630f"
            .parse()
            .unwrap();
        // The zero address is never a balance token.
        assert_eq!(registered_tokens(&lib), vec![token]);
        let measured = Address::with_last_byte(0x42);
        assert_eq!(
            registered_tokens_with(&lib, |_| vec![measured, Address::ZERO]),
            vec![measured, token]
        );
        assert_eq!(lib.root(), root_before);
    }

    /// `root_tokens` over any action set: the declared affected tokens plus
    /// what the caller's decoder adds for each action, zero dropped,
    /// duplicates merged, sorted.
    #[tokio::test]
    async fn root_tokens_merges_declared_and_decoded_tokens_without_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ops.rootfile.toml");
        std::fs::write(&path, DECLARED_ROOT).unwrap();
        let lib = InstructionLibrary::load(&crate::config::RootfileSource::Local(path))
            .await
            .unwrap();
        let declared: Address = "0xb88339cb7199b77e23db6e890353e22632ba630f"
            .parse()
            .unwrap();
        let decoded = Address::with_last_byte(0x01);
        let seen = std::cell::RefCell::new(Vec::new());
        let got = root_tokens(lib.iter(), |a| {
            seen.borrow_mut().push(a.qualified_name.clone());
            vec![decoded, declared, Address::ZERO]
        });
        assert_eq!(got.into_iter().collect::<Vec<_>>(), vec![decoded, declared]);
        assert_eq!(*seen.borrow(), vec!["ops.supply.USDC".to_string()]);
        // No actions: nothing, whatever the decoder would add.
        assert!(root_tokens(std::iter::empty(), |_| vec![decoded]).is_empty());
    }

    #[test]
    fn scan_universe_tags_provenance() {
        let a: Address = "0x00000000000000000000000000000000000000aa"
            .parse()
            .unwrap();
        let b: Address = "0x00000000000000000000000000000000000000bb"
            .parse()
            .unwrap();
        let c: Address = "0x00000000000000000000000000000000000000cc"
            .parse()
            .unwrap();
        let rootfile: BTreeSet<Address> = [a, b].into_iter().collect();
        let feed: BTreeSet<Address> = [b, c].into_iter().collect();
        let got = scan_universe(&rootfile, &feed);
        assert_eq!(
            got,
            vec![(a, "rootfile"), (b, "both"), (c, "feed_only")],
            "union with per-token provenance"
        );
    }
}
