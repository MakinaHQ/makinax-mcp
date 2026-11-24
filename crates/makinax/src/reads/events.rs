//! Chunked, budgeted reads of a makinaX module's own events.
//!
//! [`read_module_events`] walks `[from_block, to_block]` in windows of
//! `cfg.chunk_blocks`, spending at most `cfg.max_calls` RPC calls (retries
//! included). This is a bounded, live read of a fixed range only — it is not
//! a substitute for an indexer that maintains full history across restarts;
//! a caller that needs the complete event history of a module keeps one of
//! those itself and uses this only to catch up a window at a time.
//!
//! The rule is progress, not optimism:
//! - An RPC "range too large" / result-limit error halves the window (down
//!   to a single block) and retries the same span — it never by itself ends
//!   the read.
//! - **Once at least one window has been read successfully, this function
//!   never returns an error.** If the call budget then runs out, or the RPC
//!   keeps refusing, whatever was read so far comes back as a partial
//!   [`EventPage`] with `next_from_block` naming the first block not yet
//!   covered, and a `note` explaining why. A caller resumes by calling again
//!   with `from_block` set to `next_from_block`.
//! - **If NO window has ever succeeded, this function returns `Err` naming
//!   the last RPC failure and the block it happened at** — whether that
//!   failure is a non-range reason (nothing a smaller window could have
//!   fixed), or the window being floored to a single block and the RPC
//!   still refusing every attempt until the call budget itself runs out. A
//!   caller must never read an empty page with a cursor as "no events in
//!   this range" when the truth is "nothing was actually read yet".

use alloy::{
    network::Ethereum,
    primitives::Address,
    providers::Provider,
    rpc::types::{Filter, Log},
};
use serde::Serialize;

use crate::{
    config::EventsConfig,
    error::{Error, Result},
};

/// One page of a module's own events, read directly over RPC.
#[derive(Debug, Clone, Serialize)]
pub struct EventPage {
    pub logs: Vec<Log>,
    pub from_block: u64,
    pub to_block: u64,
    /// `Some(next)` when `[from_block, to_block]` was not fully read (call
    /// budget exhausted, or the RPC kept refusing); `next` is the first
    /// block not yet covered — call again with `from_block: next` to
    /// continue. `None` means the whole requested range was read.
    pub next_from_block: Option<u64>,
    /// RPC calls actually made, including retries after a range error.
    pub calls_used: u32,
    /// Set exactly when `next_from_block` is `Some`, explaining why the read
    /// stopped short of `to_block`.
    pub note: Option<String>,
}

/// Read `module`'s events over `[from_block, to_block]`, chunked and
/// budgeted per `cfg`. Filters on the module's address only — no topic
/// filter is applied, so every event the module emits is included.
///
/// See the module docs for the retry and partial-page behaviour.
pub async fn read_module_events<P: Provider<Ethereum>>(
    provider: &P,
    module: Address,
    from_block: u64,
    to_block: u64,
    cfg: &EventsConfig,
) -> Result<EventPage> {
    if from_block > to_block {
        return Ok(EventPage {
            logs: Vec::new(),
            from_block,
            to_block,
            next_from_block: None,
            calls_used: 0,
            note: None,
        });
    }

    let mut logs = Vec::new();
    let mut cursor = from_block;
    let mut chunk = cfg.chunk_blocks.max(1);
    let mut calls_used: u32 = 0;
    let mut last_error: Option<String> = None;

    while cursor <= to_block {
        // No window has succeeded yet iff the cursor has never moved past
        // `from_block` — a success is the only thing that advances it. This
        // stays correct even when a successful window legitimately reads
        // zero logs, which `logs.is_empty()` alone could not distinguish
        // from "nothing has been read".
        let no_progress = cursor == from_block;
        let end = cursor.saturating_add(chunk - 1).min(to_block);

        if calls_used >= cfg.max_calls {
            if no_progress {
                // Nothing was ever read: a partial page here would read as
                // "no events in this range" when the truth is "the read
                // never got anywhere". Name the last failure instead.
                return Err(Error::Rpc(match &last_error {
                    Some(err) => format!(
                        "get_logs [{cursor}, {end}]: call budget ({}) exhausted with no window read; last error: {err}",
                        cfg.max_calls
                    ),
                    None => format!(
                        "get_logs [{cursor}, {end}]: call budget is {} — no call was made",
                        cfg.max_calls
                    ),
                }));
            }
            let remaining = to_block.saturating_sub(cursor).saturating_add(1);
            let mut note = format!(
                "stopped after {calls_used} rpc call(s) (budget {}): {remaining} block(s) from {cursor} to {to_block} were not read; call again with from_block={cursor} to continue",
                cfg.max_calls
            );
            if let Some(err) = &last_error {
                note.push_str(&format!(". Last error: {err}"));
            }
            return Ok(EventPage {
                logs,
                from_block,
                to_block,
                next_from_block: Some(cursor),
                calls_used,
                note: Some(note),
            });
        }

        let filter = Filter::new()
            .address(module)
            .from_block(cursor)
            .to_block(end);
        calls_used += 1;
        match provider.get_logs(&filter).await {
            Ok(mut got) => {
                logs.append(&mut got);
                cursor = end + 1;
                last_error = None;
            }
            Err(e) => {
                let msg = crate::util::redact::text(&e.to_string());
                let range_like = is_range_error(&msg);
                if range_like && chunk > 1 {
                    chunk = (chunk / 2).max(1);
                    last_error = Some(msg);
                    continue;
                }
                if !range_like && no_progress {
                    // Not a range-shaped reason, and no window has ever
                    // succeeded: there is no progress to hand back instead
                    // of an error.
                    return Err(Error::Rpc(format!("get_logs [{cursor}, {end}]: {msg}")));
                }
                // Either a range error already at the minimum window, or a
                // later failure of any kind after real progress: never error
                // out here. Keep the window and let the budget check above
                // decide whether to try again or (with no progress) name
                // this as the last failure.
                last_error = Some(msg);
            }
        }
    }

    Ok(EventPage {
        logs,
        from_block,
        to_block,
        next_from_block: None,
        calls_used,
        note: None,
    })
}

/// Whether an RPC error names a range-too-large / result-limit condition
/// that a smaller window can work around, rather than a different failure
/// (rate limiting, an unauthenticated endpoint, a dead transport, ...) that
/// halving the chunk would not fix.
///
/// A field-tested pattern list, not invented for this function: the same
/// substrings a working log-range negotiation already keys on — a widened
/// window helps for "range", "too many results", "response size", "query
/// returned more", or "block" paired with "limit"/"maximum"; it never helps
/// for a rate limit or an unauthenticated endpoint (429, rate limit, too
/// many requests, unauthorized), which need a later retry instead.
///
/// Public and re-exported at [`crate::reads::is_range_error`] so any
/// caller's own log-range negotiation can classify against this same list
/// instead of keeping a second copy of it — the classification is shared;
/// each caller's retry/backoff policy on top of it is its own.
pub fn is_range_error(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    if m.contains("429")
        || m.contains("rate limit")
        || m.contains("too many requests")
        || m.contains("unauthorized")
    {
        return false;
    }
    m.contains("range")
        || m.contains("too many results")
        || m.contains("response size")
        || m.contains("query returned more")
        || (m.contains("block") && (m.contains("limit") || m.contains("maximum")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_shaped_messages_are_recognised() {
        for msg in [
            "query returned more than 10000 results",
            "block range is too large",
            "response size exceeded",
            "maximum block range: 5000",
            "eth_getLogs is limited to a 10000 block range",
        ] {
            assert!(is_range_error(msg), "{msg}");
        }
    }

    #[test]
    fn rate_limit_and_auth_messages_are_never_read_as_range() {
        for msg in [
            "429 Too Many Requests",
            "rate limit exceeded",
            "too many requests",
            "Unauthorized",
            "connection refused",
        ] {
            assert!(!is_range_error(msg), "{msg}");
        }
    }
}
