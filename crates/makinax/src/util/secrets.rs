//! Keep keystore passphrases out of child processes.
//!
//! A process built on this crate spawns children (anvil, git, bash,
//! `security`) with the inherited environment, and a keystore passphrase env var is a
//! documented, supported way to supply a signer's passphrase. Left alone,
//! that value reaches every child — including long-lived, orphaned ones.
//!
//! Deliberately a TARGETED removal rather than an allowlist (`env_clear`
//! plus a curated set): an allowlist is stronger in principle, but a spawned
//! binary's environment needs are not obvious, and a guessed one that breaks
//! it surfaces as a product regression rather than as a failing unit test
//! here — the worst place for it to appear.
//!
//! This module knows nothing about any consumer's env-var naming convention:
//! every function here takes the canonical passphrase-env names as a
//! parameter. A consumer keeps its own constant (e.g. its `FOO_KEYSTORE_
//! PASSPHRASE` family) and passes it in — this module is built to feed
//! exactly that kind of consumer-specific wrapper.

use std::process::Command;

/// Combine a product's canonical passphrase-env names with an optional
/// extra name this machine's signer configured (`passphrase_env`), deduped.
pub fn names_with_configured(canonical: &[&str], configured: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = canonical.iter().map(|s| s.to_string()).collect();
    if let Some(c) = configured.map(str::trim).filter(|c| !c.is_empty())
        && !names.iter().any(|n| n == c)
    {
        names.push(c.to_string());
    }
    names
}

/// Remove every passphrase-bearing variable from a child's environment.
///
/// For `std::process::Command` callers, where removal is available.
pub fn scrub(cmd: &mut Command, canonical: &[&str], configured: Option<&str>) {
    for name in names_with_configured(canonical, configured) {
        cmd.env_remove(name);
    }
}

/// The same protection for a builder that can only ADD variables.
///
/// alloy's `Anvil` exposes `.env()`/`.envs()` and no removal, so the secret is
/// SHADOWED with an empty value instead of unset. The child still sees the
/// NAME; it never sees the passphrase, which is the property that matters. If
/// alloy ever grows an `env_remove`, prefer it — the name itself is a small
/// hint worth not leaking.
pub fn empty_overrides(canonical: &[&str], configured: Option<&str>) -> Vec<(String, String)> {
    names_with_configured(canonical, configured)
        .into_iter()
        .map(|n| (n, String::new()))
        .collect()
}

/// The configured `passphrase_env` for a Keystore signer, if any. Other
/// signer kinds carry no passphrase env by construction.
pub fn configured_passphrase_env(signer: &crate::config::SignerConfig) -> Option<&str> {
    match signer {
        crate::config::SignerConfig::Keystore(k) => k.passphrase_env.as_deref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &[&str] = &["EXAMPLE_KEYSTORE_PASSPHRASE", "EXAMPLE_GUARDIAN_PASSPHRASE"];

    #[test]
    fn the_configured_name_is_included_and_never_duplicated() {
        let names = names_with_configured(CANONICAL, Some("MY_CUSTOM_PASS"));
        assert!(names.iter().any(|n| n == "MY_CUSTOM_PASS"));
        assert!(names.iter().any(|n| n == "EXAMPLE_KEYSTORE_PASSPHRASE"));
        // A configured name that IS canonical must not appear twice.
        let dup = names_with_configured(CANONICAL, Some("EXAMPLE_KEYSTORE_PASSPHRASE"));
        assert_eq!(
            dup.iter()
                .filter(|n| *n == "EXAMPLE_KEYSTORE_PASSPHRASE")
                .count(),
            1
        );
        // Empty/whitespace config must not add a nameless entry.
        assert_eq!(
            names_with_configured(CANONICAL, Some("   ")).len(),
            CANONICAL.len()
        );
    }

    /// THE CANARY, end to end: a real child process, spawned through `scrub`,
    /// must not be able to see the passphrase.
    ///
    /// This asserts the PROPERTY (the secret is absent from the child's
    /// environment) rather than that a particular API was called — the latter
    /// would keep passing if `env_remove` stopped working.
    #[test]
    fn a_scrubbed_child_cannot_see_the_passphrase() {
        let canary = "makinax-canary-do-not-leak";
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "env"])
            .env("EXAMPLE_KEYSTORE_PASSPHRASE", canary)
            .env("MY_CUSTOM_PASS", canary)
            // A variable that is NOT a passphrase must survive: a scrub that
            // took the whole environment would break anvil and git.
            .env("EXAMPLE_NOT_A_SECRET", "keep-me");
        scrub(&mut cmd, CANONICAL, Some("MY_CUSTOM_PASS"));
        let out = cmd.output().expect("spawn /bin/sh");
        let env = String::from_utf8_lossy(&out.stdout);
        assert!(
            !env.contains(canary),
            "the passphrase reached the child's environment:\n{env}"
        );
        assert!(
            env.contains("EXAMPLE_NOT_A_SECRET=keep-me"),
            "the scrub removed a non-secret variable — children need their environment:\n{env}"
        );
    }

    #[test]
    fn the_builder_overrides_blank_every_passphrase_name() {
        let ov = empty_overrides(CANONICAL, Some("MY_CUSTOM_PASS"));
        assert!(ov.iter().any(|(k, _)| k == "MY_CUSTOM_PASS"));
        assert!(ov.iter().all(|(_, v)| v.is_empty()), "{ov:?}");
    }

    #[test]
    fn configured_passphrase_env_reads_only_the_keystore_variant() {
        use crate::config::{KeystoreSignerConfig, SignerConfig};
        let keystore = SignerConfig::Keystore(KeystoreSignerConfig {
            path: "/tmp/ks.json".into(),
            passphrase_file: None,
            passphrase_env: Some("EXAMPLE_KEYSTORE_PASSPHRASE".into()),
            keychain_service: None,
        });
        assert_eq!(
            configured_passphrase_env(&keystore),
            Some("EXAMPLE_KEYSTORE_PASSPHRASE")
        );
        assert_eq!(configured_passphrase_env(&SignerConfig::Unsigned), None);
    }
}
