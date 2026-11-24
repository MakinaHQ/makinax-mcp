//! Circle CCTP v2 — the send side.
//!
//! A [`CctpRoute`] names both deployments and the directed path; the caller
//! supplies it (from wherever it keeps verified deployment facts).
//! The encoder computes maxFee = inputAmount - minOutputAmount; the caller's
//! floor is never widened. Zero fee is a budget, not a promise that Circle
//! currently accepts or forwards a zero-fee burn. The actual fork simulation
//! and protocol response determine availability for the chosen route.
//! Claims use the captured deployment and validate recipient, amount, sender,
//! finality and fee budget before a permissionless receiveMessage call.

use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::sol;
use alloy::sol_types::SolValue;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Circle's `MessageTransmitterV2` — where the CLAIM is submitted. Same address
/// on mainnet and Base.
///
/// DERIVED, not looked up. The starting point was `TokenMessengerV2`
/// `0x28b5a0e9…cf5d`, which is the emitter a fork burn was OBSERVED to
/// produce; asking it `localMessageTransmitter()` on each chain returns this
/// address on both. Then code-verified before being written here:
///
/// ```text
///           code bytes   localDomain()   version()
/// mainnet      4353            0             1
/// base         4353            6             1
/// ```
///
/// `localDomain()` is the decisive identification — the contract states Circle's
/// own domain number for the chain it is on, so a wrong address cannot answer
/// plausibly. `version() = 1` is CCTP **v2**; v1's transmitter answers 0. This is
/// the rule (assert the address, never assume it) applied to an address that
/// would otherwise have come from documentation.
pub(crate) const MESSAGE_TRANSMITTER_V2: &str = "0x81D40F21F12A8F0E3252Bccb954D722d4c464B64";

/// `TokenMessengerV2` — burns on the source, and **emits the settlement event on
/// the destination**. Same address on both chains; observed emitting
/// `DepositForBurn` on a production burn.
pub(crate) const TOKEN_MESSENGER_V2: &str = "0x28b5a0e9C621a5BaDaA536219b3a228C8168cf5d";

sol! {
    #[sol(rpc)]
    interface IMessageTransmitterV2 {
        /// The claim. PERMISSIONLESS — `destinationCaller` is `bytes32(0)`, so
        /// anyone may submit it and the recipient is fixed in the signed message.
        /// Selector `0x57ecfd28`, confirmed against the input of a real
        /// successful claim rather than computed and hoped.
        function receiveMessage(bytes message, bytes attestation) external returns (bool);
        /// Replay protection. PROBED [M]: a certainly-unused nonce answers 0, the
        /// zero key answers 1, and a nonsense selector reverts with `0x` — so the
        /// function is real and **nonzero means ALREADY CLAIMED**. The zero key
        /// being pre-marked is a safety property, not an anomaly: a nonce we
        /// failed to parse can never read as claimable.
        function usedNonces(bytes32 nonce) external view returns (uint256);
        /// Circle's domain for the chain this contract is deployed on.
        function localDomain() external view returns (uint32);
        function version() external view returns (uint32);
    }

    /// THE settlement fact, and it is NOT the shape CCTP v1 has.
    ///
    /// MEASURED from a real inbound Base claim: v2 emits FOUR
    /// params from **TokenMessengerV2**, topic0
    /// `0x50c55e915134d457debfa58eb6f4342956f8b0616d51a89a3659360178e1ab63`.
    ///
    /// CCTP **v1** is `MintAndWithdraw(address,uint256,address)` — three params,
    /// emitted by the TokenMinter, topic0 `0x1b2a7ff0…`. That is what prior
    /// knowledge produces, and it is wrong on both the parameter count and the
    /// emitter. A resolver watching for it would never observe a settlement: every
    /// CCTP bridge would time out while the USDC sat minted in the Safe. That is
    /// why this one was read off the chain instead of remembered.
    event MintAndWithdraw(
        address indexed mintRecipient,
        uint256 amount,
        address indexed mintToken,
        uint256 feeCollected
    );
}

pub fn message_transmitter() -> Address {
    MESSAGE_TRANSMITTER_V2
        .parse()
        .expect("MESSAGE_TRANSMITTER_V2 is a literal address")
}

pub fn token_messenger() -> Address {
    TOKEN_MESSENGER_V2
        .parse()
        .expect("TOKEN_MESSENGER_V2 is a literal address")
}

/// The nonce a burn message carries, read from the MESSAGE BYTES.
///
/// Header layout, confirmed against the observed claim's calldata:
/// `version(4) | sourceDomain(4) | destinationDomain(4) | nonce(32) | …`
/// (the observed message began `00000001 00000000 00000006`, i.e. version 1,
/// mainnet → Base).
///
/// Reading it here rather than from Iris's `decodedMessage.nonce` is what makes
/// the idempotency check independent of the attestation service: the question
/// "has this already been claimed?" is then answered entirely by the destination
/// chain, and Iris cannot make us re-submit or skip a claim by being wrong.
pub fn nonce_from_message(message: &[u8]) -> Option<B256> {
    message.get(12..44).map(B256::from_slice)
}

/// The claim calldata for a `(message, attestation)` pair.
pub fn claim_calldata(message: &Bytes, attestation: &Bytes) -> Vec<u8> {
    use alloy::sol_types::SolCall;
    IMessageTransmitterV2::receiveMessageCall {
        message: message.clone(),
        attestation: attestation.clone(),
    }
    .abi_encode()
}

sol! {
    /// `BridgeOrder.extraData` for CCTP is the finality threshold and nothing
    /// else — no output token, no deadline. Matches `CctpV2Data` in makina-rs
    /// (`crates/common/src/bridges/mod.rs`).
    struct CctpV2Data {
        uint32 minFinalityThreshold;
    }
}

/// Circle's finality tiers, by the threshold the encoder passes through.
/// Serialized lowercase (`"standard"` / `"fast"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CctpFinality {
    /// Default finality. The route must support it; current deployment and
    /// fee validation decides whether a zero-fee burn succeeds.
    #[default]
    Standard,
    /// Opt-in only. Requires `maxFee >= minimumFee`, so it CANNOT be 1:1, and
    /// expiry can require re-attestation. Availability is path-specific.
    Fast,
}

impl CctpFinality {
    pub fn min_finality_threshold(self) -> u32 {
        match self {
            Self::Fast => 1_000,
            Self::Standard => 2_000,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Standard => "standard",
        }
    }
}

/// `extraData` for a CCTP burn.
pub fn encode_extra_data(finality: CctpFinality) -> Bytes {
    CctpV2Data {
        minFinalityThreshold: finality.min_finality_threshold(),
    }
    .abi_encode()
    .into()
}

#[cfg(test)]
use crate::chain::assets::usdc_for;

/// One Circle CCTP v2 deployment on one chain: the facts a burn, a claim and
/// the on-chain checks in [`super::metadata_rpc`] need.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CctpDeployment {
    /// Circle's domain number for the chain (`localDomain()`).
    pub domain: u32,
    pub usdc: Address,
    /// `TokenMessengerV2`: burns on the source, emits the mint on the
    /// destination.
    pub token_messenger: Address,
    /// `MessageTransmitterV2`: where a claim (`receiveMessage`) is submitted.
    pub message_transmitter: Address,
    /// The core registry holding the module's bridge encoders.
    pub registry: Address,
    /// The CCTP bridge encoder registered in `registry`.
    pub encoder: Address,
    /// The registry bridge id (3 for CCTP, see [`super::BridgeKind`]).
    pub bridge_id: u16,
    pub capabilities: CctpCapabilities,
}

/// Which finality tiers a deployment serves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CctpCapabilities {
    pub standard: bool,
    pub fast: bool,
}

impl CctpFinality {
    /// Whether `deployment` serves this tier.
    pub fn supported_by(self, deployment: &CctpDeployment) -> bool {
        match self {
            Self::Standard => deployment.capabilities.standard,
            Self::Fast => deployment.capabilities.fast,
        }
    }
}

/// A directed CCTP path: both deployments and the finality tier. What a CCTP
/// bridge order is built against, and what a sent transfer should record so
/// a later change of deployment facts cannot retarget its claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CctpRoute {
    pub from_chain: u64,
    pub to_chain: u64,
    pub source: CctpDeployment,
    pub destination: CctpDeployment,
    pub finality: CctpFinality,
    /// The sending module, captured with a built call (not a deployment
    /// fact).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_sender: Option<Address>,
}

/// Largest chain id accepted in a route: a JSON-safe integer.
pub(crate) const MAX_CHAIN_ID: u64 = (1 << 53) - 1;

impl CctpRoute {
    /// Structural validity: positive, distinct, JSON-safe chain ids; distinct
    /// domains; bridge id 3 and nonzero addresses on both deployments; a
    /// finality tier the source serves.
    pub fn validate(&self) -> std::result::Result<(), String> {
        for (chain, d) in [
            (self.from_chain, &self.source),
            (self.to_chain, &self.destination),
        ] {
            if chain == 0 || chain > MAX_CHAIN_ID {
                return Err(format!(
                    "CCTP route chain ID {chain} is not a positive JSON-safe integer"
                ));
            }
            if d.bridge_id != super::BridgeKind::CctpV2.id() {
                return Err(format!("chain {chain}: CCTP requires registry bridge ID 3"));
            }
            for (field, address) in [
                ("usdc", d.usdc),
                ("token_messenger", d.token_messenger),
                ("message_transmitter", d.message_transmitter),
                ("registry", d.registry),
                ("encoder", d.encoder),
            ] {
                if address.is_zero() {
                    return Err(format!("chain {chain}: {field} must be nonzero"));
                }
            }
        }
        if self.from_chain == self.to_chain {
            return Err("CCTP route requires distinct source and destination chains".into());
        }
        if self.source.domain == self.destination.domain {
            return Err(format!("duplicate CCTP domain {}", self.source.domain));
        }
        if !self.finality.supported_by(&self.source) {
            return Err("CCTP route finality is unsupported by its source deployment".into());
        }
        Ok(())
    }
}

/// What a validated CCTP send resolves to, with the path it implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctpSend {
    pub finality: CctpFinality,
    /// `inputAmount − minOutputAmount`, i.e. what the forwarder may take.
    pub max_fee: U256,
    /// `max_fee == 0`: nobody can be paid, so the sender submits
    /// `receiveMessage` and the message cannot spend a token fee; actual burn
    /// availability is checked on-chain.
    pub self_claim: bool,
}

/// The refusal [`validate_route`] gives when the order's tokens or finality
/// are not the route's. Exported so a consumer can match it exactly.
pub const ROUTE_TOKENS_OR_FINALITY_DIFFER: &str = "CCTP tokens or finality differ from the route";

/// Validate a CCTP send against its route and derive its path. Refuses
/// rather than adjusts.
pub fn validate_route(
    route: &CctpRoute,
    input_token: Address,
    output_token: Address,
    input_amount: U256,
    min_output_amount: U256,
    finality: CctpFinality,
) -> Result<CctpSend> {
    route.validate().map_err(Error::InvalidParams)?;
    if input_token != route.source.usdc
        || output_token != route.destination.usdc
        || finality != route.finality
    {
        return Err(Error::InvalidParams(ROUTE_TOKENS_OR_FINALITY_DIFFER.into()));
    }
    if input_amount.is_zero() {
        return Err(Error::InvalidParams(
            "CCTP input amount must be positive".into(),
        ));
    }
    validate_amounts(input_amount, min_output_amount, finality)
}

/// Validate a CCTP send and derive its path.
///
/// Refuses rather than adjusts. Every value here comes from the caller or from a
/// measured constant — nothing is invented, so a request that cannot be honoured
/// is an error with a reason, not a quietly-corrected order.
#[cfg(test)]
pub fn validate(
    source_chain_id: u64,
    dest_chain_id: u64,
    input_token: Address,
    input_amount: U256,
    min_output_amount: U256,
    finality: CctpFinality,
) -> Result<CctpSend> {
    // USDC only, on BOTH sides: CCTP burns USDC on the source and mints USDC on
    // the destination, so a mismatch either end cannot settle.
    let src_usdc = usdc_for(source_chain_id).ok_or_else(|| {
        Error::InvalidParams(format!(
            "CCTP: no known USDC address for source chain {source_chain_id} — refusing to \
             build a burn for a token that cannot be confirmed"
        ))
    })?;
    if input_token != src_usdc {
        return Err(Error::InvalidParams(format!(
            "CCTP moves USDC only: input_token {input_token:?} is not chain-{source_chain_id} USDC \
             ({src_usdc:?}). Use Across (bridgeId 1) for other tokens"
        )));
    }
    if usdc_for(dest_chain_id).is_none() {
        return Err(Error::InvalidParams(format!(
            "CCTP: no known USDC address for destination chain {dest_chain_id}"
        )));
    }
    validate_amounts(input_amount, min_output_amount, finality)
}

fn validate_amounts(
    input_amount: U256,
    min_output_amount: U256,
    finality: CctpFinality,
) -> Result<CctpSend> {
    if min_output_amount > input_amount {
        return Err(Error::InvalidParams(format!(
            "CCTP: min_output_amount {min_output_amount} exceeds input_amount {input_amount} — \
             maxFee would underflow"
        )));
    }
    let max_fee = input_amount - min_output_amount;
    let self_claim = max_fee.is_zero();
    // Fast requires a non-zero fee budget (`maxFee >= minimumFee`), so a 1:1
    // Fast burn is not a thing. Refuse it rather than silently promote the
    // caller to Standard or silently widen their fee budget.
    if finality == CctpFinality::Fast && self_claim {
        return Err(Error::InvalidParams(
            "CCTP Fast finality requires a non-zero fee budget (maxFee >= Circle's minimumFee), so \
             it cannot be 1:1. Either lower min_output_amount to leave a fee, or request Standard \
             finality and verify its current fee requirements"
                .into(),
        ));
    }
    Ok(CctpSend {
        finality,
        max_fee,
        self_claim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC_MAINNET: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
    const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";

    fn amt(n: u64) -> U256 {
        U256::from(n)
    }

    /// The DEFAULT path: `minOutputAmount = inputAmount` ⇒ `maxFee = 0` ⇒ 1:1,
    /// and we submit the claim.
    #[test]
    fn equal_amounts_are_a_one_to_one_self_claim() {
        let s = validate(
            8453,
            1,
            USDC_BASE.parse().unwrap(),
            amt(15_000_000),
            amt(15_000_000),
            CctpFinality::Standard,
        )
        .expect("1:1 Standard is the default path");
        assert!(s.max_fee.is_zero(), "maxFee must be 0 for 1:1");
        assert!(s.self_claim, "nobody can be paid, so we claim");
        assert_eq!(s.finality, CctpFinality::Standard);
    }

    /// A fee budget hands delivery to Circle's forwarder.
    #[test]
    fn a_fee_budget_is_the_forwarded_path() {
        let s = validate(
            8453,
            1,
            USDC_BASE.parse().unwrap(),
            amt(15_000_000),
            amt(14_213_000), // leaves ~$0.787, the measured Base→mainnet forwardFee
            CctpFinality::Standard,
        )
        .unwrap();
        assert_eq!(s.max_fee, amt(787_000));
        assert!(
            !s.self_claim,
            "a payable fee means the forwarder can deliver"
        );
    }

    /// Fast cannot be 1:1 — refused, not silently promoted to Standard and not
    /// silently given a fee budget. Both of those would change what the owner
    /// asked for without saying so.
    #[test]
    fn fast_finality_cannot_be_one_to_one() {
        let e = validate(
            8453,
            1,
            USDC_BASE.parse().unwrap(),
            amt(15_000_000),
            amt(15_000_000),
            CctpFinality::Fast,
        )
        .expect_err("Fast at maxFee=0 must refuse");
        let m = e.to_string();
        assert!(m.contains("cannot be 1:1"), "{m}");
        assert!(m.contains("Standard"), "the remedy is named: {m}");
    }

    /// USDC only, and the refusal points at the bridge that does handle it.
    #[test]
    fn a_non_usdc_token_is_refused_with_the_alternative() {
        let e = validate(
            1,
            8453,
            WETH.parse().unwrap(),
            amt(1),
            amt(1),
            CctpFinality::Standard,
        )
        .expect_err("CCTP is USDC-only");
        let m = e.to_string();
        assert!(m.contains("USDC only"), "{m}");
        assert!(m.contains("Across"), "name the bridge that can: {m}");
    }

    /// An unknown chain refuses rather than guessing a token address.
    #[test]
    fn an_unknown_chain_refuses_rather_than_guessing() {
        assert!(usdc_for(999).is_none());
        let e = validate(
            999,
            1,
            USDC_MAINNET.parse().unwrap(),
            amt(1),
            amt(1),
            CctpFinality::Standard,
        )
        .expect_err("no USDC known for chain 999");
        assert!(e.to_string().contains("no known USDC"), "{e}");
    }

    /// `minOutputAmount > inputAmount` would underflow `maxFee`.
    #[test]
    fn an_impossible_floor_is_refused_not_saturated() {
        let e = validate(
            1,
            8453,
            USDC_MAINNET.parse().unwrap(),
            amt(100),
            amt(101),
            CctpFinality::Standard,
        )
        .expect_err("underflow must refuse");
        assert!(e.to_string().contains("underflow"), "{e}");
    }

    /// `extraData` is the finality threshold and nothing else — one uint32 word,
    /// matching makina-rs's `CctpV2Data`.
    #[test]
    fn extra_data_is_only_the_finality_threshold() {
        let std_data = encode_extra_data(CctpFinality::Standard);
        assert_eq!(std_data.len(), 32, "one abi word: {std_data:?}");
        assert_eq!(
            U256::from_be_slice(&std_data),
            U256::from(2_000u32),
            "Standard = 2000"
        );
        assert_eq!(
            U256::from_be_slice(&encode_extra_data(CctpFinality::Fast)),
            U256::from(1_000u32),
            "Fast = 1000"
        );
    }
}
