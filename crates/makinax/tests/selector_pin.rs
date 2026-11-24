//! A prior rename of the module's `sol!` interfaces/types proved that
//! renaming a `sol!` interface does not change its ABI selectors, by
//! diffing a full 50-selector dump taken immediately before and after the
//! rename. That dump was a throwaway test, deleted once the rename landed.
//!
//! This is the permanent, much smaller descendant: a handful of selectors,
//! committed as literal hex, so a FUTURE type/interface rename (or an
//! accidental signature edit disguised as a rename) is caught the same way
//! — by a value that cannot silently drift, not by re-deriving the expected
//! answer from the same source under test.
use alloy::sol_types::{SolCall, SolError};
use makinax::chain::module::IMakinaXModule as M;
use makinax::chain::taxonomy::IMakinaXErrors as E;

#[test]
fn a_few_selectors_are_pinned_against_the_real_module_interface() {
    assert_eq!(M::pauseCall::SELECTOR, [0x84, 0x56, 0xcb, 0x59]);
    assert_eq!(M::unpauseCall::SELECTOR, [0x3f, 0x4b, 0xa8, 0x3a]);
    assert_eq!(
        M::setAllowedInstrRootCall::SELECTOR,
        [0xb6, 0xe4, 0xb2, 0xda]
    );
}

#[test]
fn a_few_error_selectors_are_pinned_against_the_real_module_interface() {
    assert_eq!(E::Paused::SELECTOR, [0x9e, 0x87, 0xfa, 0xc8]);
    assert_eq!(E::ZeroAddress::SELECTOR, [0xd9, 0x2e, 0x23, 0x3d]);
}
