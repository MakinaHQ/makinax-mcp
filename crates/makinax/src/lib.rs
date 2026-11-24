//! makinax: read, build, simulate and send calls to a makinaX module.

pub mod bridges;
pub mod build;
pub mod chain;
pub mod chains;
pub mod config;
pub mod error;
/// Sending, always behind a caller-supplied pre-send hook. Only with `write`:
/// a build without it has no way to sign or broadcast.
#[cfg(feature = "write")]
pub mod exec;
/// The single supported integration API. See the module docs.
pub mod ext;
pub mod fetch;
pub mod instructions;
pub mod owner;
pub mod reads;
#[cfg(feature = "write")]
pub mod sim;
pub mod util;

pub use error::{Error, Result};
