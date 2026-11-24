//! Typed encoders for `IMakinaXModule`'s owner-restricted (Safe-only)
//! functions, plus the generic path every producer of an owner call goes
//! through.
//!
//! The set [`owner_function_names`] returns is exactly the module's
//! owner-only surface as bounded by the `OWNER_SURFACE_START`/
//! `OWNER_SURFACE_END` markers in `chain::module`, plus one named
//! exception (`setFeedRoute`, declared elsewhere in that file with its own
//! evidence) — verified against the real contract source, makina-x @
//! 6168e175, function by function, against each one's actual access
//! modifier (see the comments in `chain::module` for the citations).
//! `pause`/`unpause` are the one documented exception to "Safe-only": both
//! are `onlyGuardian` in the real contract, included here because the Safe
//! is a permanent, protected guardian (added at module init, never
//! removable) — not because either selector is Safe-exclusive on its own.
//!
//! [`encode_owner_call`] refuses any `function` this list does not name, so
//! encoding an operator-only or unknown function as an "owner call" is
//! impossible — the same architectural-guard shape as
//! [`super::batch::TxBuilderBatch`]'s private fields.
//!
//! A test in this module (`owner_function_names_matches_the_bounded_owner_surface_in_both_directions`)
//! cross-checks the list against `chain::module`'s source in BOTH
//! directions, so the two cannot silently drift apart.

use alloy::primitives::{Address, B256, U256};

use super::{batch::OwnerCall, dyn_call::encode_call};
use crate::{Error, Result};

/// Every owner-restricted function on `IMakinaXModule`, as full solidity
/// signatures — the only names [`encode_owner_call`] accepts.
pub fn owner_function_names() -> &'static [&'static str] {
    &[
        "setAllowedInstrRoot(bytes32)",
        "addOperator(address)",
        "removeOperator(address)",
        "addGuardian(address)",
        "removeGuardian(address)",
        "setOperatingMode(uint8)",
        "pause()",
        "unpause()",
        "setFeedRoute(address,address,uint256,address,uint256)",
        "clearFeedRoute(address)",
        "setFeedStaleThreshold(address,uint256)",
        "setAccountingCurrency(address)",
        "setMaxPositionIncreaseLossBps(uint256)",
        "setMaxPositionDecreaseLossBps(uint256)",
        "setInstrCooldownDuration(uint256)",
        "setMaxSwapLossBps(uint256)",
        "setSwapCooldownDuration(uint256)",
        "setSwapperTargets(uint16,address,address)",
        "addRecipient(uint256,address)",
        "removeRecipient(uint256,address)",
        "setMaxBridgeLossBps(uint16,uint256)",
        "setBridgeCooldownDuration(uint256)",
        "sweepERC20(address)",
        "sweepERC721(address,uint256)",
        "sweepERC6909(address,uint256)",
        "sweepNative()",
    ]
}

/// Shared rendering for every encoder's summary line: `bareName(arg, arg,
/// ...)` — never the full `sig(types)` the generic path used to prefix
/// before this fix, which read differently from every typed encoder right
/// next to it in the same batch.
fn describe(function: &str, args: &[String]) -> String {
    let name = function.split('(').next().unwrap_or(function);
    format!("{name}({})", args.join(", "))
}

/// Generic path: any owner-restricted function, by signature + string args,
/// via [`encode_call`] (the same signature-string encoder `batch::build_batch`
/// uses). Refuses a `function` [`owner_function_names`] does not name —
/// encoding an operator-only or unknown function as an owner call is not
/// possible through this path.
pub fn encode_owner_call(module: Address, function: &str, args: &[String]) -> Result<OwnerCall> {
    if !owner_function_names().contains(&function) {
        return Err(Error::InvalidParams(format!(
            "`{function}` is not an owner-restricted function of IMakinaXModule — refusing to \
             encode it as an owner call. Owner functions: {}",
            owner_function_names().join(", ")
        )));
    }
    // Encode now, not just validate the name — a caller gets the same
    // arity/type errors `encode_call` gives directly, at the point they
    // asked for an owner call rather than only when a batch is built.
    encode_call(function, args)?;
    Ok(OwnerCall {
        to: module,
        sig: function.to_string(),
        args: args.to_vec(),
        summary: describe(function, args),
    })
}

/// Installs/rotates the instruction whitelist. Deliberately the LAST call in
/// a batch that also configures the settings the new root was composed
/// against: everything the root authorizes must already be in place.
pub fn set_allowed_instr_root(module: Address, root: B256) -> OwnerCall {
    let sig = "setAllowedInstrRoot(bytes32)";
    let args = vec![format!("{root:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn add_operator(module: Address, operator: Address) -> OwnerCall {
    let sig = "addOperator(address)";
    let args = vec![format!("{operator:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn remove_operator(module: Address, operator: Address) -> OwnerCall {
    let sig = "removeOperator(address)";
    let args = vec![format!("{operator:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn add_guardian(module: Address, guardian: Address) -> OwnerCall {
    let sig = "addGuardian(address)";
    let args = vec![format!("{guardian:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn remove_guardian(module: Address, guardian: Address) -> OwnerCall {
    let sig = "removeGuardian(address)";
    let args = vec![format!("{guardian:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// `newMode` is the raw `OperatingMode` enum ordinal (0=OPEN, 1=FENCED,
/// 2=WALLED in makina-x @ 6168e175's `IMakinaXGovernable.OperatingMode`),
/// ABI-identical to the Solidity enum parameter.
pub fn set_operating_mode(module: Address, new_mode: u8) -> OwnerCall {
    let sig = "setOperatingMode(uint8)";
    let args = vec![new_mode.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// `onlyGuardian` in the real contract, not Safe-exclusive on its own — see
/// the module-level doc for why it is still part of the owner surface.
pub fn pause(module: Address) -> OwnerCall {
    let sig = "pause()";
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &[]),
        args: vec![],
    }
}

/// `onlyGuardian` in the real contract, not Safe-exclusive on its own — see
/// the module-level doc for why it is still part of the owner surface.
pub fn unpause(module: Address) -> OwnerCall {
    let sig = "unpause()";
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &[]),
        args: vec![],
    }
}

/// `stalenessThreshold1`/`stalenessThreshold2` are seconds. `feed2` may be
/// the zero address (single-feed route); `stalenessThreshold2` is then
/// ignored on-chain.
#[allow(clippy::too_many_arguments)]
pub fn set_feed_route(
    module: Address,
    token: Address,
    feed1: Address,
    staleness_threshold1: u64,
    feed2: Address,
    staleness_threshold2: u64,
) -> OwnerCall {
    let sig = "setFeedRoute(address,address,uint256,address,uint256)";
    let args = vec![
        format!("{token:?}"),
        format!("{feed1:?}"),
        staleness_threshold1.to_string(),
        format!("{feed2:?}"),
        staleness_threshold2.to_string(),
    ];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn clear_feed_route(module: Address, token: Address) -> OwnerCall {
    let sig = "clearFeedRoute(address)";
    let args = vec![format!("{token:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_feed_stale_threshold(module: Address, feed: Address, new_threshold: u64) -> OwnerCall {
    let sig = "setFeedStaleThreshold(address,uint256)";
    let args = vec![format!("{feed:?}"), new_threshold.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_accounting_currency(module: Address, new_currency: Address) -> OwnerCall {
    let sig = "setAccountingCurrency(address)";
    let args = vec![format!("{new_currency:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_max_position_increase_loss_bps(module: Address, new_max_loss_bps: u64) -> OwnerCall {
    let sig = "setMaxPositionIncreaseLossBps(uint256)";
    let args = vec![new_max_loss_bps.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_max_position_decrease_loss_bps(module: Address, new_max_loss_bps: u64) -> OwnerCall {
    let sig = "setMaxPositionDecreaseLossBps(uint256)";
    let args = vec![new_max_loss_bps.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_instr_cooldown_duration(module: Address, new_duration_s: u64) -> OwnerCall {
    let sig = "setInstrCooldownDuration(uint256)";
    let args = vec![new_duration_s.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_max_swap_loss_bps(module: Address, new_max_loss_bps: u64) -> OwnerCall {
    let sig = "setMaxSwapLossBps(uint256)";
    let args = vec![new_max_loss_bps.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_swap_cooldown_duration(module: Address, new_duration_s: u64) -> OwnerCall {
    let sig = "setSwapCooldownDuration(uint256)";
    let args = vec![new_duration_s.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_swapper_targets(
    module: Address,
    swapper_id: u16,
    approval_target: Address,
    execution_target: Address,
) -> OwnerCall {
    let sig = "setSwapperTargets(uint16,address,address)";
    let args = vec![
        swapper_id.to_string(),
        format!("{approval_target:?}"),
        format!("{execution_target:?}"),
    ];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// `destination_chain_id` is `U256`, matching the on-chain `uint256`
/// exactly (real EVM chain ids fit comfortably in `u64`, but nothing about
/// the ABI type guarantees that, and this module makes no assumption an
/// id-like `uint256` parameter does not need).
pub fn add_recipient(module: Address, destination_chain_id: U256, recipient: Address) -> OwnerCall {
    let sig = "addRecipient(uint256,address)";
    let args = vec![destination_chain_id.to_string(), format!("{recipient:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn remove_recipient(
    module: Address,
    destination_chain_id: U256,
    recipient: Address,
) -> OwnerCall {
    let sig = "removeRecipient(uint256,address)";
    let args = vec![destination_chain_id.to_string(), format!("{recipient:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_max_bridge_loss_bps(module: Address, bridge_id: u16, max_loss_bps: u64) -> OwnerCall {
    let sig = "setMaxBridgeLossBps(uint16,uint256)";
    let args = vec![bridge_id.to_string(), max_loss_bps.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn set_bridge_cooldown_duration(module: Address, new_duration_s: u64) -> OwnerCall {
    let sig = "setBridgeCooldownDuration(uint256)";
    let args = vec![new_duration_s.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// Asset recovery, not module configuration — but still `onlySafe` on the
/// real contract (makina-x @ 6168e175), so it is part of the complete
/// owner surface.
pub fn sweep_erc20(module: Address, token: Address) -> OwnerCall {
    let sig = "sweepERC20(address)";
    let args = vec![format!("{token:?}")];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// `token_id` is `U256`, not `u64`: ERC721 ids are routinely keccak-derived
/// (e.g. tick-range/position hashes), so nothing narrower than the on-chain
/// `uint256` can represent them without risking silent truncation.
pub fn sweep_erc721(module: Address, token: Address, token_id: U256) -> OwnerCall {
    let sig = "sweepERC721(address,uint256)";
    let args = vec![format!("{token:?}"), token_id.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

/// `id` is `U256`, not `u64`: ERC6909 ids are routinely a cast `address`
/// (20 bytes, well beyond `u64`), so nothing narrower than the on-chain
/// `uint256` can represent them without risking silent truncation.
pub fn sweep_erc6909(module: Address, token: Address, id: U256) -> OwnerCall {
    let sig = "sweepERC6909(address,uint256)";
    let args = vec![format!("{token:?}"), id.to_string()];
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &args),
        args,
    }
}

pub fn sweep_native(module: Address) -> OwnerCall {
    let sig = "sweepNative()";
    OwnerCall {
        to: module,
        sig: sig.to_string(),
        summary: describe(sig, &[]),
        args: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::address;

    /// Cross-check, in BOTH directions, bounded by explicit markers (not
    /// "runs to end of file", which could silently swallow unrelated later
    /// sections, or a heading string, which could be reworded without
    /// anyone noticing the check had gone slack):
    ///
    /// 1. every non-view `function` declared inside
    ///    `chain::module.rs`'s `OWNER_SURFACE_START`..`OWNER_SURFACE_END`
    ///    region must be named by [`owner_function_names`];
    /// 2. every name [`owner_function_names`] returns must either be
    ///    declared inside that region, or be a listed, evidenced exception
    ///    (`setFeedRoute`, declared under `IOracleRegistry` above the
    ///    marked region with its own "Safe-only" comment).
    ///
    /// The previous version of this test only checked direction 2, over an
    /// unbounded region — so an owner-only setter newly added to the real
    /// contract (and to the module bindings) but never wired into
    /// `owner_function_names()` would pass silently. This version catches
    /// exactly that.
    #[test]
    fn owner_function_names_matches_the_bounded_owner_surface_in_both_directions() {
        let module_src = include_str!("../chain/module.rs");
        let start = module_src
            .find("OWNER_SURFACE_START")
            .expect("chain::module.rs must still carry the OWNER_SURFACE_START marker");
        let end = module_src
            .find("OWNER_SURFACE_END")
            .expect("chain::module.rs must still carry the OWNER_SURFACE_END marker");
        assert!(start < end, "OWNER_SURFACE markers are out of order");
        let region = &module_src[start..end];

        // Declared elsewhere in chain::module.rs (own Safe-only evidence),
        // not duplicated inside the bounded section.
        const NAMED_EXCEPTIONS: &[&str] = &["setFeedRoute"];

        let declared_writes: Vec<&str> = region
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("function "))
            .filter(|line| !line.contains("view"))
            .filter_map(|line| line["function ".len()..].split('(').next())
            .collect();

        // Direction 1: every declared write is named.
        for name in &declared_writes {
            assert!(
                owner_function_names()
                    .iter()
                    .any(|sig| sig.split('(').next() == Some(*name)),
                "chain::module.rs declares owner-only write `{name}(...)` inside \
                 OWNER_SURFACE but owner_function_names() does not name it"
            );
        }

        // Direction 2: every named function is declared (in-region or a
        // listed exception).
        for sig in owner_function_names() {
            let name = sig.split('(').next().unwrap();
            if NAMED_EXCEPTIONS.contains(&name) {
                assert!(
                    module_src.contains(&format!("function {name}(")),
                    "named exception `{name}` must still be declared somewhere in \
                     chain::module.rs"
                );
                continue;
            }
            assert!(
                declared_writes.contains(&name),
                "`{sig}` is in owner_function_names() but chain::module.rs's \
                 OWNER_SURFACE section declares no non-view `function {name}(` \
                 (and it is not a listed exception)"
            );
        }
    }

    #[test]
    fn every_owner_function_has_a_typed_encoder() {
        let m = address!("0000000000000000000000000000000000000002");
        let g = address!("0000000000000000000000000000000000000003");
        let calls = [
            set_allowed_instr_root(m, B256::ZERO).sig,
            add_operator(m, g).sig,
            remove_operator(m, g).sig,
            add_guardian(m, g).sig,
            remove_guardian(m, g).sig,
            set_operating_mode(m, 1).sig,
            pause(m).sig,
            unpause(m).sig,
            set_feed_route(m, g, g, 3600, Address::ZERO, 0).sig,
            clear_feed_route(m, g).sig,
            set_feed_stale_threshold(m, g, 3600).sig,
            set_accounting_currency(m, g).sig,
            set_max_position_increase_loss_bps(m, 100).sig,
            set_max_position_decrease_loss_bps(m, 100).sig,
            set_instr_cooldown_duration(m, 3600).sig,
            set_max_swap_loss_bps(m, 100).sig,
            set_swap_cooldown_duration(m, 3600).sig,
            set_swapper_targets(m, 1, g, g).sig,
            add_recipient(m, U256::from(1), g).sig,
            remove_recipient(m, U256::from(1), g).sig,
            set_max_bridge_loss_bps(m, 1, 100).sig,
            set_bridge_cooldown_duration(m, 3600).sig,
            sweep_erc20(m, g).sig,
            sweep_erc721(m, g, U256::from(1)).sig,
            sweep_erc6909(m, g, U256::from(1)).sig,
            sweep_native(m).sig,
        ];
        // Every typed encoder's signature must be a real owner function...
        for sig in &calls {
            assert!(
                owner_function_names().contains(&sig.as_str()),
                "typed encoder produced a signature not in owner_function_names(): {sig}"
            );
        }
        // ...and vice versa: no owner function is missing a typed encoder.
        assert_eq!(
            calls.len(),
            owner_function_names().len(),
            "every name in owner_function_names() must have exactly one typed encoder above"
        );
    }

    #[test]
    fn encode_owner_call_refuses_operator_only_functions() {
        let m = address!("0000000000000000000000000000000000000002");
        // `swap` is a real, operator-callable function on the same module —
        // never owner-restricted, so it must never encode as an owner call.
        let e = encode_owner_call(
            m,
            "swap((uint16,bytes,address,address,uint256,uint256))",
            &[],
        )
        .unwrap_err();
        assert!(e.to_string().contains("swap"), "{e}");
    }

    #[test]
    fn summaries_use_the_bare_name_not_the_typed_signature() {
        let m = address!("0000000000000000000000000000000000000002");
        let g = address!("0000000000000000000000000000000000000003");
        // Typed and generic paths must render identically for the same
        // call — before this fix the generic path prefixed the full
        // `sig(types)` ahead of the args, which read differently from
        // every typed encoder's summary right next to it in a batch.
        let typed = add_operator(m, g);
        let generic = encode_owner_call(m, "addOperator(address)", &[format!("{g:?}")]).unwrap();
        assert_eq!(typed.summary, generic.summary);
        assert_eq!(typed.summary, format!("addOperator({g:?})"));
        assert!(!typed.summary.contains("address)("), "{}", typed.summary);
    }

    /// ERC721 ids are routinely keccak-derived and ERC6909 ids are
    /// routinely a cast `address` — both classes of value routinely exceed
    /// `u64::MAX`. A `u64` parameter here would have refused (on overflow)
    /// or silently truncated such an id; `U256` does neither.
    #[test]
    fn sweep_ids_beyond_u64_max_encode_without_truncation() {
        let m = address!("0000000000000000000000000000000000000002");
        let g = address!("0000000000000000000000000000000000000003");
        let big = U256::from(u64::MAX) + U256::from(1u64); // 2^64, one past u64::MAX
        assert!(big > U256::from(u64::MAX));

        for call in [sweep_erc721(m, g, big), sweep_erc6909(m, g, big)] {
            assert_eq!(call.args[1], big.to_string());
            let data =
                encode_call(&call.sig, &call.args).unwrap_or_else(|e| panic!("{}: {e}", call.sig));
            // calldata = 4-byte selector + word0 (address) + word1 (id).
            let decoded_id = U256::from_be_slice(&data[4 + 32..4 + 64]);
            assert_eq!(
                decoded_id, big,
                "{}: id truncated during encoding",
                call.sig
            );
        }
    }
}
