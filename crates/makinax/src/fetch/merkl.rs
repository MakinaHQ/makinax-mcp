//! Merkl rewards, as an ADVISORY input to a `MerklClaimData` harvest.
//!
//! The Merkl API (`/v4/users/{user}/rewards`) supplies what a claim on the
//! Merkl Distributor needs: per reward token, the cumulative amount and its
//! merkle proof under the current root. Nothing it says settles anything.
//! The Distributor pays `amount - claimed` against its own `getMerkleRoot()`
//! and its own `claimed(user, token)`; a stale or wrong answer makes a claim
//! revert, never pay twice. A caller compares [`MerklRewards::root`] with
//! the Distributor's root before it builds (see `reads::merkl`).
//!
//! Ported from makina-rs's Merkl client (`common/src/integrations/merkl.rs`):
//! the same response types, the same `amount > claimed` filter and the same
//! [`ClaimData::to_map`] bytes. Unlike it, this client:
//! - takes the user explicitly, keeps only rows paid to that user, and sets
//!   `users` to that user rather than echoing the API's `recipient`;
//! - returns the API's root, refusing a response whose rows disagree on it;
//! - refuses a response for another chain or with more than one entry;
//! - reads an empty list as "no rewards" (the API's answer for an address it
//!   has never seen), not as an error;
//! - surfaces HTTP 429 as [`Error::FetchRateLimited`].
//!
//! The parsing and encoding are pure and always compiled; only
//! [`fetch_rewards`] needs the `fetch-merkl` feature.

use std::collections::BTreeMap;

use alloy::{
    primitives::{Address, B256, Bytes, U256},
    sol_types::SolValue,
};
use serde::Deserialize;

use crate::{
    error::{Error, Result},
    instructions::builder::MetaFill,
};

/// Merkl's production API.
pub const MERKL_API_URL: &str = "https://api.merkl.xyz";

/// The meta type a Merkl harvest leaf declares on its claim slots.
pub const MERKL_CLAIM_META_TYPE: &str = "MerklClaimData";

/// One reward token, as the API describes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenData {
    pub address: Address,
    pub chain_id: u64,
    pub symbol: String,
    pub decimals: u64,
}

/// One campaign's share of a reward row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakdownData {
    pub reason: String,
    pub amount: U256,
    pub claimed: U256,
    pub pending: U256,
    pub campaign_id: B256,
}

/// One reward row: a token owed to `recipient`, with the proof of its
/// cumulative `amount` under `root`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewardsData {
    pub root: B256,
    pub recipient: Address,
    /// Cumulative amount ever earned: what the Distributor's `claim` takes.
    pub amount: U256,
    /// Already claimed, as the API last read it from chain.
    pub claimed: U256,
    pub pending: U256,
    pub proofs: Vec<B256>,
    pub token: TokenData,
    #[serde(default)]
    pub breakdowns: Vec<BreakdownData>,
}

impl RewardsData {
    /// What a claim of this row would pay now, per the API.
    pub fn unclaimed(&self) -> U256 {
        self.amount.saturating_sub(self.claimed)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ChainData {
    id: u64,
}

/// One entry of the API's answer: the rewards on one chain.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MerklResponse {
    chain: Option<ChainData>,
    pub rewards: Vec<RewardsData>,
}

/// The arguments of the Distributor's
/// `claim(address[] users, address[] tokens, uint256[] amounts, bytes32[][] proofs)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimData {
    pub users: Vec<Address>,
    pub tokens: Vec<Address>,
    pub amounts: Vec<U256>,
    pub proofs: Vec<Vec<B256>>,
}

impl ClaimData {
    /// Each field as a weiroll state word, keyed by its `MerklClaimData`
    /// field name: the ABI encoding without its leading offset word (length,
    /// then data). Byte for byte what makina-rs's `ClaimData::to_map` writes.
    pub fn to_map(&self) -> BTreeMap<String, Bytes> {
        fn word<T: SolValue>(v: &T) -> Bytes {
            Bytes::from(v.abi_encode().split_off(32))
        }
        BTreeMap::from([
            ("users".to_string(), word(&self.users)),
            ("tokens".to_string(), word(&self.tokens)),
            ("amounts".to_string(), word(&self.amounts)),
            ("proofs".to_string(), word(&self.proofs)),
        ])
    }

    /// [`to_map`](Self::to_map) under the leaf's meta name (its slots'
    /// `meta_type.name`), ready for `build_instruction_with_meta`.
    pub fn to_meta_fill(&self, meta_name: &str) -> MetaFill {
        MetaFill::from([(meta_name.to_string(), self.to_map())])
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// The claimable rewards of one user on one chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MerklRewards {
    pub chain_id: u64,
    pub user: Address,
    /// The root every row's proof is under; `None` when the API returned no
    /// rows at all.
    pub root: Option<B256>,
    /// Rows paid to `user` with `amount > claimed`, in API order.
    pub rewards: Vec<RewardsData>,
}

impl MerklRewards {
    /// The claim for every row, with `users` set to [`Self::user`].
    pub fn claim_data(&self) -> ClaimData {
        self.claim_data_for(|_| true)
    }

    /// The claim for the rows whose token `keep` accepts.
    pub fn claim_data_for(&self, keep: impl Fn(&TokenData) -> bool) -> ClaimData {
        let mut c = ClaimData::default();
        for r in self.rewards.iter().filter(|r| keep(&r.token)) {
            c.users.push(self.user);
            c.tokens.push(r.token.address);
            c.amounts.push(r.amount);
            c.proofs.push(r.proofs.clone());
        }
        c
    }
}

/// The rewards endpoint for one user and chain. The address is checksummed:
/// the API refuses a mixed-case address that is not.
pub fn rewards_url(base_url: &str, user: Address, chain_id: u64) -> String {
    format!(
        "{}/v4/users/{}/rewards?chainId={chain_id}",
        base_url.trim_end_matches('/'),
        user.to_checksum(None)
    )
}

fn refused(detail: impl std::fmt::Display) -> Error {
    Error::FetchRefused(format!("merkl rewards: {detail}"))
}

/// Turn one HTTP answer into [`MerklRewards`]. Pure, so every branch is
/// testable without a network.
///
/// `retry_after_s` is the response's `Retry-After`, carried on a 429.
pub fn parse_rewards(
    status: u16,
    body: &str,
    retry_after_s: Option<u64>,
    user: Address,
    chain_id: u64,
) -> Result<MerklRewards> {
    if status == 429 {
        return Err(Error::FetchRateLimited {
            service: "merkl".into(),
            retry_after_s,
        });
    }
    if !(200..300).contains(&status) {
        return Err(Error::Rpc(format!("merkl rewards: HTTP {status}")));
    }
    let entries: Vec<MerklResponse> =
        serde_json::from_str(body).map_err(|e| refused(format!("unexpected response: {e}")))?;
    let mut out = MerklRewards {
        chain_id,
        user,
        root: None,
        rewards: Vec::new(),
    };
    let entry = match entries.as_slice() {
        [] => return Ok(out),
        [one] => one,
        many => {
            return Err(refused(format!(
                "expected one entry for chain {chain_id}, got {}",
                many.len()
            )));
        }
    };
    if let Some(chain) = &entry.chain
        && chain.id != chain_id
    {
        return Err(refused(format!(
            "asked for chain {chain_id}, answered for chain {}",
            chain.id
        )));
    }
    for r in &entry.rewards {
        if r.token.chain_id != chain_id {
            return Err(refused(format!(
                "token {} is on chain {}, not {chain_id}",
                r.token.address, r.token.chain_id
            )));
        }
        match out.root {
            None => out.root = Some(r.root),
            Some(root) if root != r.root => {
                return Err(refused(format!(
                    "rows are under different roots ({root} and {})",
                    r.root
                )));
            }
            Some(_) => {}
        }
        if r.recipient == user && r.amount > r.claimed {
            out.rewards.push(r.clone());
        }
    }
    Ok(out)
}

/// Ask the Merkl API for `user`'s rewards on `chain_id`, once.
///
/// `client_name` is sent as the HTTP `User-Agent`. A transport failure or
/// a non-2xx answer is [`Error::Rpc`], a 429 is [`Error::FetchRateLimited`],
/// and a body of the wrong shape is [`Error::FetchRefused`].
#[cfg(feature = "fetch-merkl")]
pub async fn fetch_rewards(
    base_url: &str,
    user: Address,
    chain_id: u64,
    client_name: &str,
) -> Result<MerklRewards> {
    let resp = super::http_client(client_name)
        .get(rewards_url(base_url, user, chain_id))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| Error::rpc(format!("merkl rewards: {e}")))?;
    let status = resp.status().as_u16();
    let retry_after_s = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    let body = resp
        .text()
        .await
        .map_err(|e| Error::rpc(format!("merkl rewards (HTTP {status}): {e}")))?;
    parse_rewards(status, &body, retry_after_s, user, chain_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{address, b256, hex};
    use transpiler::meta_sol_types::{MerklClaimDataDef, MetaTypeDef};

    const CALIBER: Address = address!("0xd1dA9924C8eA15A8E35951b4702Aa4937A9dd70a");
    const SUMR: Address = address!("0x194f360D130F2393a5E9F3117A6a1B78aBEa1624");
    const USDC: Address = address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
    const BASE: u64 = 8453;

    fn recorded() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/merkl/rewards_base_8453.json"),
        )
        .unwrap()
    }

    fn fixture_claim() -> ClaimData {
        ClaimData {
            users: vec![CALIBER, CALIBER],
            tokens: vec![SUMR, USDC],
            amounts: vec![U256::from(65641641704373615677u128), U256::from(716109u64)],
            proofs: vec![
                vec![
                    b256!("0x6fb2cdaa1926ebddc0f4ced4dadf92b320585cb57250a69a04d84563533a823e"),
                    b256!("0x40c17767c5297d7d0ea35116b8e7c6262259e0fde40d84af930ef898d375a7d5"),
                ],
                vec![b256!(
                    "0x74f9b3ddcc6bde90f293bc01f85cb59ff45ae79e004d6e35aa4a33de091ee62f"
                )],
            ],
        }
    }

    /// The words are what makina-rs's `to_map` writes (`abi_encode()` with
    /// the first 32 bytes dropped). Expected bytes come from `cast
    /// abi-encode` with the offset word removed, independent of this crate.
    #[test]
    fn to_map_matches_the_reference_encoding() {
        let m = fixture_claim().to_map();
        let expect =
            |k: &str, h: &str| assert_eq!(m[k], Bytes::from(hex::decode(h).unwrap()), "{k}");
        assert_eq!(m.len(), 4);
        expect(
            "users",
            "0000000000000000000000000000000000000000000000000000000000000002\
             000000000000000000000000d1da9924c8ea15a8e35951b4702aa4937a9dd70a\
             000000000000000000000000d1da9924c8ea15a8e35951b4702aa4937a9dd70a",
        );
        expect(
            "tokens",
            "0000000000000000000000000000000000000000000000000000000000000002\
             000000000000000000000000194f360d130f2393a5e9f3117a6a1b78abea1624\
             000000000000000000000000833589fcd6edb6e08f4c7c32d4f71b54bda02913",
        );
        expect(
            "amounts",
            "0000000000000000000000000000000000000000000000000000000000000002\
             0000000000000000000000000000000000000000000000038ef5f55886a7fc3d\
             00000000000000000000000000000000000000000000000000000000000aed4d",
        );
        expect(
            "proofs",
            "0000000000000000000000000000000000000000000000000000000000000002\
             0000000000000000000000000000000000000000000000000000000000000040\
             00000000000000000000000000000000000000000000000000000000000000a0\
             0000000000000000000000000000000000000000000000000000000000000002\
             6fb2cdaa1926ebddc0f4ced4dadf92b320585cb57250a69a04d84563533a823e\
             40c17767c5297d7d0ea35116b8e7c6262259e0fde40d84af930ef898d375a7d5\
             0000000000000000000000000000000000000000000000000000000000000001\
             74f9b3ddcc6bde90f293bc01f85cb59ff45ae79e004d6e35aa4a33de091ee62f",
        );
    }

    /// The same words as the transpiler's own `MerklClaimData` encoder.
    #[test]
    fn to_map_matches_the_transpiler_meta_type_encoder() {
        use alloy::dyn_abi::DynSolValue;
        let c = fixture_claim();
        let arr = |v: Vec<DynSolValue>| DynSolValue::Array(v);
        let values = std::collections::HashMap::from([
            (
                "users".to_string(),
                arr(c.users.iter().map(|a| DynSolValue::Address(*a)).collect()),
            ),
            (
                "tokens".to_string(),
                arr(c.tokens.iter().map(|a| DynSolValue::Address(*a)).collect()),
            ),
            (
                "amounts".to_string(),
                arr(c
                    .amounts
                    .iter()
                    .map(|a| DynSolValue::Uint(*a, 256))
                    .collect()),
            ),
            (
                "proofs".to_string(),
                arr(c
                    .proofs
                    .iter()
                    .map(|p| arr(p.iter().map(|h| DynSolValue::FixedBytes(*h, 32)).collect()))
                    .collect()),
            ),
        ]);
        let theirs = MerklClaimDataDef.encode_values(values).unwrap();
        let ours = c.to_map();
        assert_eq!(theirs.len(), ours.len());
        for (k, v) in theirs {
            assert_eq!(ours[&k].as_ref(), v.as_slice(), "{k}");
        }
    }

    #[test]
    fn a_recorded_response_parses_and_pins_the_user() {
        let r = parse_rewards(200, &recorded(), None, CALIBER, BASE).unwrap();
        assert_eq!(
            r.root,
            Some(b256!(
                "0xcc29cd637eef263f73400a893455f92da5f064a595bb7936515531703b318e1d"
            ))
        );
        assert_eq!(r.rewards.len(), 1);
        let row = &r.rewards[0];
        assert_eq!(row.token.address, SUMR);
        assert_eq!(
            (row.token.symbol.as_str(), row.token.decimals),
            ("SUMR", 18)
        );
        assert_eq!(row.amount, U256::from(65641641704373615677u128));
        assert_eq!(row.unclaimed(), U256::from(60003219359084902u128));
        assert_eq!(row.proofs.len(), 20);
        assert_eq!(row.breakdowns.len(), 2);
        let c = r.claim_data();
        assert_eq!(c.users, vec![CALIBER]);
        assert_eq!(c.tokens, vec![SUMR]);
        assert_eq!(c.amounts, vec![row.amount]);
        assert_eq!(c.proofs, vec![row.proofs.clone()]);
        assert!(r.claim_data_for(|t| t.symbol == "USDC").is_empty());
    }

    /// Rows already claimed, or paid to someone else, are not claimed.
    #[test]
    fn claimed_and_foreign_rows_are_dropped() {
        let body = recorded();
        let other = address!("0x000000000000000000000000000000000000dEaD");
        let r = parse_rewards(200, &body, None, other, BASE).unwrap();
        assert!(r.rewards.is_empty(), "recipient is not the user");
        assert!(r.root.is_some());
        let mut v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let row = &mut v[0]["rewards"][0];
        row["claimed"] = row["amount"].clone();
        let r = parse_rewards(200, &v.to_string(), None, CALIBER, BASE).unwrap();
        assert!(r.rewards.is_empty(), "nothing left to claim");
        assert!(r.claim_data().is_empty());
    }

    #[test]
    fn an_empty_answer_is_no_rewards() {
        let r = parse_rewards(200, "[]", None, CALIBER, BASE).unwrap();
        assert_eq!((r.root, r.rewards.len()), (None, 0));
    }

    #[test]
    fn a_mismatched_shape_is_refused() {
        let body = recorded();
        let mut v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let two = serde_json::Value::Array(vec![v[0].clone(), v[0].clone()]);
        let mut other_chain = v.clone();
        other_chain[0]["chain"]["id"] = 1.into();
        let mut token_chain = v.clone();
        token_chain[0]["rewards"][0]["token"]["chainId"] = 1.into();
        let mut second = v[0]["rewards"][0].clone();
        second["root"] = format!("0x{}", "11".repeat(32)).into();
        v[0]["rewards"].as_array_mut().unwrap().push(second);
        for (what, body) in [
            ("two entries", two.to_string()),
            ("other chain", other_chain.to_string()),
            ("token chain", token_chain.to_string()),
            ("two roots", v.to_string()),
            ("not json", "<html>".into()),
            ("not a list", r#"{"rewards":[]}"#.into()),
        ] {
            match parse_rewards(200, &body, None, CALIBER, BASE) {
                Err(Error::FetchRefused(m)) => assert!(m.starts_with("merkl rewards: "), "{m}"),
                other => panic!("{what}: {other:?}"),
            }
        }
    }

    #[test]
    fn rate_limits_and_http_errors_are_typed() {
        match parse_rewards(429, "", Some(7), CALIBER, BASE) {
            Err(Error::FetchRateLimited {
                service,
                retry_after_s,
            }) => assert_eq!((service.as_str(), retry_after_s), ("merkl", Some(7))),
            other => panic!("{other:?}"),
        }
        let bad = r#"{"status":400,"detail":"Chain 999999 not supported"}"#;
        assert!(matches!(
            parse_rewards(400, bad, None, CALIBER, BASE),
            Err(Error::Rpc(m)) if m == "merkl rewards: HTTP 400"
        ));
    }

    #[test]
    fn the_url_checksums_the_user() {
        assert_eq!(
            rewards_url("https://api.merkl.xyz/", CALIBER, BASE),
            "https://api.merkl.xyz/v4/users/0xd1dA9924C8eA15A8E35951b4702Aa4937A9dd70a/rewards?chainId=8453"
        );
    }

    #[test]
    fn the_meta_fill_is_keyed_by_the_leaf_meta_name() {
        let f = fixture_claim().to_meta_fill("merkl_claim_data");
        assert_eq!(f.len(), 1);
        assert_eq!(f["merkl_claim_data"], fixture_claim().to_map());
    }
}
