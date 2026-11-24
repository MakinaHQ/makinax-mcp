//! Owner-call encoding: neutral construction of Safe Tx-Builder batches
//! against `IMakinaXModule`'s owner-restricted functions.
//!
//! - [`encode`]: one fn per owner-restricted function of `IMakinaXModule`,
//!   plus the generic path every producer of an owner call goes through.
//! - [`batch`]: the Tx-Builder batch shape (`OwnerCall`, `TxBuilderBatch`,
//!   `TxEntry`, `ValueCall`, `build_batch`, `build_value_batch`,
//!   `single_call`).
//! - [`dyn_call`]: the generic solidity-signature encoder both of the above
//!   go through.
//!
//! - [`sim`] (`write` feature): executes a batch AS the Safe on a warm
//!   anvil fork (`sim::fork`), so a consumer can prove a batch executable
//!   before handing it to the Safe's owners.
//!
//! Calls built here are ENCODED ONLY — never sent to a live chain. The fork
//! session only ever runs them against a local, disposable fork.

pub mod batch;
pub mod dyn_call;
pub mod encode;
#[cfg(feature = "write")]
pub mod sim;
