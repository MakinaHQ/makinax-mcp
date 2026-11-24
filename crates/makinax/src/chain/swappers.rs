//! What the module has ACTUALLY registered under each swapperId — read, never
//! assumed.
//!
//! WHY THIS MODULE EXISTS — a live failure. A swap needs two
//! halves: a LOCAL `[safes.<m>.swappers]` line mapping the quote adapter to an
//! id, and an ON-CHAIN `setSwapperTargets` registration for that id. When the
//! local half was missing, an earlier remedy SUGGESTED a conventional id —
//! odos 1, kyber 2 — which is a guess about the chain, not a reading of it.
//! On a live mainnet Safe the Kyber router was ALREADY registered under id
//! 1, so following that remedy would have registered the same router a
//! second time under id 2 and left config and chain permanently divergent.
//! Invariant 5 says a refusal carries its remedy; a remedy that creates
//! divergence is worse than none.
//!
//! So the remedy is derived: scan the ids, find the router, name the id the
//! chain already has.
//!
//! **THE CONVENTION IS NOT A FACT.** Measured across the same
//! operator's two Safes, same module: Kyber's MetaAggregationRouterV2 is
//! registered under swapperId **1** on mainnet and swapperId **0** on Base (the
//! Base registration came from an earlier runbook, which printed id 0). Two
//! chains, one router, two ids. Any code that infers the id from the adapter
//! name is wrong on at least one of them — which is why `0` is inside the
//! scanned range and why nothing here defaults an id.

use std::collections::{BTreeMap, BTreeSet};

use alloy::primitives::{Address, address};

use crate::chain::{RootProvider, module::IMakinaXModule};

/// Highest swapperId scanned by default.
///
/// Registrations are hand-made by a Safe owner, one per aggregator; the ids
/// observed in production are 0 and 1. Ids named by config or declared some
/// other way are ALWAYS scanned on top of this range, so the bound only
/// limits discovery of ids that nothing references. `Scan::scanned` carries
/// the range into the output: a cap nobody can see is how "registered nowhere"
/// stops meaning anything.
pub const SCAN_MAX_ID: u16 = 8;

/// The stable labels a swapperId registration scan produces. Shared with
/// `crate::error::Error::SwapperNotConfigured`'s `registration` field, so the
/// producer (this scan) and every consumer of that error agree on what "not
/// registered" means without either side guessing at the other's string.
/// The quoted router is already registered — the remedy is a config line only.
pub const REG_FOUND: &str = "already_registered";
/// Every scanned id read cleanly and none holds this router.
pub const REG_ABSENT: &str = "not_registered_in_scanned_ids";
/// At least one id could not be read, so absence cannot be concluded.
pub const REG_UNKNOWN: &str = "scan_unverified";
/// No registration scan was run (e.g. a refusal raised while building a call,
/// before any chain read): the on-chain half is neither known present nor
/// known absent.
pub(crate) const REG_NOT_CHECKED: &str = "registration_not_checked";

/// One id the module answers for, with non-zero targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Registered {
    pub swapper_id: u16,
    pub approval: Address,
    pub execution: Address,
}

/// The result of reading a range of swapperIds.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    /// Ids with non-zero targets, ascending.
    pub entries: Vec<Registered>,
    /// Every id the scan attempted — the denominator.
    pub scanned: Vec<u16>,
    /// Ids whose read FAILED, with why. These are NOT empty slots: their
    /// contents are unknown, and their presence is exactly what turns
    /// "registered nowhere" into "cannot tell".
    pub unreadable: Vec<(u16, String)>,
}

/// Where a router lives, as the chain answered — including "cannot tell".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterLookup {
    /// Registered under this id. The remedy is a config line, no transaction.
    Registered(u16),
    /// Read cleanly, present nowhere. `free_id` is a genuinely unused id.
    Absent { free_id: u16 },
    /// Some id could not be read. A failed read is not permission to act:
    /// suggesting a registration here is how a duplicate gets created.
    Unknown { unreadable: Vec<u16> },
    /// Every measured nonzero slot is occupied; an unscanned id is not free.
    NoFreeSlot,
}

impl RouterLookup {
    /// The stable label producers and consumers agree on (`REG_*`).
    pub fn label(&self) -> &'static str {
        match self {
            RouterLookup::Registered(_) => REG_FOUND,
            RouterLookup::Absent { .. } => REG_ABSENT,
            RouterLookup::Unknown { .. } | RouterLookup::NoFreeSlot => REG_UNKNOWN,
        }
    }
}

/// Read `getSwapperTargets` for `0..=SCAN_MAX_ID` plus every id in `extra_ids`.
///
/// Never returns `Err`: a dead provider yields a Scan where every id is
/// `unreadable`, which `lookup` reports as `Unknown`. Collapsing that into an
/// error would have callers fall back to a convention, and the convention is
/// what this check removed.
///
/// Cost is one `eth_call` per id, on paths that are already terminal (a refusal)
/// or already read-heavy (a full setup verification). It is not on any hot path.
pub async fn scan(provider: &RootProvider, module_addr: Address, extra_ids: &[u16]) -> Scan {
    let module = IMakinaXModule::new(module_addr, provider);
    let mut ids: BTreeSet<u16> = (0..=SCAN_MAX_ID).collect();
    ids.extend(extra_ids.iter().copied());
    let mut out = Scan {
        scanned: ids.iter().copied().collect(),
        ..Default::default()
    };
    for id in ids {
        match module.getSwapperTargets(id).call().await {
            Ok(t) if t.approvalTarget != Address::ZERO || t.executionTarget != Address::ZERO => {
                out.entries.push(Registered {
                    swapper_id: id,
                    approval: t.approvalTarget,
                    execution: t.executionTarget,
                });
            }
            Ok(_) => {}
            // A getter that REVERTS on an unset id is indistinguishable here
            // from an RPC flake, and neither is evidence of an empty slot.
            // Recorded, not silently dropped.
            Err(e) => out
                .unreadable
                .push((id, crate::util::redact::text(&e.to_string()))),
        }
    }
    out
}

/// The scan a caller with NO provider gets: every id unreadable, for one stated
/// reason. Not an empty module — "we could not look" and "there is nothing
/// there" are the two answers this whole module exists to keep apart.
pub fn all_unreadable(reason: String) -> Scan {
    let ids: Vec<u16> = (0..=SCAN_MAX_ID).collect();
    Scan {
        entries: Vec::new(),
        unreadable: ids.iter().map(|id| (*id, reason.clone())).collect(),
        scanned: ids,
    }
}

impl Scan {
    /// Which id holds `router` — or why we cannot say.
    ///
    /// Matches on the EXECUTION target: that is the address the module calls
    /// with aggregator calldata, and the address a quote reports as its router.
    /// For both supported aggregators approval == execution; a registration
    /// where they differ still matches on execution and the mismatch is visible
    /// in `entries`.
    pub fn lookup(&self, router: Address) -> RouterLookup {
        if let Some(e) = self.entries.iter().find(|e| e.execution == router) {
            return RouterLookup::Registered(e.swapper_id);
        }
        if !self.unreadable.is_empty() {
            return RouterLookup::Unknown {
                unreadable: self.unreadable.iter().map(|(id, _)| *id).collect(),
            };
        }
        match self.first_free_id() {
            Some(free_id) => RouterLookup::Absent { free_id },
            None => RouterLookup::NoFreeSlot,
        }
    }

    /// The lowest id ≥ 1 that is neither registered nor unreadable.
    ///
    /// Skips 0 deliberately. Zero is a LEGAL id — Base's Kyber lives there — but
    /// a fresh registration should not start at the one value that also reads as
    /// "unset" to every human and every getter; the established convention and
    /// both runbooks start at 1. An id already in use is never suggested, which
    /// is the defect this whole module exists to remove.
    pub fn first_free_id(&self) -> Option<u16> {
        let taken: BTreeSet<u16> = self
            .entries
            .iter()
            .map(|e| e.swapper_id)
            .chain(self.unreadable.iter().map(|(id, _)| *id))
            .collect();
        self.scanned
            .iter()
            .copied()
            .filter(|id| *id > 0)
            .find(|id| !taken.contains(id))
    }

    /// The denominator, in one phrase for a human-facing remedy.
    ///
    /// Neutral wording; a consumer may substitute its own.
    pub fn range_note(&self) -> String {
        let read = self.scanned.len() - self.unreadable.len();
        let lo = self.scanned.first().copied().unwrap_or(0);
        let hi = self.scanned.last().copied().unwrap_or(0);
        format!(
            "read {read} of {} swapperIds on-chain ({lo}..={hi}, plus every id named by config or declared some other way)",
            self.scanned.len()
        )
    }

    /// `id -> execution target`, for reports.
    pub fn by_id(&self) -> BTreeMap<u16, Address> {
        self.entries
            .iter()
            .map(|e| (e.swapper_id, e.execution))
            .collect()
    }
}

/// KyberSwap's MetaAggregationRouterV2.
///
/// Same address on mainnet and Base [measured: registered on both of the
/// operator's modules; corroborated for chain 1 and 8453 by an independent
/// declared source; asserted against live Kyber responses by this crate's own
/// quote fixtures].
pub const KYBER_ROUTER: Address = address!("0x6131B5fae19EA4f9D964eAc0408E4408b66337b5");

/// Name the quote adapter that routes through `router` — **LABELLING ONLY**.
///
/// The quote adapters resolve their router PER QUOTE (Kyber returns
/// `routerAddress`, Odos returns `transaction.to`) and the swap path compares the
/// registered targets against that live value. This table never enters that
/// comparison, never builds a call and never gates a swap; it exists so a config
/// remedy can say `kyber = 1` instead of `<adapter> = 1`.
///
/// Odos is deliberately absent: it deploys a router per chain, so there is no
/// address to hard-code. An unrecognised router returns `None`, and the remedy
/// then says which fact it is missing instead of guessing a name.
pub fn adapter_for_router(router: Address) -> Option<&'static str> {
    (router == KYBER_ROUTER).then_some("kyber")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::{Provider, ProviderBuilder, mock::Asserter};

    /// Two ABI words: (approvalTarget, executionTarget).
    fn targets(approval: Address, execution: Address) -> alloy::primitives::Bytes {
        let mut b = vec![0u8; 64];
        b[12..32].copy_from_slice(approval.as_slice());
        b[44..64].copy_from_slice(execution.as_slice());
        alloy::primitives::Bytes::from(b)
    }

    fn zero_targets() -> alloy::primitives::Bytes {
        alloy::primitives::Bytes::from(vec![0u8; 64])
    }

    fn mocked(a: &Asserter) -> RootProvider {
        ProviderBuilder::new()
            .connect_mocked_client(a.clone())
            .erased()
    }

    const MODULE: Address = address!("0x5AFe00000000000000000000000000000000B003");

    /// THE field case, as a live mainnet Safe actually is: the Kyber router
    /// is registered under id 1 and nothing local maps it. The lookup must name
    /// id 1 — the id the CHAIN has — and never a free id.
    #[tokio::test]
    async fn a_registered_router_is_found_at_the_id_the_chain_has() {
        let a = Asserter::new();
        a.push_success(&zero_targets()); // id 0
        a.push_success(&targets(KYBER_ROUTER, KYBER_ROUTER)); // id 1
        for _ in 2..=SCAN_MAX_ID {
            a.push_success(&zero_targets());
        }
        let s = scan(&mocked(&a), MODULE, &[]).await;
        assert_eq!(s.lookup(KYBER_ROUTER), RouterLookup::Registered(1));
        assert_eq!(s.lookup(KYBER_ROUTER).label(), REG_FOUND);
        assert!(s.unreadable.is_empty());
        assert_eq!(s.by_id(), BTreeMap::from([(1u16, KYBER_ROUTER)]));
    }

    /// Base's registration lives at id 0. A scan that started at 1 would report
    /// the router absent and produce the duplicate this module exists to stop.
    #[tokio::test]
    async fn id_zero_is_inside_the_range() {
        let a = Asserter::new();
        a.push_success(&targets(KYBER_ROUTER, KYBER_ROUTER)); // id 0 — Base
        for _ in 1..=SCAN_MAX_ID {
            a.push_success(&zero_targets());
        }
        let s = scan(&mocked(&a), MODULE, &[]).await;
        assert_eq!(s.lookup(KYBER_ROUTER), RouterLookup::Registered(0));
    }

    /// Nothing registered: the suggestion is a genuinely free id, and it is not
    /// 0 even though 0 is free.
    #[tokio::test]
    async fn an_empty_module_yields_a_free_id_starting_at_one() {
        let a = Asserter::new();
        for _ in 0..=SCAN_MAX_ID {
            a.push_success(&zero_targets());
        }
        let s = scan(&mocked(&a), MODULE, &[]).await;
        assert_eq!(s.lookup(KYBER_ROUTER), RouterLookup::Absent { free_id: 1 });
        assert_eq!(s.lookup(KYBER_ROUTER).label(), REG_ABSENT);
    }

    /// A free id steps over ids that ARE in use — including ones only config
    /// knows about, which is why `extra_ids` is scanned on top of the range.
    #[tokio::test]
    async fn a_free_id_steps_over_everything_in_use() {
        let other = address!("0x00000000000000000000000000000000000000ee");
        let a = Asserter::new();
        // ids 0..=8 then the extra id 40 (BTreeSet order).
        a.push_success(&zero_targets()); // 0
        a.push_success(&targets(other, other)); // 1
        a.push_success(&targets(other, other)); // 2
        for _ in 3..=SCAN_MAX_ID {
            a.push_success(&zero_targets());
        }
        a.push_success(&targets(other, other)); // 40, from config
        let s = scan(&mocked(&a), MODULE, &[40]).await;
        assert_eq!(s.lookup(KYBER_ROUTER), RouterLookup::Absent { free_id: 3 });
        assert!(s.scanned.contains(&40), "config ids are always scanned");
        assert_eq!(s.lookup(other), RouterLookup::Registered(1));
    }

    /// The honesty case. One unreadable id and absence is no longer a
    /// conclusion — `Unknown`, never `Absent`, so no caller can be handed a
    /// registration derived from a read that did not happen.
    #[tokio::test]
    async fn one_failed_read_forbids_concluding_absence() {
        let a = Asserter::new();
        a.push_success(&zero_targets()); // 0
        a.push_failure_msg("execution reverted"); // 1
        for _ in 2..=SCAN_MAX_ID {
            a.push_success(&zero_targets());
        }
        let s = scan(&mocked(&a), MODULE, &[]).await;
        assert_eq!(
            s.lookup(KYBER_ROUTER),
            RouterLookup::Unknown {
                unreadable: vec![1]
            }
        );
        assert_eq!(s.lookup(KYBER_ROUTER).label(), REG_UNKNOWN);
        assert!(s.range_note().contains("read 8 of 9"), "{}", s.range_note());
    }

    /// A dead provider is the same answer, not a panic and not an empty module.
    #[tokio::test]
    async fn a_dead_provider_is_unknown_not_empty() {
        let provider =
            crate::chain::provider_from_endpoints(&["http://127.0.0.1:1".into()], "dead-rpc-test")
                .expect("endpoint parses");
        let s = scan(&provider, MODULE, &[]).await;
        assert_eq!(s.entries.len(), 0);
        assert_eq!(s.unreadable.len(), s.scanned.len());
        assert!(matches!(
            s.lookup(KYBER_ROUTER),
            RouterLookup::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn occupied_scan_never_suggests_an_unread_slot() {
        let a = Asserter::new();
        for _ in 0..=SCAN_MAX_ID {
            a.push_success(&targets(Address::repeat_byte(7), Address::repeat_byte(7)));
        }
        let s = scan(&mocked(&a), MODULE, &[]).await;
        assert_eq!(s.first_free_id(), None);
        assert_eq!(s.lookup(KYBER_ROUTER), RouterLookup::NoFreeSlot);
        assert!(!s.scanned.contains(&9));
    }

    /// The label table names what it knows and refuses what it does not.
    #[test]
    fn router_labelling_names_kyber_and_nothing_it_cannot_prove() {
        assert_eq!(adapter_for_router(KYBER_ROUTER), Some("kyber"));
        // Odos' router is per-chain: no address, no name, no guess.
        assert_eq!(
            adapter_for_router(address!("0xCf5540fFFCdC3d510B18bFcA6d2b9987b0772559")),
            None
        );
        assert_eq!(adapter_for_router(Address::ZERO), None);
    }
}
