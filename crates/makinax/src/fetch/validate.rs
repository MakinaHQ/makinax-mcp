//! Fetched data, checked against what was asked for.
//!
//! A fetcher (or a caller-supplied adapter) answers a request with calldata
//! and the identities that calldata acts on. Before anything is encoded,
//! those identities are compared EXACTLY with the request: the router the
//! module has registered, the tokens, the amount, the recipient, the chain.
//! A mismatch is [`Error::FetchRefused`], and the message names the field
//! with its expected and actual values: `<field>: expected X, got Y`.
//!
//! Only identities are compared. No slippage bound, price band or minimum is
//! applied here: min-out comes from the request's slippage (see
//! [`crate::fetch::quote::min_out_for`]) and is enforced on-chain by the
//! module.

use std::fmt::Display;

use alloy::primitives::{Address, U256};

use crate::{
    build::BridgeOrderArgs,
    error::{Error, Result},
};

#[cfg(feature = "quote")]
use crate::fetch::quote::Quote;

/// The refusal for one field: `<field>: expected X, got Y`.
pub fn mismatch(field: &str, expected: impl Display, actual: impl Display) -> Error {
    Error::FetchRefused(format!("{field}: expected {expected}, got {actual}"))
}

/// `Ok` when `actual == expected`, else [`mismatch`] for `field`.
pub fn check_field<T: PartialEq + Display>(field: &str, expected: T, actual: T) -> Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(mismatch(field, expected, actual))
    }
}

/// What a swap quote must answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapExpectation {
    /// The module the quote was requested for (its sender and receiver).
    /// A quote carries no module field, so this binds the expectation to
    /// the call being built: [`crate::build::build_swap_from_quote`] refuses
    /// an expectation formed for another module.
    pub module: Address,
    /// The router the module has registered for the swapper id the order
    /// will name (`getSwapperTargets(id).executionTarget`).
    pub swapper_target: Address,
    pub input_token: Address,
    pub output_token: Address,
    pub input_amount: U256,
}

/// Check `q` against `want`: the execution target is the registered
/// router, the approval target is that router, and tokens and amount are
/// the requested ones. See [`validate_swap_quote_with_spenders`] for a
/// caller that knows a separate spender.
#[cfg(feature = "quote")]
pub fn validate_swap_quote(q: &Quote, want: &SwapExpectation) -> Result<()> {
    validate_swap_quote_with_spenders(q, want, &[])
}

/// [`validate_swap_quote`], where the approval target may also be one of
/// `known_spenders`. The library knows no spender of its own: a caller
/// passes one it has read, e.g. the approval target the module registered
/// for the swapper id (`getSwapperTargets(id).approvalTarget`).
///
/// A router [`crate::chain::swappers::adapter_for_router`] can label must be
/// quoted by that adapter; a router it cannot label is not guessed at.
#[cfg(feature = "quote")]
pub fn validate_swap_quote_with_spenders(
    q: &Quote,
    want: &SwapExpectation,
    known_spenders: &[Address],
) -> Result<()> {
    check_field("execution_target", want.swapper_target, q.execution_target)?;
    if q.approval_target != want.swapper_target && !known_spenders.contains(&q.approval_target) {
        let expected = std::iter::once(want.swapper_target)
            .chain(known_spenders.iter().copied())
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" or ");
        return Err(mismatch("approval_target", expected, q.approval_target));
    }
    if let Some(adapter) = crate::chain::swappers::adapter_for_router(q.execution_target) {
        check_field("adapter", adapter, q.adapter)?;
    }
    check_field("input_token", want.input_token, q.input_token)?;
    check_field("output_token", want.output_token, q.output_token)?;
    check_field("input_amount", want.input_amount, q.input_amount)?;
    Ok(())
}

/// What a bridge order must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeExpectation {
    /// Where the funds must land on the destination chain.
    pub recipient: Address,
    pub input_token: Address,
    /// The token the destination delivers.
    pub output_token: Address,
    pub input_amount: U256,
    pub dest_chain_id: u64,
    /// The on-chain bridge id (see [`crate::bridges::BridgeKind`]).
    pub bridge_id: u16,
}

/// Check `o` against `want`: bridge id, recipient, input and output token,
/// input amount and destination chain, exactly. `min_output_amount` is the
/// caller's floor and is not compared. An order with no recipient is refused here;
/// [`crate::build::build_bridge`] defaults an unset recipient to the Safe
/// before calling this.
pub fn validate_bridge_order(o: &BridgeOrderArgs, want: &BridgeExpectation) -> Result<()> {
    check_field("bridge_id", want.bridge_id, o.bridge_id)?;
    let recipient = match o.recipient.as_deref() {
        None => return Err(mismatch("recipient", want.recipient, "none")),
        Some(r) => parse_field::<Address>("recipient", want.recipient, r)?,
    };
    check_field("recipient", want.recipient, recipient)?;
    let input_token = parse_field::<Address>("input_token", want.input_token, &o.input_token)?;
    check_field("input_token", want.input_token, input_token)?;
    let output_token = parse_field::<Address>("output_token", want.output_token, &o.output_token)?;
    check_field("output_token", want.output_token, output_token)?;
    let input_amount = parse_field::<U256>("input_amount", want.input_amount, &o.input_amount)?;
    check_field("input_amount", want.input_amount, input_amount)?;
    match crate::chain::chain_id_for(&o.dest_chain) {
        Some(id) => check_field("dest_chain_id", want.dest_chain_id, id)?,
        None => {
            return Err(mismatch(
                "dest_chain_id",
                want.dest_chain_id,
                format!("unknown chain `{}`", o.dest_chain),
            ));
        }
    }
    Ok(())
}

/// Parse an order field; an unparseable value is a mismatch for that field.
fn parse_field<T: std::str::FromStr>(field: &str, expected: impl Display, raw: &str) -> Result<T> {
    raw.parse()
        .map_err(|_| mismatch(field, expected, format!("`{raw}` (unparseable)")))
}
