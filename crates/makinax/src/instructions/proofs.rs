//! Merkle proofs for named actions, and the check that the loaded root is
//! the one the module enforces.
//!
//! Both refuse explicitly rather than hand back a proof that would revert:
//! an action absent from the rootfile is [`Error::UnknownAction`], and a
//! rootfile whose root differs from the module's `allowedInstrRoot` is
//! [`Error::RootMismatch`] carrying both roots.

use alloy::{
    network::Ethereum,
    primitives::{Address, B256, keccak256},
    providers::Provider,
};
use serde::Serialize;

use super::{
    library::InstructionLibrary,
    verify::{fetch_onchain_root, matches},
};
use crate::error::{Error, Result};

/// Everything needed to prove one action is whitelisted under `root`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProofBundle {
    /// The qualified name, `protocol.action.token`.
    pub action: String,
    /// The on-chain leaf: `keccak256` of the instruction's inner hash.
    pub leaf_hash: B256,
    /// Sibling hashes from the leaf up to (not including) the root.
    pub proof: Vec<B256>,
    /// The root the proof verifies against: the loaded library's root.
    pub root: B256,
}

/// The proof for `qualified_name` in `lib`.
///
/// Refuses with [`Error::UnknownAction`] (carrying the name as given) when
/// the rootfile has no such action. The proof is checked against the
/// library's root before it is returned, so a proof that does not fold to
/// the root is an [`Error::Merkle`], never a bundle.
pub fn proof_for(lib: &InstructionLibrary, qualified_name: &str) -> Result<ProofBundle> {
    let action = lib.get(qualified_name)?;
    let root = lib.root();
    if !folds_to_root(action.leaf_hash, &action.proof, root) {
        return Err(Error::Merkle(format!(
            "the proof for `{}` does not verify against the loaded root {root}",
            action.qualified_name
        )));
    }
    Ok(ProofBundle {
        action: action.qualified_name.clone(),
        leaf_hash: action.leaf_hash,
        proof: action.proof.clone(),
        root,
    })
}

/// Compare the loaded library's root with the module's `allowedInstrRoot`.
///
/// `Ok(root)` when they agree. When they differ, [`Error::RootMismatch`]
/// with both roots as `0x`-prefixed hex. The zero root never agrees (see
/// [`matches`]): an empty library is refused as [`Error::Rootfile`], since
/// it whitelists nothing and there is nothing to compare.
pub async fn check_root<P: Provider<Ethereum>>(
    lib: &InstructionLibrary,
    provider: &P,
    module: Address,
) -> Result<B256> {
    let loaded = lib.root();
    if loaded == B256::ZERO {
        return Err(Error::Rootfile(format!(
            "the loaded rootfile ({}) whitelists no action (zero root), so it cannot match any module root",
            lib.source_label()
        )));
    }
    let onchain = fetch_onchain_root(provider, module).await?;
    if matches(loaded, onchain) {
        Ok(onchain)
    } else {
        Err(Error::RootMismatch {
            loaded: loaded.to_string(),
            onchain: onchain.to_string(),
        })
    }
}

/// Fold `proof` onto `leaf` with sorted-pair `keccak256`, the pairing the
/// transpiler's `MerkleTree` builds with, and compare to `root`.
fn folds_to_root(leaf: B256, proof: &[B256], root: B256) -> bool {
    let mut node = leaf;
    for sibling in proof {
        let (a, b) = if node < *sibling {
            (node, *sibling)
        } else {
            (*sibling, node)
        };
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(a.as_slice());
        buf[32..].copy_from_slice(b.as_slice());
        node = keccak256(buf);
    }
    node == root
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::providers::{ProviderBuilder, mock::Asserter};

    fn fixture() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rootfile.toml")
    }

    async fn lib() -> InstructionLibrary {
        InstructionLibrary::load(&crate::config::RootfileSource::Local(fixture()))
            .await
            .expect("fixture loads")
    }

    const MODULE: Address = Address::repeat_byte(0x11);

    /// Review focus 2: a rootfile whose root differs from the module's is a
    /// structured refusal carrying BOTH roots, never a proof that reverts.
    #[tokio::test]
    async fn a_different_onchain_root_is_a_mismatch_with_both_roots() {
        let lib = lib().await;
        let other = B256::repeat_byte(0xab);
        let a = Asserter::new();
        a.push_success(&other);
        let p = ProviderBuilder::new().connect_mocked_client(a);
        let e = check_root(&lib, &p, MODULE).await.unwrap_err();
        match e {
            Error::RootMismatch { loaded, onchain } => {
                assert_eq!(loaded, format!("0x{}", alloy::hex::encode(lib.root())));
                assert_eq!(onchain, format!("0x{}", "ab".repeat(32)));
            }
            other => panic!("expected RootMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_same_onchain_root_is_accepted() {
        let lib = lib().await;
        let a = Asserter::new();
        a.push_success(&lib.root());
        let p = ProviderBuilder::new().connect_mocked_client(a);
        assert_eq!(check_root(&lib, &p, MODULE).await.unwrap(), lib.root());
    }

    /// An unset on-chain root (zero) never matches a real library.
    #[tokio::test]
    async fn an_unset_onchain_root_is_a_mismatch() {
        let lib = lib().await;
        let a = Asserter::new();
        a.push_success(&B256::ZERO);
        let p = ProviderBuilder::new().connect_mocked_client(a);
        let e = check_root(&lib, &p, MODULE).await.unwrap_err();
        assert!(
            matches!(e, Error::RootMismatch { ref onchain, .. } if *onchain == format!("0x{}", "00".repeat(32))),
            "{e:?}"
        );
    }

    /// An empty library never matches anything, and is refused before any
    /// RPC is made (the asserter has nothing queued).
    #[tokio::test]
    async fn an_empty_library_is_refused_without_a_read() {
        let lib = InstructionLibrary::empty("none");
        let p = ProviderBuilder::new().connect_mocked_client(Asserter::new());
        let e = check_root(&lib, &p, MODULE).await.unwrap_err();
        assert!(
            matches!(e, Error::Rootfile(ref m) if m.contains("zero root")),
            "{e:?}"
        );
    }

    /// A failed read is an RPC error, not a mismatch and not a match.
    #[tokio::test]
    async fn a_failed_read_is_an_rpc_error() {
        let lib = lib().await;
        let a = Asserter::new();
        a.push_failure_msg("boom");
        let p = ProviderBuilder::new().connect_mocked_client(a);
        let e = check_root(&lib, &p, MODULE).await.unwrap_err();
        assert!(
            matches!(e, Error::Rpc(ref m) if m.contains("allowedInstrRoot")),
            "{e:?}"
        );
    }

    #[test]
    fn a_wrong_proof_does_not_fold_to_the_root() {
        let l = B256::repeat_byte(1);
        let r = B256::repeat_byte(2);
        let mut buf = [0u8; 64];
        buf[..32].copy_from_slice(l.as_slice());
        buf[32..].copy_from_slice(r.as_slice());
        let root = keccak256(buf);
        assert!(folds_to_root(l, &[r], root));
        assert!(
            folds_to_root(r, &[l], root),
            "pair order is sorted, not positional"
        );
        assert!(!folds_to_root(l, &[B256::repeat_byte(3)], root));
        assert!(!folds_to_root(l, &[], root));
    }
}
