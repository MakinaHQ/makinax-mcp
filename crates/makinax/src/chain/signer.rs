//! Signer construction & custody defaults.
//!
//! - `keystore` (default recommendation): encrypted local JSON keystore,
//!   passphrase from a 0600 file, env var, or the macOS Keychain.
//! - `gcp`: GCP KMS via alloy-signer-gcp.
//! - `private_key` (env var): REFUSED unless the caller explicitly allows it
//!   (a dev/test run) — unencrypted keys in env vars are not acceptable
//!   custody for real funds.

use std::str::FromStr;

use alloy::{network::EthereumWallet, signers::local::PrivateKeySigner};

use crate::{
    config::SignerConfig,
    error::{Error, Result},
};

/// Lightweight description of the configured signer for `health_check`.
#[derive(Debug, Clone)]
pub struct SignerInfo {
    pub kind: &'static str,
    pub configured: bool,
    pub note: String,
}

pub fn describe(cfg: &SignerConfig) -> SignerInfo {
    match cfg {
        SignerConfig::Gcp(g) => SignerInfo {
            kind: "gcp",
            configured: !g.project_id.is_empty() && !g.keyring.is_empty() && !g.key_name.is_empty(),
            note: format!(
                "project={} location={} keyring={} key={} v{}",
                g.project_id, g.location, g.keyring, g.key_name, g.key_version
            ),
        },
        SignerConfig::Keystore(k) => SignerInfo {
            kind: "keystore",
            configured: k.path.exists(),
            note: if k.path.exists() {
                format!("keystore at {}", k.path.display())
            } else {
                // Neutral: this crate has no keystore-creation subcommand of
                // its own to name. A consumer that has one restores its
                // exact wording via its own wrapper around `describe`.
                format!("keystore file {} does NOT exist", k.path.display())
            },
        },
        SignerConfig::PrivateKey(p) => {
            let present = std::env::var(&p.env).is_ok();
            SignerInfo {
                kind: "private_key",
                configured: present,
                note: if present {
                    format!("env `{}` is set (dev-mode only)", p.env)
                } else {
                    format!("env `{}` is NOT set", p.env)
                },
            }
        }
        SignerConfig::Unsigned => SignerInfo {
            kind: "unsigned",
            configured: true,
            note: "read-only signer; no writes possible".to_string(),
        },
    }
}

/// Classify signers for the mainnet gate: keystore and KMS qualify; raw
/// private keys and unsigned never do.
pub fn is_non_dev_signer(cfg: &SignerConfig) -> bool {
    matches!(cfg, SignerConfig::Keystore(_) | SignerConfig::Gcp(_))
}

/// The `security find-generic-password` invocation for `service`, scrubbed
/// of `extra_scrub` (a consumer's OTHER configured signers' canonical
/// passphrase-env names — this crate has no notion of that convention, so
/// the consumer supplies it) plus `configured` (this ONE config's own
/// `passphrase_env`, if any).
///
/// Building the command needs no macOS-specific API — only running it does
/// — so this is exercised directly in tests on any platform via
/// `Command::get_envs()`, without spawning `security` at all.
pub fn keychain_lookup_command(
    service: &str,
    extra_scrub: &[&str],
    configured: Option<&str>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new("security");
    cmd.args(["find-generic-password", "-s", service, "-w"]);
    crate::util::secrets::scrub(&mut cmd, extra_scrub, configured);
    cmd
}

/// Resolve a keystore's decryption passphrase. `extra_scrub` is scrubbed
/// from the macOS Keychain lookup's child environment alongside this
/// config's own `passphrase_env` — a consumer with OTHER configured signers
/// (each with its own canonical passphrase-env name) passes them all here,
/// so a keychain lookup for ONE signer never leaks a sibling's passphrase to
/// the `security` child process.
pub fn resolve_keystore_passphrase(
    k: &crate::config::KeystoreSignerConfig,
    extra_scrub: &[&str],
) -> Result<String> {
    // The passphrase FILE is the headless, cross-platform default —
    // read it FIRST. A file that is not 0600 is refused: a group/world
    // readable passphrase beside the key defeats the only boundary that
    // matters here.
    if let Some(pf) = &k.passphrase_file {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(pf)
                .map_err(|e| Error::Config(format!("passphrase_file {}: {e}", pf.display())))?;
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                return Err(Error::Config(format!(
                    "passphrase_file {} is {mode:o}, must be 0600 (a passphrase readable by anyone but you is not custody) — run `chmod 600 {}`",
                    pf.display(),
                    pf.display()
                )));
            }
        }
        let p = std::fs::read_to_string(pf)
            .map_err(|e| Error::Config(format!("passphrase_file {}: {e}", pf.display())))?;
        return Ok(p.trim_end_matches(['\n', '\r']).to_string());
    }
    if let Some(env) = &k.passphrase_env
        && let Ok(p) = std::env::var(env)
    {
        return Ok(p);
    }
    #[cfg(target_os = "macos")]
    if let Some(service) = &k.keychain_service {
        // The keychain lookup exists BECAUSE the passphrase should
        // not be in the environment — handing it to the child that fetches it
        // would defeat the point.
        let mut cmd = keychain_lookup_command(service, extra_scrub, k.passphrase_env.as_deref());
        let out = cmd
            .output()
            .map_err(|e| Error::Config(format!("keychain lookup failed: {}", e)))?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
        }
    }
    // Only the macOS Keychain lookup spawns a child process to scrub.
    #[cfg(not(target_os = "macos"))]
    let _ = extra_scrub;
    Err(Error::Config(KEYSTORE_PASSPHRASE_UNAVAILABLE.into()))
}

/// Neutral refusal when no keystore passphrase source is configured. A
/// consumer with its own vocabulary (e.g. a `keystore-create` subcommand)
/// restores its exact wording by matching on this exact constant — never by
/// guessing at substrings — in its own wrapper around
/// [`resolve_keystore_passphrase`].
pub const KEYSTORE_PASSPHRASE_UNAVAILABLE: &str = "keystore passphrase unavailable: set `passphrase_file` (0600, the headless default), `passphrase_env`, or `keychain_service` (macOS desktop only)";

/// Neutral refusal for a `private_key` signer when `allow_private_key` is
/// false. A consumer with its own vocabulary (a `--dev` flag, a
/// `keystore-create` subcommand) restores its exact wording by matching on
/// this exact constant — never by guessing at substrings — and replacing it
/// in its own wrapper around [`build_wallet_scrubbing`] /
/// [`build_hash_signer_scrubbing`].
pub const PRIVATE_KEY_REFUSED: &str = "raw private_key signer is refused unless explicitly allowed: keys in env vars are unencrypted and easily exfiltrated. Use a keystore or GCP KMS signer instead, or pass allow_private_key: true for a disposable dev/test key.";

/// Build an `EthereumWallet` from the signer config, or `None` if the config is `unsigned`.
///
/// Errors only when a signer IS configured but cannot be constructed
/// (missing env, bad key, refused custody type, etc.). Equivalent to
/// [`build_wallet_scrubbing`] with an empty `extra_scrub`.
pub async fn build_wallet(
    cfg: &SignerConfig,
    allow_private_key: bool,
) -> Result<Option<EthereumWallet>> {
    build_wallet_scrubbing(cfg, allow_private_key, &[]).await
}

/// Same as [`build_wallet`], but scrubs `extra_scrub` from a macOS Keychain
/// lookup's child environment too — see [`resolve_keystore_passphrase`].
pub async fn build_wallet_scrubbing(
    cfg: &SignerConfig,
    allow_private_key: bool,
    extra_scrub: &[&str],
) -> Result<Option<EthereumWallet>> {
    match cfg {
        SignerConfig::Unsigned => Ok(None),
        SignerConfig::PrivateKey(p) => {
            if !allow_private_key {
                return Err(Error::Config(PRIVATE_KEY_REFUSED.into()));
            }
            let raw = std::env::var(&p.env).map_err(|_| {
                Error::Config(format!("private_key signer requires env `{}`", p.env))
            })?;
            let signer = PrivateKeySigner::from_str(raw.trim())
                .map_err(|e| Error::Config(format!("invalid private key: {}", e)))?;
            Ok(Some(EthereumWallet::from(signer)))
        }
        SignerConfig::Keystore(k) => {
            let pass = resolve_keystore_passphrase(k, extra_scrub)?;
            let signer = PrivateKeySigner::decrypt_keystore(&k.path, pass).map_err(|e| {
                Error::Config(format!("keystore decrypt {}: {}", k.path.display(), e))
            })?;
            Ok(Some(EthereumWallet::from(signer)))
        }
        SignerConfig::Gcp(g) => {
            use alloy::signers::gcp::{GcpKeyRingRef, GcpSigner, KeySpecifier};
            use gcloud_sdk::{
                GoogleApi,
                google::cloud::kms::v1::key_management_service_client::KeyManagementServiceClient,
            };
            let keyring = GcpKeyRingRef::new(&g.project_id, &g.location, &g.keyring);
            let client = GoogleApi::from_function(
                KeyManagementServiceClient::new,
                "https://cloudkms.googleapis.com",
                None,
            )
            .await
            .map_err(|e| Error::Config(format!("GCP KMS client: {}", e)))?;
            let specifier = KeySpecifier::new(keyring, &g.key_name, g.key_version as u64);
            let signer = GcpSigner::new(client, specifier, None)
                .await
                .map_err(|e| Error::Config(format!("GCP KMS signer: {}", e)))?;
            Ok(Some(EthereumWallet::from(signer)))
        }
    }
}

/// A signer that can sign a RAW HASH (`Signer::sign_hash`), e.g. a Safe
/// transaction hash. Same three arms as
/// [`build_wallet`], same custody rules; `None` for `unsigned`. Equivalent
/// to [`build_hash_signer_scrubbing`] with an empty `extra_scrub`.
pub async fn build_hash_signer(
    cfg: &SignerConfig,
    allow_private_key: bool,
) -> Result<Option<Box<dyn alloy::signers::Signer + Send + Sync>>> {
    build_hash_signer_scrubbing(cfg, allow_private_key, &[]).await
}

/// Same as [`build_hash_signer`], but scrubs `extra_scrub` from a macOS
/// Keychain lookup's child environment too — see
/// [`resolve_keystore_passphrase`].
pub async fn build_hash_signer_scrubbing(
    cfg: &SignerConfig,
    allow_private_key: bool,
    extra_scrub: &[&str],
) -> Result<Option<Box<dyn alloy::signers::Signer + Send + Sync>>> {
    match cfg {
        SignerConfig::Unsigned => Ok(None),
        SignerConfig::PrivateKey(p) => {
            if !allow_private_key {
                return Err(Error::Config(PRIVATE_KEY_REFUSED.into()));
            }
            let raw = std::env::var(&p.env).map_err(|_| {
                Error::Config(format!("private_key signer requires env `{}`", p.env))
            })?;
            let signer = PrivateKeySigner::from_str(raw.trim())
                .map_err(|e| Error::Config(format!("invalid private key: {}", e)))?;
            Ok(Some(Box::new(signer)))
        }
        SignerConfig::Keystore(k) => {
            let pass = resolve_keystore_passphrase(k, extra_scrub)?;
            let signer = PrivateKeySigner::decrypt_keystore(&k.path, pass).map_err(|e| {
                Error::Config(format!("keystore decrypt {}: {}", k.path.display(), e))
            })?;
            Ok(Some(Box::new(signer)))
        }
        SignerConfig::Gcp(g) => {
            use alloy::signers::gcp::{GcpKeyRingRef, GcpSigner, KeySpecifier};
            use gcloud_sdk::{
                GoogleApi,
                google::cloud::kms::v1::key_management_service_client::KeyManagementServiceClient,
            };
            let keyring = GcpKeyRingRef::new(&g.project_id, &g.location, &g.keyring);
            let client = GoogleApi::from_function(
                KeyManagementServiceClient::new,
                "https://cloudkms.googleapis.com",
                None,
            )
            .await
            .map_err(|e| Error::Config(format!("GCP KMS client: {}", e)))?;
            let specifier = KeySpecifier::new(keyring, &g.key_name, g.key_version as u64);
            let signer = GcpSigner::new(client, specifier, None)
                .await
                .map_err(|e| Error::Config(format!("GCP KMS signer: {}", e)))?;
            Ok(Some(Box::new(signer)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KeystoreSignerConfig, PrivateKeySignerConfig, SignerConfig};

    #[tokio::test]
    async fn private_key_refused_without_allow_private_key() {
        let cfg = SignerConfig::PrivateKey(PrivateKeySignerConfig {
            env: "TEST_PK_GATE".into(),
        });
        let err = build_wallet(&cfg, false).await.unwrap_err();
        assert_eq!(err.to_string(), format!("config: {PRIVATE_KEY_REFUSED}"));
    }

    #[tokio::test]
    async fn keystore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = rand::thread_rng();
        let (signer, _uuid) =
            PrivateKeySigner::new_keystore(dir.path(), &mut rng, "hunter2", Some("operator.json"))
                .unwrap();
        let expected = signer.address();

        // edition-2024: set_var is unsafe (process-global mutation; fine in this single test)
        unsafe { std::env::set_var("TEST_KS_PASS", "hunter2") };
        let cfg = SignerConfig::Keystore(KeystoreSignerConfig {
            path: dir.path().join("operator.json"),
            passphrase_file: None,
            passphrase_env: Some("TEST_KS_PASS".into()),
            keychain_service: None,
        });
        let wallet = build_wallet(&cfg, false).await.unwrap().unwrap();
        use alloy::network::NetworkWallet;
        let addr =
            <EthereumWallet as NetworkWallet<alloy::network::Ethereum>>::default_signer_address(
                &wallet,
            );
        assert_eq!(addr, expected);
    }

    /// Acceptance 1/3: the passphrase FILE is read first, and a
    /// file that is not 0600 is REFUSED (headless custody without a boundary
    /// is not custody).
    #[cfg(unix)]
    #[test]
    fn passphrase_file_is_read_first_and_must_be_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("operator.pass");
        std::fs::write(&pf, "file-secret\n").unwrap();
        std::fs::set_permissions(&pf, std::fs::Permissions::from_mode(0o600)).unwrap();

        let k = KeystoreSignerConfig {
            path: dir.path().join("k.json"),
            passphrase_file: Some(pf.clone()),
            // Env is ALSO set but must be ignored — the file wins.
            passphrase_env: Some("CC119_SHOULD_NOT_BE_READ".into()),
            keychain_service: None,
        };
        unsafe { std::env::set_var("CC119_SHOULD_NOT_BE_READ", "env-secret") };
        assert_eq!(resolve_keystore_passphrase(&k, &[]).unwrap(), "file-secret");

        // Loosen to 0644 → refused, naming the mode and the fix.
        std::fs::set_permissions(&pf, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = resolve_keystore_passphrase(&k, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("0600") && err.contains("chmod"), "{err}");
    }

    #[test]
    fn non_dev_signer_classification() {
        assert!(is_non_dev_signer(&SignerConfig::Keystore(
            KeystoreSignerConfig {
                path: "/tmp/x.json".into(),
                passphrase_file: None,
                passphrase_env: None,
                keychain_service: None,
            }
        )));
        assert!(!is_non_dev_signer(&SignerConfig::PrivateKey(
            PrivateKeySignerConfig { env: "X".into() }
        )));
        assert!(!is_non_dev_signer(&SignerConfig::Unsigned));
    }

    /// The renamed parameter is the caller's own explicit allowance, not a
    /// server-wide "dev mode" flag this crate has any notion of — with it
    /// `true`, the same private_key signer that was refused above succeeds.
    #[tokio::test]
    async fn private_key_succeeds_when_the_caller_allows_it() {
        // Any valid 32-byte scalar does; hex-encode a fixed byte pattern
        // rather than a hand-typed literal so the length can't drift.
        let key_hex = alloy::hex::encode([0x11u8; 32]);
        let expected = PrivateKeySigner::from_str(&key_hex).unwrap().address();
        unsafe { std::env::set_var("TEST_PK_ALLOWED", &key_hex) };
        let cfg = SignerConfig::PrivateKey(PrivateKeySignerConfig {
            env: "TEST_PK_ALLOWED".into(),
        });
        let wallet = build_wallet(&cfg, true).await.unwrap().unwrap();
        use alloy::network::NetworkWallet;
        let addr =
            <EthereumWallet as NetworkWallet<alloy::network::Ethereum>>::default_signer_address(
                &wallet,
            );
        assert_eq!(addr, expected);
    }

    /// `keychain_lookup_command` needs no macOS API to BUILD the command —
    /// `Command::get_envs()` (cross-platform, stable std) exposes which
    /// names were marked for removal without ever spawning `security`.
    #[test]
    fn keychain_lookup_command_scrubs_extra_and_configured_names() {
        let cmd = keychain_lookup_command(
            "some-service",
            &["A_SIBLINGS_PASSPHRASE", "ANOTHER_SIBLINGS_PASSPHRASE"],
            Some("THIS_CONFIGS_OWN_PASSPHRASE"),
        );
        let removed: Vec<&str> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_str().unwrap())
            .collect();
        assert!(removed.contains(&"A_SIBLINGS_PASSPHRASE"), "{removed:?}");
        assert!(
            removed.contains(&"ANOTHER_SIBLINGS_PASSPHRASE"),
            "{removed:?}"
        );
        assert!(
            removed.contains(&"THIS_CONFIGS_OWN_PASSPHRASE"),
            "{removed:?}"
        );
    }

    /// The regression this pins against: a keychain lookup for ONE signer
    /// must scrub a SIBLING signer's canonical passphrase name too, when the
    /// consumer supplies it as `extra_scrub` — not just the name this one
    /// config points at.
    #[test]
    fn resolve_keystore_passphrase_scrubs_a_sibling_when_given_as_extra() {
        let canary = "sibling-scrub-canary-do-not-leak";
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.args(["-c", "env"])
            .env("SIBLING_SIGNERS_PASSPHRASE", canary)
            .env("KEPT_VAR", "keep-me");
        // The exact mechanism `resolve_keystore_passphrase`'s keychain branch
        // uses: scrub with extra_scrub = the consumer's OTHER canonical names.
        crate::util::secrets::scrub(&mut cmd, &["SIBLING_SIGNERS_PASSPHRASE"], None);
        let out = cmd.output().unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        assert!(!env.contains(canary), "{env}");
        assert!(env.contains("KEPT_VAR=keep-me"), "{env}");
    }

    /// Neutrality: this crate names no command-line tool. `describe`'s
    /// keystore-not-found note and every refusal text it can produce must
    /// never mention a `makinax `-prefixed subcommand or a `--dev` flag; a
    /// caller that wants its own wording wraps these texts.
    #[test]
    fn neutral_texts_name_no_consumer_cli() {
        let missing_keystore = describe(&SignerConfig::Keystore(KeystoreSignerConfig {
            path: "/tmp/definitely-does-not-exist-signer-test.json".into(),
            passphrase_file: None,
            passphrase_env: None,
            keychain_service: None,
        }))
        .note;
        for text in [
            missing_keystore.as_str(),
            KEYSTORE_PASSPHRASE_UNAVAILABLE,
            PRIVATE_KEY_REFUSED,
        ] {
            assert!(!text.contains("`makinax "), "{text}");
            assert!(!text.contains("--dev"), "{text}");
        }
    }

    /// `describe` reports each signer kind, whether it is usable as
    /// configured, and why not.
    #[test]
    fn describe_reports_kind_and_whether_it_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("k.json");
        std::fs::write(&present, "{}").unwrap();
        let keystore = |path: std::path::PathBuf| {
            SignerConfig::Keystore(KeystoreSignerConfig {
                path,
                passphrase_file: None,
                passphrase_env: None,
                keychain_service: None,
            })
        };
        let info: SignerInfo = describe(&keystore(present.clone()));
        assert_eq!((info.kind, info.configured), ("keystore", true));
        assert!(info.note.contains(&present.display().to_string()));
        let info: SignerInfo = describe(&keystore(dir.path().join("absent.json")));
        assert_eq!((info.kind, info.configured), ("keystore", false));
        assert!(info.note.contains("does NOT exist"), "{}", info.note);

        let pk = SignerConfig::PrivateKey(PrivateKeySignerConfig {
            env: "TEST_DESCRIBE_PK_NEVER_SET".into(),
        });
        let info: SignerInfo = describe(&pk);
        assert_eq!((info.kind, info.configured), ("private_key", false));
        assert!(info.note.contains("is NOT set"), "{}", info.note);

        let info: SignerInfo = describe(&SignerConfig::Unsigned);
        assert_eq!((info.kind, info.configured), ("unsigned", true));
    }

    /// The `_scrubbing` builders, given a scrub list, still build the
    /// configured signer: a wallet and a raw-hash signer for one keystore
    /// both carry its address, and the custody gate and the unsigned arm hold
    /// for both. What the scrub removes from the keychain child is pinned by
    /// `keychain_lookup_command_scrubs_extra_and_configured_names`. No
    /// signature is made here; the address is the check.
    #[tokio::test]
    async fn scrubbing_builders_with_a_scrub_list_build_the_configured_signer() {
        use alloy::network::NetworkWallet;
        let dir = tempfile::tempdir().unwrap();
        let mut rng = rand::thread_rng();
        let (signer, _) =
            PrivateKeySigner::new_keystore(dir.path(), &mut rng, "s3cret", Some("op.json"))
                .unwrap();
        let expected = signer.address();
        unsafe { std::env::set_var("TEST_SCRUB_KS_PASS", "s3cret") };
        let cfg = SignerConfig::Keystore(KeystoreSignerConfig {
            path: dir.path().join("op.json"),
            passphrase_file: None,
            passphrase_env: Some("TEST_SCRUB_KS_PASS".into()),
            keychain_service: None,
        });
        let scrub = ["A_SIBLINGS_PASSPHRASE"];

        let wallet = build_wallet_scrubbing(&cfg, false, &scrub)
            .await
            .unwrap()
            .unwrap();
        let addr =
            <EthereumWallet as NetworkWallet<alloy::network::Ethereum>>::default_signer_address(
                &wallet,
            );
        assert_eq!(addr, expected);

        let hash_signer = build_hash_signer_scrubbing(&cfg, false, &scrub)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hash_signer.address(), expected);

        let pk = SignerConfig::PrivateKey(PrivateKeySignerConfig {
            env: "TEST_SCRUB_PK_GATE".into(),
        });
        assert!(build_wallet_scrubbing(&pk, false, &scrub).await.is_err());
        assert!(
            build_hash_signer_scrubbing(&pk, false, &scrub)
                .await
                .is_err()
        );
        let none = SignerConfig::Unsigned;
        assert!(
            build_wallet_scrubbing(&none, true, &scrub)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            build_hash_signer_scrubbing(&none, true, &scrub)
                .await
                .unwrap()
                .is_none()
        );
    }
}
