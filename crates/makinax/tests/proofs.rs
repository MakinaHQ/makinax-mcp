//! Proofs for named actions verify against the loaded root, and an action
//! the rootfile lacks is refused by name (review focus 2).

use alloy::primitives::{B256, keccak256};
use makinax::{
    config::RootfileSource,
    error::Error,
    instructions::{InstructionLibrary, proofs::proof_for},
};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

async fn lib() -> InstructionLibrary {
    InstructionLibrary::load(&RootfileSource::Local(fixture("rootfile.toml")))
        .await
        .unwrap()
}

/// Written independently of the library: the transpiler's `MerkleTree`
/// hashes each parent as `keccak256(abi.encode(min(l, r), max(l, r)))`
/// (two static `bytes32`, so plain 64-byte concatenation), i.e.
/// OpenZeppelin's sorted-pair scheme. The leaf is `keccak256(inner hash)`.
fn verify_merkle(leaf: B256, proof: &[B256], root: B256) -> bool {
    proof.iter().fold(leaf, |node, sib| {
        let (lo, hi) = if node < *sib {
            (node, *sib)
        } else {
            (*sib, node)
        };
        keccak256([lo.as_slice(), hi.as_slice()].concat())
    }) == root
}

#[tokio::test]
async fn proof_for_a_known_action_verifies_against_the_root() {
    let lib = lib().await;
    let name = lib.iter().next().unwrap().qualified_name.clone();
    let p = proof_for(&lib, &name).unwrap();
    assert_eq!(p.action, name);
    assert_eq!(p.root, lib.root());
    assert!(verify_merkle(p.leaf_hash, &p.proof, p.root));
}

/// Every action, not only the first: a tree-shape bug shows at particular
/// leaf positions (five leaves pad to eight).
#[tokio::test]
async fn every_action_in_the_fixture_has_a_verifying_proof() {
    let lib = lib().await;
    assert_eq!(lib.count(), 5, "fixture carries five actions");
    for a in lib.iter() {
        let p = proof_for(&lib, &a.qualified_name).unwrap();
        assert_eq!(p.leaf_hash, keccak256(a.instruction.hash()));
        assert!(
            verify_merkle(p.leaf_hash, &p.proof, p.root),
            "{}",
            a.qualified_name
        );
        // A proof for one leaf must not verify another.
        for b in lib.iter().filter(|b| b.leaf_hash != a.leaf_hash) {
            assert!(!verify_merkle(b.leaf_hash, &p.proof, p.root));
        }
    }
}

#[tokio::test]
async fn an_unknown_action_is_refused_by_name() {
    let lib = lib().await;
    let e = proof_for(&lib, "nope.nope").unwrap_err();
    assert!(matches!(e, Error::UnknownAction(ref n) if n == "nope.nope"));
}

#[tokio::test]
async fn an_empty_library_refuses_every_action() {
    let lib = InstructionLibrary::empty("none");
    let name = "morpho.deposit_vault.Gauntlet WETH Balanced";
    let e = proof_for(&lib, name).unwrap_err();
    assert!(matches!(e, Error::UnknownAction(ref n) if n == name));
}
