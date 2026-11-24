//! Across V4 (SpokePool) codec: the SpokePool addresses, extraData encoding,
//! deposit-event decode (correlation id), destination fill-event query. Event
//! shapes: the V3 address-typed events AND the V4 bytes32-typed events are
//! both declared — deposits/fills are decoded by trying both.
//!
//! VERIFIED against the live chain, which the header used to
//! only ask for: a production Base fill carries topic0
//! `0x44b559f101f8fbcc8a0ea43fa91a05a729a5ea6e14a7c75aa750374690137208`, which
//! is exactly `FilledRelay::SIGNATURE_HASH` as declared below. The binding was
//! never the problem — see `SPOKE_POOLS` for what was.

use alloy::{
    primitives::{Address, Bytes, U256},
    rpc::types::{Filter, Log},
    sol,
    sol_types::{SolEvent, SolValue},
};

sol! {
    // Across V3 generation (address-typed, uint32 depositId).
    struct V3RelayExecutionEventInfo {
        address updatedRecipient;
        bytes updatedMessage;
        uint256 updatedOutputAmount;
        uint8 fillType;
    }

    event V3FundsDeposited(
        address inputToken, address outputToken,
        uint256 inputAmount, uint256 outputAmount,
        uint256 indexed destinationChainId, uint32 indexed depositId,
        uint32 quoteTimestamp, uint32 fillDeadline, uint32 exclusivityDeadline,
        address indexed depositor, address recipient, address exclusiveRelayer,
        bytes message
    );
    event FilledV3Relay(
        address inputToken, address outputToken,
        uint256 inputAmount, uint256 outputAmount,
        uint256 repaymentChainId, uint256 indexed originChainId,
        uint32 indexed depositId, uint32 fillDeadline, uint32 exclusivityDeadline,
        address exclusiveRelayer, address indexed relayer, address depositor,
        address recipient, bytes message,
        V3RelayExecutionEventInfo relayExecutionInfo
    );

    // Across V4 generation (bytes32-typed, uint256 depositId).
    struct RelayExecutionEventInfo {
        bytes32 updatedRecipient;
        bytes32 updatedMessageHash;
        uint256 updatedOutputAmount;
        uint8 fillType;
    }

    event FundsDeposited(
        bytes32 inputToken, bytes32 outputToken,
        uint256 inputAmount, uint256 outputAmount,
        uint256 indexed destinationChainId, uint256 indexed depositId,
        uint32 quoteTimestamp, uint32 fillDeadline, uint32 exclusivityDeadline,
        bytes32 indexed depositor, bytes32 recipient, bytes32 exclusiveRelayer,
        bytes message
    );
    event FilledRelay(
        bytes32 inputToken, bytes32 outputToken,
        uint256 inputAmount, uint256 outputAmount,
        uint256 repaymentChainId, uint256 indexed originChainId,
        uint256 indexed depositId, uint32 fillDeadline, uint32 exclusivityDeadline,
        bytes32 exclusiveRelayer, bytes32 indexed relayer, bytes32 depositor,
        bytes32 recipient, bytes32 messageHash,
        RelayExecutionEventInfo relayExecutionInfo
    );
}

/// Across SpokePool addresses, per chain id — **knowledge, not configuration**,
/// the same shape as CCTP's `MessageTransmitterV2`.
///
/// A protocol address a settlement scan needs at runtime must not be
/// producible only by hand-written config: a fill scan whose SpokePool comes
/// from config alone cannot run at all on a config without it, and a timeout
/// clock running beside it will call a filled transfer unfilled.
///
/// MEASURED, not looked up:
///
/// - **base (8453) `0x09aea4…C64`** is the emitter of a production fill.
///   Queried by the scanner-equivalent filter (`originChainId = 1` and the
///   deposit's `depositId`) over a 256-block window: exactly one log, in
///   a production fill transaction. This address is not a candidate that answered
///   plausibly; it is the contract that emitted the settlement.
/// - **mainnet (1) `0x5c7BCd…5C5`** was a candidate, and is asserted rather
///   than assumed. `chainId()` → 1 and `wrappedNativeToken()` → mainnet WETH
///   (`0xC02aaA…6Cc2`) identify the deployment chain the way `localDomain()`
///   does for CCTP. `crossDomainAdmin()` → `0xc186fA914353c44b2E33eBE05f21846F1048bEda`,
///   **the same HubPool the measured Base SpokePool names** — one Across
///   system, both ends. Decisively: `numberOfDeposits()` had passed that
///   `depositId` — this is the
///   contract that issued the deposit id, not merely a SpokePool.
///
/// A chain absent here has no built-in tracker; a caller should refuse to
/// send on a lane it cannot watch, or disclose that it is not watching.
/// Config may always add one — see [`crate::bridges::across_spoke_pool`] for
/// the precedence.
pub const SPOKE_POOLS: &[(u64, &str)] = &[
    (1, "0x5c7BCd6E7De5423a257D81B442095A1a6ced35C5"),
    (8453, "0x09aea4b2242abC8bb4BB78D537A67a245A7bEC64"),
];

/// The shipped SpokePool for a chain id, if one is known.
pub fn spoke_pool_for(chain_id: u64) -> Option<Address> {
    SPOKE_POOLS
        .iter()
        .find(|(c, _)| *c == chain_id)
        .map(|(_, a)| a.parse().expect("SPOKE_POOLS holds literal addresses"))
}

/// `extraData = abi.encode(address outputToken, uint32 fillDeadlineOffset)` —
/// the exact tuple the Across encoder on the module expects.
pub fn encode_extra_data(output_token: Address, fill_deadline_offset_s: u32) -> Bytes {
    (output_token, fill_deadline_offset_s).abi_encode().into()
}

#[derive(Debug, Clone)]
pub struct DepositInfo {
    pub deposit_id: U256,
    pub dest_chain_id: u64,
    pub input_amount: U256,
    pub output_amount: U256,
    pub fill_deadline_unix: i64,
}

/// Find the Across deposit event in a receipt's logs (either generation).
pub fn decode_deposit_logs(logs: &[Log]) -> Option<DepositInfo> {
    for log in logs {
        if let Ok(ev) = log.log_decode::<V3FundsDeposited>() {
            let d = ev.inner.data;
            return Some(DepositInfo {
                deposit_id: U256::from(d.depositId),
                dest_chain_id: u64::try_from(d.destinationChainId).unwrap_or(0),
                input_amount: d.inputAmount,
                output_amount: d.outputAmount,
                fill_deadline_unix: d.fillDeadline as i64,
            });
        }
        if let Ok(ev) = log.log_decode::<FundsDeposited>() {
            let d = ev.inner.data;
            return Some(DepositInfo {
                deposit_id: d.depositId,
                dest_chain_id: u64::try_from(d.destinationChainId).unwrap_or(0),
                input_amount: d.inputAmount,
                output_amount: d.outputAmount,
                fill_deadline_unix: d.fillDeadline as i64,
            });
        }
    }
    None
}

/// Log filter for the destination SpokePool's fill events (both generations).
/// `deposit_id`/`origin_chain_id` narrow the RPC query via the indexed topics
/// shared by both event generations (numerically identical topic encoding
/// whether the underlying Solidity type is `uint32` or `uint256`); exact
/// correlation is still re-checked in `decode_fill_logs`.
pub fn fill_filter(
    spoke_pool: Address,
    deposit_id: U256,
    origin_chain_id: u64,
    from_block: u64,
) -> Filter {
    Filter::new()
        .address(spoke_pool)
        .event_signature(vec![
            FilledV3Relay::SIGNATURE_HASH,
            FilledRelay::SIGNATURE_HASH,
        ])
        .topic1(U256::from(origin_chain_id))
        .topic2(deposit_id)
        .from_block(from_block)
}

#[derive(Debug, Clone)]
pub struct FillInfo {
    pub tx_hash: Option<String>,
    pub output_amount: U256,
}

/// The fill matching OUR deposit: same depositId AND same origin chain.
pub fn decode_fill_logs(logs: &[Log], deposit_id: U256, origin_chain_id: u64) -> Option<FillInfo> {
    for log in logs {
        if let Ok(ev) = log.log_decode::<FilledV3Relay>() {
            let d = ev.inner.data;
            if U256::from(d.depositId) == deposit_id
                && d.originChainId == U256::from(origin_chain_id)
            {
                return Some(FillInfo {
                    tx_hash: log.transaction_hash.map(|h| format!("{h:?}")),
                    output_amount: d.relayExecutionInfo.updatedOutputAmount,
                });
            }
        }
        if let Ok(ev) = log.log_decode::<FilledRelay>() {
            let d = ev.inner.data;
            if d.depositId == deposit_id && d.originChainId == U256::from(origin_chain_id) {
                return Some(FillInfo {
                    tx_hash: log.transaction_hash.map(|h| format!("{h:?}")),
                    output_amount: d.relayExecutionInfo.updatedOutputAmount,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an rpc `Log` from a sol! event via its `SolEvent` log encoding
    /// (`inner: alloy::primitives::Log<LogData> { address, data }`, decoded
    /// back out with `.log_decode::<T>()`).
    fn log_from_event<E: SolEvent>(address: Address, ev: &E) -> Log {
        let data = ev.encode_log_data();
        Log {
            inner: alloy::primitives::Log { address, data },
            ..Default::default()
        }
    }

    /// The production fill this module's scan must find, pinned as bytes.
    ///
    /// Two facts, both read off the chain and neither of them
    /// re-derivable from this source tree — which is exactly why they are
    /// pinned here rather than trusted to a comment:
    ///
    /// 1. the Base SpokePool address is the **emitter** of that fill, so the
    ///    filter this module builds points at the contract that actually spoke;
    /// 2. the fill's `topic0` is `FilledRelay::SIGNATURE_HASH` as declared
    ///    above, so the event binding was correct all along.
    ///
    /// (2) is the load-bearing half. It rules out the second hypothesis for the
    /// missed settlement — a drifted ABI — and localizes the defect to the
    /// missing address alone. If someone edits the `FilledRelay` declaration,
    /// this fails naming the real fill it would stop matching,
    /// instead of the scan silently going quiet again.
    #[test]
    fn the_declared_fill_event_is_the_one_the_production_fill_emitted() {
        const OBSERVED_TOPIC0: &str =
            "0x44b559f101f8fbcc8a0ea43fa91a05a729a5ea6e14a7c75aa750374690137208";
        const OBSERVED_EMITTER: &str = "0x09aea4b2242abC8bb4BB78D537A67a245A7bEC64";

        assert_eq!(
            format!("{:?}", FilledRelay::SIGNATURE_HASH),
            OBSERVED_TOPIC0,
            "the declared FilledRelay no longer matches the observed production fill on \
             Base. A scan filtered on this signature will find nothing and the row will \
             sit unresolved — the settlement failure, one layer over."
        );
        assert_eq!(
            spoke_pool_for(8453),
            Some(OBSERVED_EMITTER.parse().unwrap()),
            "the Base SpokePool must stay the address that emitted the observed fill"
        );
        assert_eq!(
            spoke_pool_for(1),
            Some(
                "0x5c7BCd6E7De5423a257D81B442095A1a6ced35C5"
                    .parse::<Address>()
                    .unwrap()
            ),
        );
        // A chain we have NOT measured must answer None rather than guess —
        // the disclosure path depends on this being an honest absence.
        assert_eq!(spoke_pool_for(42161), None, "arbitrum was never measured");
        assert_eq!(spoke_pool_for(0), None);
    }

    #[test]
    fn extra_data_is_abi_encoded_address_u32() {
        let token: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let b = encode_extra_data(token, 3600);
        // abi.encode(address, uint32) = two 32-byte words
        assert_eq!(b.len(), 64);
        assert_eq!(&b[12..32], token.as_slice());
        assert_eq!(u32::from_be_bytes(b[60..64].try_into().unwrap()), 3600);
    }

    fn addr(byte: u8) -> Address {
        Address::repeat_byte(byte)
    }

    #[test]
    fn decodes_v3_deposit_event() {
        let ev = V3FundsDeposited {
            inputToken: addr(1),
            outputToken: addr(2),
            inputAmount: U256::from(10u64),
            outputAmount: U256::from(9u64),
            destinationChainId: U256::from(1u64),
            depositId: 42,
            quoteTimestamp: 0,
            fillDeadline: 1_800_000_000,
            exclusivityDeadline: 0,
            depositor: addr(3),
            recipient: addr(4),
            exclusiveRelayer: addr(5),
            message: Default::default(),
        };
        let log = log_from_event(addr(9), &ev);
        let info = decode_deposit_logs(&[log]).expect("decoded");
        assert_eq!(info.deposit_id, U256::from(42u64));
        assert_eq!(info.dest_chain_id, 1);
        assert_eq!(info.input_amount, U256::from(10u64));
        assert_eq!(info.output_amount, U256::from(9u64));
        assert_eq!(info.fill_deadline_unix, 1_800_000_000);
    }

    #[test]
    fn decodes_fill_only_for_matching_deposit_and_origin() {
        let ev = FilledV3Relay {
            inputToken: addr(1),
            outputToken: addr(2),
            inputAmount: U256::from(10u64),
            outputAmount: U256::from(9u64),
            repaymentChainId: U256::from(8453u64),
            originChainId: U256::from(8453u64),
            depositId: 42,
            fillDeadline: 1_800_000_000,
            exclusivityDeadline: 0,
            exclusiveRelayer: addr(5),
            relayer: addr(6),
            depositor: addr(3),
            recipient: addr(4),
            message: Default::default(),
            relayExecutionInfo: V3RelayExecutionEventInfo {
                updatedRecipient: addr(4),
                updatedMessage: Default::default(),
                updatedOutputAmount: U256::from(9u64),
                fillType: 0,
            },
        };
        let log = log_from_event(addr(9), &ev);

        // Matching deposit id + origin chain: decodes.
        let info =
            decode_fill_logs(std::slice::from_ref(&log), U256::from(42u64), 8453).expect("decoded");
        assert_eq!(info.output_amount, U256::from(9u64));

        // Wrong deposit id: no match.
        assert!(decode_fill_logs(std::slice::from_ref(&log), U256::from(43u64), 8453).is_none());

        // Wrong origin chain: no match.
        assert!(decode_fill_logs(&[log], U256::from(42u64), 1).is_none());
    }
}
