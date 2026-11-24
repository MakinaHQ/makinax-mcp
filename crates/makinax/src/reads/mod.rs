//! Reads of a makinaX module's own state: token balances of its Safe, its
//! oracle routes and prices, its configuration and operating state, and its
//! own emitted events over a bounded, live range; and the Merkl Distributor
//! a harvest leaf claims from.

pub mod balances;
pub mod events;
pub mod merkl;
pub mod module_state;
pub mod oracle;

/// The RPC range-too-large / result-limit classifier `read_module_events`
/// uses, re-exported here so a caller's own log-range negotiation can share
/// it by its shorter path (`makinax::reads::is_range_error`) instead of
/// keeping a second copy of the same pattern list.
pub use events::is_range_error;
