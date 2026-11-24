//! `read_module_events` against a mocked RPC: the chunk/budget/retry state
//! machine, not real log decoding (that is exercised by the bridges' own
//! decoders). Review focus 3: a range larger than the RPC allows, or an RPC
//! that returns "range too large", must come back as partial results with a
//! cursor — never an error that loses what was already read.
//!
//! That rule holds only once at least one window has
//! actually been read. With ZERO progress — the window floored to a single
//! block and the RPC still refusing until the budget runs out, or a range
//! error followed by a different kind of failure — the read must return
//! `Err` naming the last failure, never an empty page with a cursor that a
//! caller could misread as "no events in this range".

use alloy::{
    primitives::Address,
    providers::{ProviderBuilder, mock::Asserter},
    rpc::types::Log,
};
use makinax::{Error, config::EventsConfig, reads::events::read_module_events};

const MODULE: Address = Address::with_last_byte(0x11);

fn log_at(block: u64) -> Vec<Log> {
    vec![Log {
        block_number: Some(block),
        ..Default::default()
    }]
}

fn cfg(chunk_blocks: u64, max_calls: u32) -> EventsConfig {
    EventsConfig {
        chunk_blocks,
        max_calls,
    }
}

/// chunk=10, max_calls=2, range 0..=99 → logs from 2 calls, cursor at 20.
#[tokio::test]
async fn a_range_larger_than_the_budget_returns_a_cursor() {
    let mock = Asserter::new();
    mock.push_success(&log_at(3));
    mock.push_success(&log_at(13));
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 0, 99, &cfg(10, 2))
        .await
        .expect("two chunks succeed within budget");
    assert_eq!(page.calls_used, 2);
    assert_eq!(page.logs.len(), 2);
    assert_eq!(page.next_from_block, Some(20));
    assert!(page.note.is_some(), "a cursor must explain why it stopped");
}

/// First call errors "query returned more than 10000 results", then ok →
/// the chunk shrinks internally and the read completes without an error.
#[tokio::test]
async fn a_range_too_large_error_halves_the_chunk_and_continues() {
    let mock = Asserter::new();
    mock.push_failure_msg("query returned more than 10000 results");
    mock.push_success(&log_at(5));
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 0, 9, &cfg(20, 50))
        .await
        .expect("a range error is never surfaced once a smaller window works");
    assert_eq!(page.calls_used, 2);
    assert_eq!(page.logs.len(), 1);
    assert_eq!(page.next_from_block, None);
    assert_eq!(page.note, None);
}

/// ok, then 3 errors (budget = 4) → Ok(page) with the progress kept, a
/// cursor at the first unread block, and a note — never an error.
#[tokio::test]
async fn a_failure_after_progress_keeps_the_progress() {
    let mock = Asserter::new();
    mock.push_success(&log_at(1));
    mock.push_failure_msg("boom");
    mock.push_failure_msg("boom");
    mock.push_failure_msg("boom");
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 0, 99, &cfg(10, 4))
        .await
        .expect("progress already made must never be discarded as an error");
    assert_eq!(page.calls_used, 4);
    assert_eq!(page.logs.len(), 1);
    assert_eq!(page.next_from_block, Some(10));
    assert!(page.note.is_some());
}

/// The whole range fits comfortably inside the budget → no cursor, no note.
#[tokio::test]
async fn a_complete_read_has_no_cursor() {
    let mock = Asserter::new();
    mock.push_success(&log_at(1));
    mock.push_success(&log_at(11));
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 0, 19, &cfg(10, 50))
        .await
        .expect("a fully covered range reads cleanly");
    assert_eq!(page.calls_used, 2);
    assert_eq!(page.logs.len(), 2);
    assert_eq!(page.next_from_block, None);
    assert_eq!(page.note, None);
}

/// With no window ever read and a non-range reason, there is no
/// progress to hand back instead of an error.
#[tokio::test]
async fn a_first_call_failure_for_a_non_range_reason_is_an_error() {
    let mock = Asserter::new();
    mock.push_failure_msg("connection refused");
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let err = read_module_events(&provider, MODULE, 0, 99, &cfg(10, 50))
        .await
        .expect_err("no chunk has succeeded yet, and this is not range-shaped");
    match err {
        Error::Rpc(m) => assert!(m.contains("connection refused"), "{m}"),
        other => panic!("expected Error::Rpc, got {other:?}"),
    }
}

/// First named case: range errors halve the window all the way to a
/// single block and keep failing there until the call budget itself runs
/// out, with zero progress the whole time. That must be `Err` naming the
/// range condition and the block it happened at — not an empty `Ok` page
/// that reads as "no events in this range".
#[tokio::test]
async fn range_errors_floored_at_one_block_that_never_recover_are_an_error() {
    let mock = Asserter::new();
    // chunk starts at 4: [0,3] fails and halves to 2, [0,1] fails and halves
    // to 1, [0,0] fails at the floor — three calls, budget exhausted, still
    // zero progress.
    mock.push_failure_msg("query returned more than 10000 results");
    mock.push_failure_msg("query returned more than 10000 results");
    mock.push_failure_msg("query returned more than 10000 results");
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let err = read_module_events(&provider, MODULE, 0, 99, &cfg(4, 3))
        .await
        .expect_err("floored at 1 block and still failing, with no progress, must not be Ok");
    match err {
        Error::Rpc(m) => {
            assert!(m.contains("query returned more than 10000 results"), "{m}");
            assert!(m.starts_with("get_logs [0, 0]"), "{m}");
        }
        other => panic!("expected Error::Rpc, got {other:?}"),
    }
}

/// Second named case: a range error (which alone never ends the read)
/// followed by a different kind of failure, with zero progress. The error
/// must name the SECOND failure, not the range condition that preceded it.
#[tokio::test]
async fn a_range_error_then_a_non_range_error_with_no_progress_is_an_error() {
    let mock = Asserter::new();
    mock.push_failure_msg("query returned more than 10000 results");
    mock.push_failure_msg("connection refused");
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let err = read_module_events(&provider, MODULE, 0, 99, &cfg(10, 50))
        .await
        .expect_err("no window has succeeded, and the second failure is not range-shaped");
    match err {
        Error::Rpc(m) => {
            assert!(m.contains("connection refused"), "{m}");
            assert!(
                !m.contains("query returned more"),
                "must name the second failure, not the first: {m}"
            );
        }
        other => panic!("expected Error::Rpc, got {other:?}"),
    }
}

/// A single-block range makes exactly one call and never has a cursor.
#[tokio::test]
async fn a_single_block_range_makes_one_call_and_has_no_cursor() {
    let mock = Asserter::new();
    mock.push_success(&log_at(42));
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let page = read_module_events(&provider, MODULE, 42, 42, &cfg(10, 50))
        .await
        .expect("a single block is one call away");
    assert_eq!(page.calls_used, 1);
    assert_eq!(page.logs.len(), 1);
    assert_eq!(page.next_from_block, None);
    assert_eq!(page.note, None);
}

/// An empty range (from > to) is a trivial complete read: no RPC call, no
/// cursor.
#[tokio::test]
async fn an_empty_range_reads_nothing_and_makes_no_call() {
    let provider = ProviderBuilder::new().connect_mocked_client(Asserter::new());
    let page = read_module_events(&provider, MODULE, 50, 10, &cfg(10, 50))
        .await
        .expect("an empty range never touches the RPC");
    assert_eq!(page.calls_used, 0);
    assert!(page.logs.is_empty());
    assert_eq!(page.next_from_block, None);
    assert_eq!(page.note, None);
}
