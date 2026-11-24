//! makinax-mcp: an MCP server over [`makinax::ext`].
//!
//! Every tool is a thin wrapper over one `makinax::ext` call: read a
//! module, list its actions, build and prove a call, check fetched data,
//! simulate, encode owner calls and, in the read-write build, send. The
//! server keeps no state of its own: a config file in, tool results out.
//!
//! The read-only build (`--no-default-features`) has no `execute_call` and
//! no signing code at all.

pub mod config;
pub mod service;
mod tools;

pub use service::{ConfigWording, MakinaxService};

/// `<version> (<git sha>)`, the identity `--version` and the MCP handshake
/// report.
pub const BUILD_IDENTITY: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("MAKINAX_GIT_SHA"),
    ")"
);

/// `read-write` or `read-only`: which build this is.
pub const BUILD_VARIANT: &str = if cfg!(feature = "write") {
    "read-write"
} else {
    "read-only"
};

/// The line `makinax-mcp --version` prints.
pub fn version_line() -> String {
    format!("makinax-mcp {BUILD_IDENTITY} [{BUILD_VARIANT}]")
}
