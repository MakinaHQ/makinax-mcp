//! Simulation tooling for calls to a makinaX module (`write` feature).
//!
//! - [`fork`]: the warm anvil fork lifecycle (spawn, health-ping, re-anchor,
//!   orphan reaping) and the [`fork::ExecTarget`] a simulation is run for.
//! - [`receipts`]: receipts binding a simulated call to the block it was
//!   simulated at, single-use.
//! - [`simulate`]: the simulation entry point (eth_call, fork, Tenderly).
//! - [`gas`]: the gas-limit policy for a simulated call.
//! - [`trace`]: `debug_traceTransaction` call-trace fetch and summary.
//! - [`weiroll_diag`]: decoding a failed weiroll instruction set.
//! - [`timings`]: phase-level wall-clock for a multi-phase call.
//! - `tenderly` (`tenderly` feature): the Tenderly REST simulation client.
//!
//! Nothing here enforces anything: whether a call must be simulated before
//! it is sent, and what a stale receipt means, is the consumer's decision.

pub mod fork;
pub mod gas;
pub mod receipts;
pub mod simulate;
#[cfg(feature = "tenderly")]
pub mod tenderly;
pub mod timings;
pub mod trace;
pub mod weiroll_diag;

pub use fork::{ExecTarget, ForkManager};
pub use receipts::{SimReceipt, SimReceiptStore};
pub use simulate::{SimBackend, SimulationResult, simulate};
