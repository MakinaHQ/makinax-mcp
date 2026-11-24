//! The neutral error type: every failure mode a makinaX module read, build,
//! simulate or send path can hit, with no product-specific field (no config
//! section names, no product env-var names, no product remediation text).
//!
//! A consumer maps this onto its own richer error type via `From<Error>` —
//! this crate is built to feed exactly that kind of mapping.

use thiserror::Error as ThisError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("rpc: {0}")]
    Rpc(String),

    #[error("rootfile: {0}")]
    Rootfile(String),

    #[error("merkle: {0}")]
    Merkle(String),

    #[error("loaded root {loaded} differs from the module's on-chain root {onchain}")]
    RootMismatch { loaded: String, onchain: String },

    #[error("unknown module: {0}")]
    UnknownModule(String),

    #[error("unknown action: {0}")]
    UnknownAction(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// A param was supplied that the action has no runtime slot for. Its
    /// value, if the action uses one there, is fixed by the root. Structured
    /// so a consumer can render its own remedy field for field.
    #[error(
        "invalid params: action `{action}` has no runtime param `{param}`; fixed values are committed by the root and change only with a new root"
    )]
    UnknownParam { action: String, param: String },

    /// An action's input slot carries a meta-type (Swap, MerklClaim, …),
    /// which `instructions::builder` does not fill. Structured so a consumer
    /// can render its own wording for this refusal field for field.
    #[error(
        "invalid params: action `{action}` has a meta-type slot `{slot}` (meta-type slots are not supported)"
    )]
    MetaTypeSlot { action: String, slot: String },

    /// A declared-requirement signature's arity does not match the
    /// argument count supplied for it (`dyn_call::encode_call`). Structured
    /// (not folded into `Config`) so a consumer whose own wording predates
    /// this crate — this exact check moved here verbatim — can restore its
    /// old text losslessly via `From<Error>`, field for field, rather than
    /// pattern-matching a rendered string.
    #[error("declared requirement: `{sig}` takes {expected} args, {supplied} supplied")]
    ArgCount {
        sig: String,
        expected: usize,
        supplied: usize,
    },

    #[error("simulation failed: {explanation}")]
    SimFailed { code: String, explanation: String },

    /// Structured (named fields, a fixed rendered message) so a caller's
    /// `From<makinax::Error>` can map it field for field, losslessly. A
    /// caller that matches on these fields would change behaviour, not just
    /// wording, under a lossy mapping.
    #[error(
        "fork backend failed for chain `{chain}` during {phase} (fork source: {fork_source}): {detail}. Tune [fork] (timeout_ms/retries/retry_backoff_ms/compute_units_per_second), point fork.source_rpc_urls.{chain} at a faster endpoint, or configure [tenderly] as the facts-sim backend"
    )]
    ForkBackend {
        chain: String,
        phase: String,
        fork_source: String,
        detail: String,
    },

    /// No valid simulation receipt for these exact call params
    /// (`sim::receipts`): missing, already spent, or issued for a different
    /// call. Structured so a consumer can render its own remedy (e.g. name
    /// its own simulate entry point) losslessly.
    #[error("simulation required: no valid receipt for these exact call params — simulate first")]
    SimulationRequired,

    /// A simulation receipt older than its TTL (`sim::receipts`).
    #[error(
        "simulation stale: receipt issued at block {issued}, current block {current}, ttl {ttl} blocks — re-simulate"
    )]
    SimulationStale { issued: u64, current: u64, ttl: u64 },

    /// A MAINNET-targeted simulation asked for a chain whose fork endpoint
    /// is an attached dev endpoint (`dev_rpc_urls.<chain>`): its answer would
    /// describe dev state while the transaction goes to the live chain.
    #[error(
        "config: refusing to run a MAINNET-targeted simulation on chain `{chain}`'s dev endpoint: `dev_rpc_urls.{chain}` is configured and every facts sim resolves to it, so the answer would describe dev state while the transaction goes to the live chain. Remove `dev_rpc_urls.{chain}` for mainnet operation, or configure [tenderly] as the facts-sim backend."
    )]
    MainnetOnDevEndpoint { chain: String },

    /// A MAINNET-targeted simulation asked for a chain whose warm fork has
    /// taken dev executions: it no longer re-anchors, so it is frozen with
    /// dev state applied. Dev-write marks live in the `ForkManager`, so a
    /// fresh manager (a fresh process) is a clean fork.
    #[error(
        "config: refusing to run a MAINNET-targeted simulation on chain `{chain}`'s warm fork: it has taken {dev_writes} dev execution(s), so it no longer re-anchors and is frozen with dev state applied. Start a new process for a clean fork (dev-write marks are in-memory), or configure [tenderly] as the facts-sim backend."
    )]
    MainnetOnDevFork { chain: String, dev_writes: u32 },

    #[error("quote not found: {0}")]
    QuoteNotFound(String),

    #[error("quote stale: {age_s}s old, ttl {ttl_s}s")]
    QuoteStale { age_s: u64, ttl_s: u64 },

    #[error("fetched data refused: {0}")]
    FetchRefused(String),

    /// A fetch service answered HTTP 429. Structured so a caller can back
    /// off (honouring `retry_after_s`, the service's `Retry-After`, when it
    /// sent one) instead of reporting a failure.
    #[error("{service}: rate limited")]
    FetchRateLimited {
        service: String,
        retry_after_s: Option<u64>,
    },

    /// The CONFIG half of a swapper is missing. The remedy is DERIVED from
    /// the module's registered ids, never suggested from a convention — see
    /// `chain::swappers` for why a guess here creates a duplicate
    /// registration and permanent config/on-chain divergence.
    ///
    /// Structured identically to the consumer's own richer error type for
    /// the same reason as `ForkBackend` above: a `From<Error>` mapping there
    /// constructs this shape directly.
    #[error(
        "swap adapter `{adapter}` has no swapperId mapping on machine `{machine}` — a swap needs BOTH a config mapping and an on-chain registration, and the CONFIG half is missing (registering on-chain again cannot fix that half). {remedy}"
    )]
    SwapperNotConfigured {
        machine: String,
        adapter: String,
        /// The id the config line names: the id this router is ALREADY
        /// registered under when the scan found it, otherwise a free id.
        /// `None` when the scan itself failed — there is no derived id
        /// then, and a placeholder would be the guess this error was
        /// rewritten to remove.
        swapper_id: Option<u16>,
        /// One of `chain::swappers_reg::REG_*` — what the scan established,
        /// kept as the shared constant so the producer and every consumer
        /// of this error cannot drift on what "not registered" was allowed
        /// to mean.
        registration: &'static str,
        router: String,
        module: String,
        /// The registration call — `None` when the router is already
        /// registered. A calldata blob sitting beside "already registered"
        /// is an invitation to create the duplicate this error exists to
        /// prevent.
        calldata: Option<String>,
        /// The whole remedy, rendered once, so this message and any
        /// machine-readable payload cannot disagree about what to do.
        remedy: String,
    },

    #[error("send refused by pre-send hook: {0}")]
    HookRefused(String),

    #[error(transparent)]
    Internal(#[from] eyre::Report),
}

impl Error {
    /// [`Error::Rpc`] for a message built from a transport, provider or HTTP
    /// error. Such text can quote the endpoint it was talking to, and an
    /// endpoint URL routinely carries its key, so the message is redacted
    /// ([`crate::util::redact::text`]) before it becomes the error. Text
    /// with no URL in it is kept byte for byte.
    pub(crate) fn rpc(message: impl std::fmt::Display) -> Self {
        Error::Rpc(crate::util::redact::text(&message.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_mismatch_names_both_roots() {
        let e = Error::RootMismatch {
            loaded: "0xaaa".into(),
            onchain: "0xbbb".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("0xaaa"), "{msg}");
        assert!(msg.contains("0xbbb"), "{msg}");
    }

    #[test]
    fn io_is_transparent() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let e: Error = io.into();
        assert!(e.to_string().contains("missing"), "{e}");
    }

    #[test]
    fn internal_wraps_eyre() {
        let e: Error = eyre::eyre!("boom").into();
        assert!(e.to_string().contains("boom"), "{e}");
    }

    /// `ArgCount`'s rendered text is neutral: it must name neither this
    /// crate's own "declared requirement" framing's origin nor any
    /// consumer's vocabulary for whatever supplied the args (a consumer
    /// that used to say "manifest supplies" restores that exact word
    /// itself, via `From<Error>` — this crate never says it).
    #[test]
    fn arg_count_is_neutral_and_names_both_numbers() {
        let e = Error::ArgCount {
            sig: "setFeedRoute(address,address,uint256,address,uint256)".into(),
            expected: 5,
            supplied: 2,
        };
        let msg = e.to_string();
        assert!(msg.contains("setFeedRoute"), "{msg}");
        assert!(msg.contains("takes 5 args"), "{msg}");
        assert!(msg.contains('2'), "{msg}");
        assert!(
            !msg.to_lowercase().contains("manifest"),
            "makinax's own text must not name a consumer's config-source vocabulary: {msg}"
        );
    }

    /// Pins the exact rendered text of `Error::ForkBackend`: a caller that
    /// maps it field for field, or matches on its message, relies on this
    /// text staying the same.
    #[test]
    fn fork_backend_renders_its_documented_text() {
        let e = Error::ForkBackend {
            chain: "base".into(),
            phase: "spawn".into(),
            fork_source: "https://example.invalid".into(),
            detail: "timed out".into(),
        };
        assert_eq!(
            e.to_string(),
            "fork backend failed for chain `base` during spawn (fork source: https://example.invalid): timed out. Tune [fork] (timeout_ms/retries/retry_backoff_ms/compute_units_per_second), point fork.source_rpc_urls.base at a faster endpoint, or configure [tenderly] as the facts-sim backend"
        );
    }

    /// Same pin, for `Error::SwapperNotConfigured` — and exercises the
    /// shared `chain::swappers_reg` constant as the `registration` value.
    #[test]
    fn swapper_not_configured_renders_its_documented_text() {
        let e = Error::SwapperNotConfigured {
            machine: "demo".into(),
            adapter: "odos".into(),
            swapper_id: None,
            registration: crate::chain::swappers::REG_ABSENT,
            router: "0xrouter".into(),
            module: "0xmodule".into(),
            calldata: None,
            remedy: "register it".into(),
        };
        assert_eq!(
            e.to_string(),
            "swap adapter `odos` has no swapperId mapping on machine `demo` — a swap needs BOTH a config mapping and an on-chain registration, and the CONFIG half is missing (registering on-chain again cannot fix that half). register it"
        );
        // Fields not interpolated into the message still round-trip — they
        // are the machine-readable half of this error.
        assert_eq!(e_registration(&e), crate::chain::swappers::REG_ABSENT);
    }

    fn e_registration(e: &Error) -> &'static str {
        match e {
            Error::SwapperNotConfigured { registration, .. } => registration,
            other => panic!("expected SwapperNotConfigured, got {other:?}"),
        }
    }

    /// A message built from a transport error loses the endpoint's key on
    /// its way into `Error::Rpc`, in the field as well as in the rendered
    /// text; a message with no URL is kept byte for byte.
    #[test]
    fn rpc_masks_a_keyed_endpoint_and_keeps_everything_else() {
        let e = Error::rpc(
            "get_logs: error sending request for url \
             (https://rpc.example.com/v2/9fKd2Lm4Xp7Qw1Nz8Bt3Yr): timed out",
        );
        assert_eq!(
            e.to_string(),
            "rpc: get_logs: error sending request for url \
             (https://rpc.example.com/v2/<redacted>): timed out"
        );
        assert!(matches!(&e, Error::Rpc(m) if !m.contains("9fKd2Lm4Xp7Qw1Nz8Bt3Yr")));

        let plain = "server returned an error response: error code -32000:\n  nonce too low";
        assert!(matches!(Error::rpc(plain), Error::Rpc(m) if m == plain));
    }
}
