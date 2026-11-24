//! The anvil fork command line and binary resolution, from the neutral
//! config alone.
#![cfg(feature = "write")]

use makinax::{
    Error,
    config::ForkConfig,
    sim::fork::{anvil_fork_args, resolve_anvil_path},
};

#[test]
fn the_hardfork_reaches_the_anvil_command_line() {
    let args = anvil_fork_args(&ForkConfig::default(), "cancun");
    let i = args
        .iter()
        .position(|a| a == "--hardfork")
        .unwrap_or_else(|| panic!("--hardfork missing: {args:?}"));
    assert_eq!(args[i + 1], "cancun", "{args:?}");
}

#[test]
fn the_default_hardfork_is_carried_by_the_fork_config() {
    let fork = ForkConfig::default();
    let args = anvil_fork_args(&fork, &fork.hardfork);
    let i = args.iter().position(|a| a == "--hardfork").unwrap();
    assert_eq!(args[i + 1], "osaka", "{args:?}");
}

/// An explicit `fork.anvil_path` that does not exist is refused, naming the
/// path. The refusal is the structured fork-backend error (phase
/// `binary-resolution`), the same shape every other fork-binary failure
/// takes, so a consumer that renders it gets one consistent message.
#[test]
fn an_explicit_missing_anvil_path_is_refused_naming_the_path() {
    let path = std::path::PathBuf::from("/definitely/not/here/anvil-fork-args-test");
    let fork = ForkConfig {
        anvil_path: Some(path.clone()),
        ..Default::default()
    };
    match resolve_anvil_path(&fork) {
        Err(Error::ForkBackend { phase, detail, .. }) => {
            assert_eq!(phase, "binary-resolution");
            assert!(
                detail.contains(&path.display().to_string()),
                "the refusal must name the configured path: {detail}"
            );
        }
        other => panic!("expected a binary-resolution refusal, got {other:?}"),
    }
}
