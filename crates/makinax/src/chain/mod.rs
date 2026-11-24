//! Read, build, simulate and send calls to a makinaX module: on-chain
//! bindings, provider construction, a hedged transport for multi-endpoint
//! failover, revert-decoding, the swapperId registration scan, and (behind
//! `write`) signer construction.

pub mod assets;
pub mod hedge;

/// How far behind the wall clock a block may be and still be called current,
/// and how far ahead before it is nonsense.
///
/// House figure: a caller that already refuses a source block older than
/// sixty seconds, with five seconds of tolerance for a clock ahead.
pub(crate) const HEAD_STALE_AFTER_S: u64 = 60;
pub(crate) const HEAD_AHEAD_TOLERANCE_S: u64 = 5;

/// Is this block head RECENT — not stale beyond the bound — and therefore fit
/// to pin a decision to?
///
/// The name says "recent" rather than "current" on purpose: current is what a
/// reader would want it to mean and is precisely what it cannot establish. A
/// reader greps the name, not the comment below it, and the name is what
/// survives into the call sites.
///
/// Pinning two reads to one block makes them mutually consistent, which
/// removes the split where the owners come from one height and the
/// threshold from another. It does NOT establish that the height is current
/// — and the height itself arrives through the same hedged transport, first
/// success wins, so a lagging endpoint that answers instantly names the
/// block and every endpoint then serves that past faithfully. The reads
/// agree, the answer is internally consistent, and it describes a Safe as it
/// was.
///
/// The property the pin does carry — that an endpoint without the block
/// errors rather than answering at its own head — only protects against
/// pinning FORWARD. It does nothing when the pin came from the lagging node.
/// So the head is judged against the clock before anything is pinned to it.
///
/// This rests on one assumption worth naming: a node asked for a height
/// above its own head errors. A provider that instead answers at its own
/// head would defeat the pin even with this bound, because the pinned read
/// would come back silently from the wrong height.
///
/// WHAT THIS DOES NOT CLOSE, and why that is not a dead end. A head inside
/// the window can still be behind the true head, so a decision can still be
/// taken on a chain state up to this many seconds old. That residual is a
/// consequence of taking the height from ONE racing read — it is not an
/// inherent limit of working under a hedge, and a reader who concludes the
/// true head cannot be established will stop looking, which is the opposite
/// of what this comment is for. Two mechanisms shrink it well below any time
/// bound, both built from things already in this tree:
///
/// 1. Remember the highest head this process has seen for the chain and
///    refuse one below it. No laggard can fool that, because a higher block
///    has already been proven to exist. It is session state, so a fresh
///    process starts with no floor — exactly at the start of its first
///    read — and persisting it would refuse a process pointed at a fork restarted
///    lower, which test rigs do, so it has to be keyed by chain and tolerate
///    a deliberate reset. BUILT: [`head_floor_admits`], reset by
///    [`reset_head_floor`] where a fork is started for a chain.
/// 2. Fan the height read out the way the receipt path already fans out and
///    take the MAXIMUM rather than the first answer, which removes the
///    laggard winning the race at all. That turns one endpoint reporting a
///    wild number from something the hedge routes around into something
///    that poisons every read, so it needs outlier rejection.
///
/// Both are design rather than lines, and both are a caller's own next step
/// beyond what is built here. This bound is the floor under them, not a
/// substitute for them.
///
/// THE WINDOW, STATED RATHER THAN ASSUMED AWAY. A head inside this window
/// can be a few blocks behind the tip, so ANY change made in the last
/// [`HEAD_STALE_AFTER_S`] seconds can still be invisible: measured at tip 200
/// with an owner removed at 195, an endpoint at 190 is about twenty seconds
/// behind and passes. That is exactly the window a person acts in — adding a
/// new owner to the Safe, then continuing — so this is a deliberate
/// tolerance, not an elimination. Zero tolerance is not available; naming
/// the tolerance is.
///
/// One asymmetry follows and is worth acting on: READING and ADOPTING need
/// not share a threshold. A slightly stale read is fine for showing someone
/// a status. It is not fine for writing a receipt that later steps trust,
/// which is a one-way door. Where a caller is about to make something
/// durable, it wants corroboration that the height is the tip rather than
/// merely recent — and note that for adoption the fan-out above fails in the
/// SAFE direction, since a wild-high head makes the pinned read error and
/// the adoption refuse.
pub fn head_is_recent(block_timestamp: u64, now: u64) -> bool {
    block_timestamp <= now + HEAD_AHEAD_TOLERANCE_S
        && now.saturating_sub(block_timestamp) <= HEAD_STALE_AFTER_S
}

/// Mechanism 1 of the two named above: the highest head this process has
/// seen for each chain. A head BELOW a height that has already been proven
/// to exist cannot be the tip, whatever its own timestamp says — so this
/// catches the laggard that [`head_is_recent`] lets through, and it does not
/// depend on a clock, a tolerance or a number of seconds.
///
/// Session state on purpose. It is not persisted, because a persisted floor
/// would refuse a process pointed at a fork restarted lower — which a test
/// rig does every run — and a floor that survives into an unrelated
/// topology is a refusal nobody can clear. A fresh process therefore starts
/// with no floor, which is exactly at the start of a session, so this
/// NARROWS the window rather than closing it: the first read of a session
/// has nothing to be judged against. Mechanism 2 (fan the height read out
/// and take the maximum) is what closes the first read, and it is a
/// caller's own next step.
static HEAD_FLOOR: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
    std::sync::LazyLock::new(Default::default);

/// Record `number` as seen for `chain`, and say whether it may be trusted as a
/// head. `Err(floor)` means a higher block has already been observed on this
/// chain, so this answer describes the past.
pub fn head_floor_admits(chain: &str, number: u64) -> std::result::Result<(), u64> {
    let mut floor = match HEAD_FLOOR.lock() {
        Ok(floor) => floor,
        // A poisoned lock is a panic in another thread, not evidence about the
        // chain. Admit rather than refuse every read for the rest of the
        // process: this is a narrowing, and it must not become an outage.
        Err(poisoned) => poisoned.into_inner(),
    };
    match floor.get(chain) {
        Some(&seen) if number < seen => Err(seen),
        _ => {
            floor.insert(chain.to_owned(), number);
            Ok(())
        }
    }
}

/// The height every postcondition read should be pinned to, or the plain
/// sentence saying why this chain cannot be read right now.
///
/// "Is the module enabled on this Safe?" is compared against a FIXED value —
/// `true` — so unlike the owner set there is nothing recorded for a stale
/// answer to disagree with. A `true` from a node that has not yet seen the
/// module disabled satisfies every check there is, and no comparison can catch
/// it. The only thing that bounds it is the HEIGHT it was read at, which is
/// why this returns a block rather than a verdict: the caller pins its reads
/// to it, so the answers are one observation of one moment, and that moment
/// has a bounded age.
///
/// It deliberately does NOT apply the head floor. The floor is worth its blast
/// radius where a durable receipt is written from the answer — one chain key,
/// once, at handover — and these run on every finalize and every repair. A
/// chain key is not a topology: the same key names the live chain and a fork
/// of it, and a process that has seen both would refuse the lower one for the
/// rest of its life. What bounds THIS is the pin plus the clock.
/// `idle_is_fine` turns the clock check off for an endpoint that is not on the
/// public network — an anvil fork, which only advances its timestamp when it
/// MINES. A fork left alone for a minute is not an endpoint that is behind;
/// it is a chain that produced no blocks, and refusing to finalize on it would
/// break the fork rigs and every hands-on session on one. The danger this
/// bound is for is a LAGGING PUBLIC ENDPOINT, and a loopback RPC is not one.
/// Why [`recent_head`] refused, structured so a consumer renders its OWN
/// remedy text (naming its own settings surface, its own vocabulary) rather
/// than being handed a pre-rendered sentence it cannot recover its exact
/// wording from. [`std::fmt::Display`] still gives a neutral rendering for a
/// caller that doesn't need to customize it.
#[derive(Debug, Clone)]
pub enum RecentHeadError {
    /// The current block could not be read at all.
    Unreadable(String),
    /// Read cleanly, but older than the staleness bound. Carries the block
    /// number that was too old, so a consumer's remedy can name it.
    Behind { head_block: u64 },
}

impl std::fmt::Display for RecentHeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecentHeadError::Unreadable(e) => {
                write!(f, "the current block could not be read: {e}")
            }
            RecentHeadError::Behind { head_block } => write!(
                f,
                "the endpoint answering for this chain is behind: its latest block ({head_block}) is too old to judge this Safe's current state. Read again, or change the configured endpoint"
            ),
        }
    }
}

impl std::error::Error for RecentHeadError {}

pub async fn recent_head(
    provider: &provider::RootProvider,
    idle_is_fine: bool,
) -> std::result::Result<u64, RecentHeadError> {
    use alloy::providers::Provider as _;
    let head = provider
        .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest)
        .await
        .map_err(|e| RecentHeadError::Unreadable(crate::util::redact::text(&e.to_string())))?
        .ok_or_else(|| {
            RecentHeadError::Unreadable("the current block could not be read".to_string())
        })?;
    if !idle_is_fine && !head_is_recent(head.header.timestamp, clock_now_s()) {
        return Err(RecentHeadError::Behind {
            head_block: head.header.number,
        });
    }
    Ok(head.header.number)
}

/// Forget the floor for one chain. Called where a caller points at a
/// different topology for it — a fork restarted from a lower height is a
/// legitimate step backwards, and the only one.
pub fn reset_head_floor(chain: &str) {
    match HEAD_FLOOR.lock() {
        Ok(mut floor) => floor.remove(chain),
        Err(poisoned) => poisoned.into_inner().remove(chain),
    };
}

/// Is this endpoint a local one — an anvil fork or a dev node — rather than a
/// public RPC? Used to decide whether an idle chain counts as a stale one; see
/// [`recent_head`]. Host-only: a loopback host is local whatever the port or
/// the path.
pub fn endpoint_is_local(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or_else(|| rest.split(['/', '?', '#']).next().unwrap_or(""));
    host_name_is_local(host)
}

fn host_name_is_local(host: &str) -> bool {
    let name = match host.rsplit_once(':') {
        Some((head, port))
            if !head.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => host,
    };
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1" | "[::1]" | "0.0.0.0"
    )
}

/// Wall clock in unix seconds, for [`head_is_recent`].
pub fn clock_now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub mod module;
pub mod provider;
#[cfg(feature = "write")]
pub mod signer;
pub mod swappers;
pub mod taxonomy;

pub use provider::{ProviderMode, Providers, RootProvider, provider_from_endpoints};

/// EIP-155 chain id for a config chain key ("mainnet", "base", …).
///
/// Resolved through the verified [`crate::chains`] table first, then through
/// `alloy-chains` so any chain the wider ecosystem names, this crate names
/// the same way.
pub fn chain_id_for(chain: &str) -> Option<u64> {
    use std::str::FromStr;
    if let Some(network) = crate::chains::network_by_key(chain) {
        return Some(network.chain_id);
    }
    // "mainnet" is this crate's name for Ethereum; alloy-chains calls it that
    // too, but accept "ethereum" as a courtesy.
    let lowered = chain.to_ascii_lowercase();
    let key = if lowered == "ethereum" {
        "mainnet"
    } else {
        lowered.as_str()
    };
    alloy_chains::Chain::from_str(key).ok().map(|c| c.id())
}

/// A chain filter that accepts 8453, `"8453"`, or `"base"`.
pub fn parse_chain_filter(v: &serde_json::Value) -> Result<Option<u64>, String> {
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| format!("chain id `{n}` is not a u64"))
            .map(Some),
        serde_json::Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if let Ok(id) = t.parse::<u64>() {
                return Ok(Some(id));
            }
            chain_id_for(t).map(Some).ok_or_else(|| {
                format!("unknown chain `{t}` — use a name (base) or a chain id (8453)")
            })
        }
        other => Err(format!(
            "chain must be a name, a number, or a numeric string, not {other}"
        )),
    }
}

#[cfg(test)]
mod chain_id_tests {
    use super::{
        HEAD_AHEAD_TOLERANCE_S, HEAD_STALE_AFTER_S, chain_id_for, head_is_recent,
        parse_chain_filter,
    };

    #[test]
    fn resolves_the_chains_this_crate_verifies_and_falls_back_to_alloy_chains() {
        assert_eq!(chain_id_for("mainnet"), Some(1));
        assert_eq!(chain_id_for("ethereum"), Some(1));
        assert_eq!(chain_id_for("base"), Some(8453));
        assert_eq!(chain_id_for("arbitrum"), Some(42161));
        assert_eq!(chain_id_for("optimism"), Some(10));
        assert_eq!(chain_id_for("not-a-chain"), None);
    }

    #[test]
    fn a_chain_filter_accepts_name_or_id() {
        assert_eq!(
            parse_chain_filter(&serde_json::json!(8453)).unwrap(),
            Some(8453)
        );
        assert_eq!(
            parse_chain_filter(&serde_json::json!("8453")).unwrap(),
            Some(8453)
        );
        assert_eq!(
            parse_chain_filter(&serde_json::json!("base")).unwrap(),
            Some(8453)
        );
        assert_eq!(
            parse_chain_filter(&serde_json::json!("BASE")).unwrap(),
            Some(8453)
        );
        assert_eq!(parse_chain_filter(&serde_json::json!(null)).unwrap(), None);
        assert!(parse_chain_filter(&serde_json::json!("not-a-chain")).is_err());
    }

    /// The bound has to refuse the stale head AND accept the ordinary one:
    /// the over-broad version of this fix — refusing whenever anything
    /// about the head looks unusual — passes every failing case and breaks
    /// every real Safe, so the control arm is the test.
    #[test]
    fn a_head_is_judged_by_the_clock_and_an_ordinary_one_still_passes() {
        let now = 1_700_000_000;
        // Control: a head from this moment, and one a few seconds old, are
        // exactly what a healthy endpoint returns.
        assert!(head_is_recent(now, now));
        assert!(head_is_recent(now - 3, now));
        assert!(
            head_is_recent(now - HEAD_STALE_AFTER_S, now),
            "the edge is inclusive"
        );
        // A lagging endpoint naming its own block, which is the whole defect.
        assert!(!head_is_recent(now - HEAD_STALE_AFTER_S - 1, now));
        assert!(!head_is_recent(now - 3_600, now));
        // A clock ahead by a little is ordinary; by a lot is nonsense.
        assert!(head_is_recent(now + HEAD_AHEAD_TOLERANCE_S, now));
        assert!(!head_is_recent(now + HEAD_AHEAD_TOLERANCE_S + 1, now));
        // A zero timestamp is not a fresh head at the epoch.
        assert!(!head_is_recent(0, now));
    }
}

#[cfg(test)]
mod head_floor_tests {
    use super::{head_floor_admits, head_is_recent, reset_head_floor};

    // Each test owns its chain key: the floor is process-wide session state,
    // so a shared key would make tests contend.
    #[test]
    fn a_height_already_proven_to_exist_is_the_floor_until_a_fork_is_re_anchored() {
        let chain = "test-floor-basic";
        assert!(head_floor_admits(chain, 100).is_ok());
        // Forward and level are both fine: the floor is a floor, not a ratchet
        // that demands progress.
        assert!(head_floor_admits(chain, 105).is_ok());
        assert!(head_floor_admits(chain, 105).is_ok());
        // Backwards is not. The block that was seen is named, so the refusal
        // can say what it is contradicting.
        assert_eq!(head_floor_admits(chain, 104), Err(105));
        assert_eq!(head_floor_admits(chain, 1), Err(105));
        // A refused height must NOT become the new floor, or one laggard
        // would lower the bar for the next one.
        assert_eq!(head_floor_admits(chain, 104), Err(105));

        // The one legitimate step backwards, and it has to be deliberate.
        reset_head_floor(chain);
        assert!(head_floor_admits(chain, 1).is_ok());
        assert_eq!(head_floor_admits(chain, 0), Err(1));
    }

    #[test]
    fn chains_do_not_share_a_floor() {
        assert!(head_floor_admits("test-floor-a", 500).is_ok());
        assert!(
            head_floor_admits("test-floor-b", 3).is_ok(),
            "a height seen on one chain must not refuse another chain, which \
             may legitimately be at any height of its own"
        );
    }

    // The clock and the floor answer different questions, and the floor exists
    // because this one is not enough: a head can be minutes-fresh and still be
    // behind the tip by the seconds in which an owner was removed.
    // An anvil fork only advances its timestamp when it mines, so "idle" and
    // "behind" look identical on the clock. The endpoint tells them apart.
    #[test]
    fn a_loopback_endpoint_is_local_and_a_public_one_is_not() {
        use super::endpoint_is_local;
        for url in [
            "http://127.0.0.1:8545",
            "http://localhost:8545/",
            "http://[::1]:8545",
            "https://localhost",
            "http://127.0.0.1:8545/some/path",
            "ws://127.0.0.1:8546",
        ] {
            assert!(endpoint_is_local(url), "{url}");
        }
        for url in [
            "https://mainnet.base.org",
            "https://eth.llamarpc.com/x/127.0.0.1",
            "https://localhost.evil.invalid/",
            "https://user:pass@rpc.invalid:8545",
            "https://10.0.0.5:8545",
        ] {
            assert!(!endpoint_is_local(url), "{url}");
        }
    }

    #[test]
    fn the_clock_alone_admits_a_head_that_is_merely_recent() {
        let now = 1_700_000_000;
        assert!(head_is_recent(now - 20, now));
        assert!(head_is_recent(now, now));
    }
}

#[cfg(test)]
mod recent_head_tests {
    use super::{RecentHeadError, clock_now_s, recent_head};
    use alloy::{
        providers::{Provider, ProviderBuilder, mock::Asserter},
        rpc::types::{Block, Transaction},
    };

    fn head(number: u64, timestamp: u64) -> Block<Transaction> {
        let mut b = Block::<Transaction>::default();
        b.header.inner.number = number;
        b.header.inner.timestamp = timestamp;
        b
    }

    fn answering(response: Option<&Block<Transaction>>) -> super::provider::RootProvider {
        let a = Asserter::new();
        match response {
            Some(b) => a.push_success(b),
            None => a.push_failure_msg("connection refused"),
        }
        ProviderBuilder::new().connect_mocked_client(a).erased()
    }

    /// A stale head is `Behind`, carrying the block a consumer's remedy
    /// names; the same head is accepted when the caller says an idle chain
    /// is fine; an unreadable head is `Unreadable` with the cause.
    #[tokio::test]
    async fn recent_head_refuses_a_stale_head_with_its_block_and_names_an_unreadable_one() {
        let old = head(1_234, clock_now_s() - 3_600);
        match recent_head(&answering(Some(&old)), false).await {
            Err(RecentHeadError::Behind { head_block }) => assert_eq!(head_block, 1_234),
            other => panic!("expected Behind, got {other:?}"),
        }
        let text = RecentHeadError::Behind { head_block: 1_234 }.to_string();
        assert!(text.contains("(1234)"), "{text}");
        assert_eq!(
            recent_head(&answering(Some(&old)), true).await.unwrap(),
            1_234
        );

        let fresh = head(99, clock_now_s());
        assert_eq!(
            recent_head(&answering(Some(&fresh)), false).await.unwrap(),
            99
        );

        match recent_head(&answering(None), false).await {
            Err(e @ RecentHeadError::Unreadable(_)) => {
                assert!(e.to_string().contains("connection refused"), "{e}")
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
    }
}
