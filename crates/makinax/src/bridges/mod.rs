//! Bridge support: per-bridge codecs ([`across`], [`cctp`]), the on-chain
//! checks for CCTP deployment facts ([`metadata_rpc`]), the bridge-id
//! registry ([`BridgeKind`]) and the record of a sent transfer
//! ([`PendingBridge`]).
//!
//! BINDING TRUTH: settlement is decided by destination-chain on-chain events
//! only. Explorer APIs and attestation services are advisory and must never
//! gate a valuation or an action.

pub mod across;
pub mod cctp;
pub mod metadata_rpc;

use alloy::primitives::U256;
use serde::{Deserialize, Serialize};

/// Bridge ids on the encoder registry: **1 = Across, 2 = LayerZero OFT,
/// 3 = CCTP v2**.
///
/// THE single source for this mapping. Anything that needs to name a bridgeId
/// goes through `from_id`/`protocol` — a second copy is how the wrong one
/// survives.
///
/// PROVENANCE — MEASURED on-chain, both chains. A transposed
/// mapping (2 = CCTP, 3 = LayerZero) was believed until then, with no
/// contract behind it.
///
/// The encoders are ERC1967 proxies with unverified implementations, so the
/// source cannot be read. They are still identifiable: the ABIs differ, so ask
/// which FUNCTION answers rather than which source matches.
///
///   registry() = 0xd91567aF9d2792Cb5760a73a7Fc526c7C634876a
///                (the SAME address on Base and mainnet)
///   id 1 → 0x2fb97323511E50EA6a62F2Bf30be9b254517D586  Across
///          isRouteRegistered(USDC-base, 1, USDC-eth) → true
///   id 2 → 0x44e22Ad5f637fFe6b62d6e4A0b635e87509c48fB  LayerZero
///          getLzEndpointId(1) → 30101 (LZ V2 Ethereum EID);
///          getLzEndpointId(8453) → typed revert 0xa044b96d — a real function
///          rejecting a self-lane, not a missing one
///   id 3 → 0x97430094DA8c0E37830834CF77E5D29baBb2347F  CCTP
///          getCctpDomain(1) → 0 (Circle's Ethereum domain; hard-coded, so it
///          answers even for an otherwise empty config)
///
/// Encoders are chain-identical for all three ids. `IntegrationIds.sol` and
/// makina-rs `BridgeType` agree with the measurement; they are corroboration,
/// not the source.
///
/// Separately measured the same day: `getMaxBridgeLossBps` is 0 for ids 1, 2
/// AND 3 on both chains, so every lane is closed by owner config regardless of
/// what is registered here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeKind {
    AcrossV4,
    /// Recognized; send support is a follow-up (claim-free like Across).
    LzV2,
    /// Buildable against a [`cctp::CctpRoute`]. A burn is settled by a
    /// destination `receiveMessage` claim (attestation + destination gas +
    /// cross-chain idempotency).
    CctpV2,
}

impl BridgeKind {
    /// Every kind, in id order. `every_kind_is_in_all` keeps it complete.
    pub const ALL: [Self; 3] = [Self::AcrossV4, Self::LzV2, Self::CctpV2];

    pub fn from_id(id: u16) -> Option<Self> {
        match id {
            1 => Some(Self::AcrossV4),
            2 => Some(Self::LzV2),
            3 => Some(Self::CctpV2),
            _ => None,
        }
    }

    /// The id this kind is registered under — the inverse of `from_id`, so the
    /// mapping cannot be restated anywhere else.
    pub fn id(&self) -> u16 {
        match self {
            Self::AcrossV4 => 1,
            Self::LzV2 => 2,
            Self::CctpV2 => 3,
        }
    }

    /// Human protocol name for reports and refusals. Distinct from `label`,
    /// which is a STORED discriminant (`PendingBridge.bridge`) and must not
    /// change wording.
    pub fn protocol(&self) -> &'static str {
        match self {
            Self::AcrossV4 => "Across v4",
            Self::LzV2 => "LayerZero OFT",
            Self::CctpV2 => "Circle CCTP v2",
        }
    }

    /// Name a bridgeId without asserting one that is not recognised here.
    pub fn describe_id(id: u16) -> String {
        match Self::from_id(id) {
            Some(k) => format!("bridgeId {id} ({})", k.protocol()),
            None => format!("bridgeId {id} (unrecognised — not 1/2/3)"),
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::AcrossV4 => "across_v4",
            Self::CctpV2 => "cctp_v2",
            Self::LzV2 => "lz_v2",
        }
    }
}

/// Which settlement scan could not run. A bridge has two watchers — the
/// destination fill (did the money arrive?) and the source refund (did it come
/// back?) — and "we could not look" means different things about each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackedScan {
    DestinationFill,
    SourceRefund,
}

impl TrackedScan {
    pub fn label(self) -> &'static str {
        match self {
            Self::DestinationFill => "destination fill",
            Self::SourceRefund => "source refund",
        }
    }
}

/// Why a settlement scan could not run this pass.
///
/// Fieldless on purpose: the chain it concerns rides on [`TrackingGap`] as an
/// owned `String`. An enum carrying `&'static str` would not round-trip
/// through serialization, and a caller persists this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingCause {
    /// No SpokePool shipped for the chain and none configured.
    NoSpokePool,
    /// No RPC configured (or reachable) for the chain.
    NoRpc,
    /// The chain name does not resolve to a chain id.
    UnknownChain,
    /// The send's correlation id has not been recovered from the receipt yet.
    AwaitingDepositId,
    /// A chain read failed partway through the scan.
    RpcError,
    /// Something the scan needs is missing from config or from the record
    /// itself (the sending module is no longer configured, an address does
    /// not parse). Structural: no number of retries produces it.
    MissingPrecondition,
}

impl TrackingCause {
    /// Will the next pass plausibly succeed without anyone doing anything?
    ///
    /// The distinction that matters to a reader: a transient gap is the system
    /// working through a hiccup, a STRUCTURAL one will still be there tomorrow
    /// and nobody is coming unless someone is told. Treating a single RPC blip
    /// as an incident is how a surface trains its reader to ignore it.
    pub fn is_structural(self) -> bool {
        match self {
            Self::NoSpokePool | Self::NoRpc | Self::UnknownChain | Self::MissingPrecondition => {
                true
            }
            Self::AwaitingDepositId | Self::RpcError => false,
        }
    }
}

/// A settlement scan that could not run, recorded ON the transfer's record.
///
/// `Unavailable` is a state, never a silence. A scan that never ran and says
/// nothing leaves a transfer "in flight" with nothing to say about it, and a
/// timeout clock fills the gap with a verdict. A recorded gap is the
/// difference between "watching, and it has not arrived" and "not watching".
///
/// Rendering it for a reader (why, and what to do) is the caller's: this
/// type carries the facts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingGap {
    pub scan: TrackedScan,
    pub cause: TrackingCause,
    /// The chain the scan concerns, when the cause is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
    /// When this gap was FIRST observed — not when it was last seen. A gap that
    /// persists keeps its original timestamp so the record is not rewritten on
    /// every poll tick, and so a reader can see how long the blind spot has
    /// been open.
    pub since_unix: i64,
}

impl TrackingGap {
    /// Same gap, ignoring when it started — the equality the resolver uses to
    /// decide whether anything is worth persisting.
    pub fn same_gap_as(&self, other: &TrackingGap) -> bool {
        self.scan == other.scan && self.cause == other.cause && self.chain == other.chain
    }

    pub fn is_structural(&self) -> bool {
        self.cause.is_structural()
    }
}

/// Resolve the Across SpokePool for a chain: an explicit config override wins,
/// then the shipped [`across::SPOKE_POOLS`] table.
///
/// Config first, so a caller can always name a deployment this build does
/// not ship (a new chain, a redeployment) without waiting for a release. The
/// constants are the floor, not the ceiling — which is what makes refusing an
/// unwatchable lane at send time acceptable rather than paternalistic: the
/// escape hatch is one config line and needs no code change.
///
/// Returns the CAUSE on failure rather than `None`, so no call site can decide
/// to say nothing about it.
pub fn across_spoke_pool(
    bridges: &crate::config::BridgesConfig,
    chain: &str,
) -> std::result::Result<alloy::primitives::Address, TrackingCause> {
    if let Some(a) = bridges
        .across_v4
        .as_ref()
        .and_then(|c| c.spoke_pools.get(chain))
        .copied()
    {
        return Ok(a);
    }
    let chain_id = crate::chain::chain_id_for(chain).ok_or(TrackingCause::UnknownChain)?;
    across::spoke_pool_for(chain_id).ok_or(TrackingCause::NoSpokePool)
}

/// Bridge lifecycle, as recorded on a [`PendingBridge`]. Sent/TimedOut are
/// still watched (a late fill or a refund can still arrive); Credited,
/// Refunded and Failed are terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeState {
    Sent,
    /// Not assigned by anything that observes Across or CCTP today: Across
    /// goes `Sent → Credited` on a fill and CCTP goes `Sent → Credited` on the
    /// destination mint, with no observed intermediate. Kept so records that
    /// carry it still deserialize.
    ///
    /// It is NON-TERMINAL, so [`in_flight_raw_by_token`] counts a transfer in
    /// this state as still in flight on the sending side — while the
    /// destination may already hold the funds. A caller that assigns it must
    /// account for that double count.
    Received,
    Credited,
    Refunded,
    TimedOut,
    /// Terminal: the send transaction itself never confirmed on-chain (no
    /// receipt within a trust window well past any mempool retention) — no
    /// funds ever left the Safe. Distinct from `Refunded` (funds left and
    /// came back) and `TimedOut` (funds left, deposit landed, no fill yet).
    Failed,
}

impl BridgeState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            BridgeState::Credited | BridgeState::Refunded | BridgeState::Failed
        )
    }
}

/// A sent bridge transfer: everything a destination-side settlement watcher
/// needs, in one serializable record. A caller persists it wherever it keeps
/// state (this crate keeps none).
///
/// Generic over the recorded CCTP route type `R` so a caller that records
/// richer deployment facts keeps its own type; [`cctp::CctpRoute`] is the
/// default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingBridge<R = cctp::CctpRoute> {
    /// Deployment identity captured before broadcast, so a later change of
    /// deployment facts cannot retarget the claim.
    #[serde(default = "no_route", skip_serializing_if = "Option::is_none")]
    pub cctp_route: Option<R>,
    pub bridge: String, // BridgeKind::label()
    pub bridge_id: u16,
    pub source_chain: String,
    pub dest_chain: String,
    pub dest_chain_id: u64,
    pub input_token: String,
    pub output_token: String,
    /// Raw input-token units, decimal string — the in-flight amount.
    pub input_amount: String,
    pub min_output_amount: String,
    /// Expected output at deposit time (raw output-token units, from the
    /// deposit event's outputAmount; falls back to min_output_amount when the
    /// event wasn't decodable). Baseline for the shortfall-at-credit cost.
    pub output_amount: String,
    pub recipient: String,
    /// Across depositId (decimal string) — the cross-chain correlation id.
    pub deposit_id: Option<String>,
    pub source_tx: Option<String>,
    pub sent_block: Option<u64>,
    pub sent_unix: i64,
    pub fill_deadline_unix: Option<i64>,
    pub state: BridgeState,
    pub received_tx: Option<String>,
    pub received_amount: Option<String>,
    /// sent − received at credit time, raw output-token units — the bridge
    /// COST (fee), accounted here + as a history event, never as a bleed
    /// observation.
    pub shortfall: Option<String>,
    /// Set once the timeout notification event was emitted (dedupe).
    #[serde(default)]
    pub timeout_notified: bool,
    /// Destination-chain getLogs cursor (next block to scan from). `None`
    /// until the first scan; then persisted so a restart does not redo the
    /// whole scan from `latest − chunk` every time.
    #[serde(default)]
    pub dest_scan_from: Option<u64>,
    /// Learned bounded destination log window; never a scan checkpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_scan_window: Option<u64>,
    /// When the destination scan last COMPLETED (ran to the chain head without
    /// erroring). Paired with `dest_scan_from`, this is the witness that
    /// entitles a record to assert a timeout: the cursor says a scan finished,
    /// this says when, and a timeout needs one that finished at or after the
    /// deadline it is about to declare missed.
    ///
    /// `None` on records that predate this field — which is correct rather than
    /// awkward: they genuinely have no evidence anyone looked.
    #[serde(default)]
    pub dest_scanned_unix: Option<i64>,
    /// The settlement scan that could NOT run this pass, if any.
    /// `None` means the scan ran — not that nothing went wrong unnoticed.
    #[serde(default)]
    pub tracking: Option<TrackingGap>,
    /// Source-chain refund-scan getLogs cursor (next block to scan from),
    /// chunked the same way as `dest_scan_from`. `None` until the
    /// first refund-check pass (which starts from `sent_block`); then
    /// persisted so restarts/range-limited RPCs don't redo the whole
    /// `sent_block..head` scan in one unchunked call every time.
    #[serde(default)]
    pub refund_scan_from: Option<u64>,

    // ── CCTP only. A bridge that must be CLAIMED on the destination
    //    carries state Across has no use for: Across fills itself, so it has no
    //    second transaction to track, retry, or shout about.
    /// The claim tx the sender submitted, if any. Not proof of settlement —
    /// the destination mint is. Kept so a restart does not re-submit a claim
    /// that is still in flight, and so a record can say who paid for delivery.
    #[serde(default)]
    pub claim_tx: Option<String>,
    /// When we last submitted a claim. Drives the in-flight window; a claim
    /// past it with the nonce still unspent never landed.
    #[serde(default)]
    pub claim_submitted_unix: Option<i64>,
    /// Circle's last observed attestation state (`not_observed` | `pending` |
    /// `complete`), for a reader. ADVISORY: nothing should read this to decide
    /// anything.
    #[serde(default)]
    pub attestation_status: Option<String>,
    /// Set once the unclaimed-burn escalation was emitted, so a stuck burn is
    /// loud ONCE per state rather than every poll cycle — the same dedupe
    /// discipline as `timeout_notified`. Surviving restarts is the point: a
    /// watcher that restarts mid-escalation does not re-shout.
    #[serde(default)]
    pub escalation_notified: bool,
    /// Circle's `forwardState`, verbatim. On a self-claim this is `FAILED` /
    /// `INSUFFICIENT_FEE` — the forwarder declining because it cannot be paid,
    /// which IS the 1:1 path. Stored so a reader can be told that, instead of
    /// seeing the word FAILED beside a transfer that is fine.
    #[serde(default)]
    pub forward_state: Option<String>,
    /// How the settlement mint was matched: `"recipient"` (exact — the record
    /// carried a resolved recipient) or `"sole_mint"` (the fallback net, for
    /// records written before the recipient was recorded). Kept because a report that says "the
    /// claim settled" without saying which path matched is hiding the difference
    /// between a fact and a heuristic.
    #[serde(default)]
    pub settled_by: Option<String>,
}

/// `None`, without asking `R` for a `Default`.
fn no_route<R>() -> Option<R> {
    None
}

/// In-flight virtual balance: sum of sent input-token amounts of PENDING
/// bridge transfers, per (source-side) token address. Pure — a valuation
/// prices it like any idle balance. `sent − received`: a credited/refunded
/// transfer is no longer pending and contributes nothing.
///
/// The second element names records this sum could NOT read (unparseable
/// token or amount). A corrupt record must never contribute a silent zero —
/// that understates in-flight exposure and inflates apparent headroom — so a
/// caller surfaces each named record as a warning and treats the figure as a
/// floor, not the total.
pub fn in_flight_raw_by_token<R>(
    pending_bridge_details: &[PendingBridge<R>],
) -> (
    std::collections::BTreeMap<alloy::primitives::Address, U256>,
    Vec<String>,
) {
    let mut out = std::collections::BTreeMap::new();
    let mut unreadable: Vec<String> = Vec::new();
    for p in pending_bridge_details {
        if p.state.is_terminal() {
            continue;
        }
        let Ok(token) = p.input_token.parse() else {
            unreadable.push(format!(
                "pending bridge (source tx {}): input_token `{}` does not parse — its in-flight amount is NOT counted",
                p.source_tx.as_deref().unwrap_or("unknown"),
                p.input_token
            ));
            continue;
        };
        let Ok(amt) = p.input_amount.parse::<U256>() else {
            unreadable.push(format!(
                "pending bridge (source tx {}): input_amount `{}` does not parse — its in-flight amount is NOT counted",
                p.source_tx.as_deref().unwrap_or("unknown"),
                p.input_amount
            ));
            continue;
        };
        *out.entry(token).or_insert(U256::ZERO) += amt;
    }
    (out, unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` lists every variant once, in id order. The exhaustive match
    /// stops compiling when a variant is added; every id `from_id` knows
    /// must then be in `ALL`, so a new variant cannot be left out of it.
    #[test]
    fn every_kind_is_in_all() {
        let id_order = |k: BridgeKind| match k {
            BridgeKind::AcrossV4 => 0,
            BridgeKind::LzV2 => 1,
            BridgeKind::CctpV2 => 2,
        };
        for (i, kind) in BridgeKind::ALL.iter().enumerate() {
            assert_eq!(id_order(*kind), i, "ALL out of order or duplicated");
            assert_eq!(BridgeKind::from_id(kind.id()), Some(*kind));
        }
        for id in 0..=u16::MAX {
            if let Some(kind) = BridgeKind::from_id(id) {
                assert!(BridgeKind::ALL.contains(&kind), "{kind:?} missing from ALL");
            }
        }
    }

    /// The mapping as MEASURED on-chain (see the `BridgeKind` doc comment for
    /// the encoder addresses and the probe that identified each).
    #[test]
    fn kind_registry_maps_ids() {
        assert!(matches!(BridgeKind::from_id(1), Some(BridgeKind::AcrossV4)));
        assert!(matches!(BridgeKind::from_id(2), Some(BridgeKind::LzV2)));
        assert!(matches!(BridgeKind::from_id(3), Some(BridgeKind::CctpV2)));
        assert!(BridgeKind::from_id(99).is_none());
        // `id()` is the inverse, so the mapping is stated exactly once.
        for k in [BridgeKind::AcrossV4, BridgeKind::LzV2, BridgeKind::CctpV2] {
            assert_eq!(BridgeKind::from_id(k.id()), Some(k), "{k:?} round-trips");
        }
        assert!(BridgeKind::describe_id(3).contains("CCTP"));
        assert!(BridgeKind::describe_id(2).contains("LayerZero"));
        assert!(BridgeKind::describe_id(99).contains("unrecognised"));
    }

    /// Per-protocol calldata dispatch, pinned by BEHAVIOUR — what bytes each
    /// id produces — because a transposed id would corrupt exactly that.
    #[test]
    fn each_bridge_id_produces_its_own_protocols_extra_data() {
        // CCTP (3): finality threshold ONLY — one word, no token, no deadline.
        let cctp_data = cctp::encode_extra_data(cctp::CctpFinality::Standard);
        assert_eq!(cctp_data.len(), 32);
        assert_eq!(U256::from_be_slice(&cctp_data), U256::from(2_000u32));

        // Across (1): output token + fill deadline — a DIFFERENT shape.
        let across_data = across::encode_extra_data(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
                .parse()
                .unwrap(),
            3600,
        );
        assert_ne!(
            across_data, cctp_data,
            "two protocols must not encode to the same bytes — if they did, a transposed id \
             would be undetectable at the calldata layer"
        );
        assert!(across_data.len() > cctp_data.len());
        assert_eq!(BridgeKind::from_id(2), Some(BridgeKind::LzV2));
        assert_eq!(BridgeKind::from_id(3), Some(BridgeKind::CctpV2));
    }

    fn sample_pending_bridge() -> PendingBridge {
        PendingBridge {
            bridge: BridgeKind::AcrossV4.label().to_string(),
            bridge_id: 1,
            source_chain: "base".to_string(),
            dest_chain: "arbitrum".to_string(),
            dest_chain_id: 42161,
            input_token: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            output_token: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            input_amount: "1000000000000000000".to_string(),
            min_output_amount: "990000000000000000".to_string(),
            output_amount: "995000000000000000".to_string(),
            recipient: "0x1111111111111111111111111111111111111111".to_string(),
            deposit_id: Some("42".to_string()),
            source_tx: Some("0xabc".to_string()),
            sent_block: Some(100),
            sent_unix: 1_800_000_000,
            fill_deadline_unix: Some(1_800_003_600),
            state: BridgeState::Sent,
            received_tx: None,
            received_amount: None,
            shortfall: None,
            timeout_notified: false,
            dest_scan_from: None,
            dest_scan_window: None,
            refund_scan_from: None,
            dest_scanned_unix: None,
            tracking: None,
            claim_tx: None,
            claim_submitted_unix: None,
            attestation_status: None,
            escalation_notified: false,
            forward_state: None,
            cctp_route: None,
            settled_by: None,
        }
    }

    #[test]
    fn pending_bridge_roundtrips_json_and_transitions() {
        let mut p = sample_pending_bridge();
        let s = serde_json::to_string(&p).unwrap();
        let back: PendingBridge = serde_json::from_str(&s).unwrap();
        assert_eq!(back.state, BridgeState::Sent);
        p.state = BridgeState::Credited;
        assert!(p.state.is_terminal());
        assert!(
            !BridgeState::TimedOut.is_terminal(),
            "timed_out keeps being watched"
        );
        assert!(
            BridgeState::Failed.is_terminal(),
            "failed (send never confirmed) is terminal — nothing left to watch"
        );
        assert_eq!(
            serde_json::to_value(BridgeState::Failed).unwrap(),
            serde_json::json!("failed"),
        );
    }

    #[test]
    fn in_flight_raw_by_token_sums_only_non_terminal_and_names_unreadable_records() {
        let token: alloy::primitives::Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let mut sent = sample_pending_bridge();
        sent.state = BridgeState::Sent;
        let mut credited = sample_pending_bridge();
        credited.state = BridgeState::Credited;
        let mut corrupt = sample_pending_bridge();
        corrupt.input_amount = "lots".into();
        let (out, unreadable) = in_flight_raw_by_token(&[sent, credited, corrupt]);
        assert_eq!(
            out.get(&token).copied(),
            Some(U256::from(1_000_000_000_000_000_000u128))
        );
        assert_eq!(unreadable.len(), 1, "{unreadable:?}");
        assert!(unreadable[0].contains("lots"), "{unreadable:?}");
    }

    /// Config wins over the shipped table; an unknown chain and an unshipped
    /// chain are different causes.
    #[test]
    fn the_spoke_pool_prefers_config_then_the_shipped_table() {
        let none = crate::config::BridgesConfig::default();
        assert_eq!(
            across_spoke_pool(&none, "base"),
            Ok(across::spoke_pool_for(8453).unwrap())
        );
        assert_eq!(
            across_spoke_pool(&none, "arbitrum"),
            Err(TrackingCause::NoSpokePool)
        );
        assert_eq!(
            across_spoke_pool(&none, "no-such-chain"),
            Err(TrackingCause::UnknownChain)
        );
        let configured = crate::config::BridgesConfig {
            across_v4: Some(crate::config::AcrossV4Config {
                spoke_pools: [(
                    "arbitrum".to_string(),
                    alloy::primitives::Address::repeat_byte(7),
                )]
                .into(),
            }),
            ..Default::default()
        };
        assert_eq!(
            across_spoke_pool(&configured, "arbitrum"),
            Ok(alloy::primitives::Address::repeat_byte(7))
        );
    }
}
