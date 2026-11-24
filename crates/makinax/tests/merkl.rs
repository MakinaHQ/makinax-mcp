//! A Merkl harvest leaf filled with claim words: the leaf and its proof are
//! unchanged, the plain builder still refuses it, and the Distributor is
//! read from the leaf.

use alloy::primitives::{Address, B256, Bytes, U256, address, b256, keccak256};
use makinax::ext::fetch::merkl::{ClaimData, MERKL_CLAIM_META_TYPE};
use makinax::ext::instructions::{
    MetaFill, ParamMap, build_instruction, build_instruction_with_meta,
};
use makinax::ext::reads::merkl::distributor_of;
use makinax::ext::{Action, Error, InstructionLibrary, RootfileSource, proof_for};

const LEAF: &str = "merkl.harvest.Merkl";
const META: &str = "merkl_claim_data";
const DISTRIBUTOR: Address = address!("0x3Ef3D8bA38EBe18DB133cEc108f4D14CE00Dd9Ae");
const SAFE: Address = address!("0xd1dA9924C8eA15A8E35951b4702Aa4937A9dd70a");

async fn lib() -> InstructionLibrary {
    InstructionLibrary::load(&RootfileSource::Local(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/merkl/rootfile.toml"),
    ))
    .await
    .expect("fixture loads")
}

fn claim() -> ClaimData {
    ClaimData {
        users: vec![SAFE, SAFE],
        tokens: vec![
            address!("0x194f360D130F2393a5E9F3117A6a1B78aBEa1624"),
            address!("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"),
        ],
        amounts: vec![U256::from(65641641704373615677u128), U256::from(716109u64)],
        proofs: vec![
            vec![b256!(
                "0x6fb2cdaa1926ebddc0f4ced4dadf92b320585cb57250a69a04d84563533a823e"
            )],
            vec![],
        ],
    }
}

fn refusal(r: makinax::ext::Result<impl Sized>) -> Error {
    match r {
        Err(e) => e,
        Ok(_) => panic!("built, expected a refusal"),
    }
}

/// The transpiler's leaf hash of `action`'s template with `state` swapped in.
fn leaf_with_state(action: &Action, state: &[Bytes]) -> B256 {
    let mut inst = action.instruction.clone();
    inst.state = state.to_vec();
    keccak256(inst.hash().as_slice())
}

#[tokio::test]
async fn the_fixture_leaf_declares_merkl_claim_slots() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    assert_eq!(a.instruction.bitmap.to::<u128>(), 0);
    let fields: Vec<_> = a
        .instruction
        .inputs_slots
        .iter()
        .map(|s| {
            let mt = s.meta_type.as_ref().expect("meta slot");
            assert_eq!(
                (mt.r#type.as_str(), mt.name.as_str()),
                (MERKL_CLAIM_META_TYPE, META)
            );
            (s.index, mt.field.as_str())
        })
        .collect();
    assert_eq!(
        fields,
        [(0, "users"), (1, "tokens"), (2, "amounts"), (3, "proofs")]
    );
}

#[tokio::test]
async fn filling_writes_the_words_and_keeps_the_leaf_and_proof() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    let words = claim().to_map();
    let inst =
        build_instruction_with_meta(a, &ParamMap::new(), &claim().to_meta_fill(META)).unwrap();

    assert_eq!(inst.state[0], words["users"]);
    assert_eq!(inst.state[1], words["tokens"]);
    assert_eq!(inst.state[2], words["amounts"]);
    assert_eq!(inst.state[3], words["proofs"]);
    assert_ne!(
        inst.state, a.instruction.state,
        "placeholders were replaced"
    );

    assert_eq!(
        leaf_with_state(a, &inst.state),
        a.leaf_hash,
        "leaf hash unchanged"
    );
    assert_eq!(leaf_with_state(a, &a.instruction.state), a.leaf_hash);
    assert_eq!(inst.merkleProof, a.proof);
    let bundle = proof_for(&lib, LEAF).unwrap();
    assert_eq!(bundle.leaf_hash, a.leaf_hash);
    assert_eq!(inst.commands, a.instruction.commands);
    assert_eq!(inst.stateBitmap, 0);
    assert_eq!(inst.instructionType, 2);
}

#[tokio::test]
async fn the_plain_builder_still_refuses_meta_slots() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    match build_instruction(a, &ParamMap::new()) {
        Err(Error::MetaTypeSlot { action, slot }) => {
            assert_eq!(
                (action.as_str(), slot.as_str()),
                (LEAF, "merkl_claim_data.users")
            )
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("built"),
    }
}

#[tokio::test]
async fn an_unfilled_meta_slot_is_refused() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    let mut fill = claim().to_meta_fill(META);
    fill.get_mut(META).unwrap().remove("proofs");
    match build_instruction_with_meta(a, &ParamMap::new(), &fill) {
        Err(Error::MetaTypeSlot { slot, .. }) => assert_eq!(slot, "merkl_claim_data.proofs"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("built"),
    }
    assert!(matches!(
        build_instruction_with_meta(a, &ParamMap::new(), &MetaFill::new()),
        Err(Error::MetaTypeSlot { .. })
    ));
}

#[tokio::test]
async fn a_word_for_no_slot_is_refused() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    let mut fill = claim().to_meta_fill(META);
    fill.get_mut(META)
        .unwrap()
        .insert("recipient".into(), Bytes::new());
    let e = refusal(build_instruction_with_meta(a, &ParamMap::new(), &fill));
    assert!(
        e.to_string()
            .contains("no meta-type slot `merkl_claim_data.recipient`"),
        "{e}"
    );
    let fill = claim().to_meta_fill("other_meta");
    assert!(build_instruction_with_meta(a, &ParamMap::new(), &fill).is_err());
}

#[tokio::test]
async fn a_param_for_a_meta_slot_is_refused() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    let params = ParamMap::from([(
        "merkl_claim_data.users".to_string(),
        makinax::ext::instructions::ParamValue::Str("0x00".into()),
    )]);
    let e = refusal(build_instruction_with_meta(
        a,
        &params,
        &claim().to_meta_fill(META),
    ));
    assert!(
        e.to_string()
            .contains("filled only from pre-encoded meta words"),
        "{e}"
    );
}

/// A word that is not the slot's declared type in state form is refused:
/// raw bytes, a word with its offset word left in, and trailing junk.
#[tokio::test]
async fn a_word_of_the_wrong_shape_is_refused() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    let good = claim().to_map()["users"].clone();
    let with_offset: Bytes = [B256::with_last_byte(32).as_slice(), &good].concat().into();
    let trailing: Bytes = [good.as_ref(), &[0u8; 32]].concat().into();
    for bad in [
        Bytes::from_static(&[1, 2, 3]),
        Bytes::new(),
        with_offset,
        trailing,
    ] {
        let mut fill = claim().to_meta_fill(META);
        fill.get_mut(META)
            .unwrap()
            .insert("users".into(), bad.clone());
        let e = refusal(build_instruction_with_meta(a, &ParamMap::new(), &fill));
        assert!(
            e.to_string().contains("meta word `merkl_claim_data.users`"),
            "{bad}: {e}"
        );
    }
}

/// A meta slot the leaf commits to cannot be filled: the leaf hash would change.
#[tokio::test]
async fn a_committed_meta_slot_is_refused() {
    let lib = lib().await;
    let mut a = lib.get(LEAF).unwrap().clone();
    a.instruction.bitmap = alloy::primitives::U128::from(1u128 << 127 >> 2);
    let e = refusal(build_instruction_with_meta(
        &a,
        &ParamMap::new(),
        &claim().to_meta_fill(META),
    ));
    assert!(
        e.to_string()
            .contains("`merkl_claim_data.amounts` (state 2) is committed"),
        "{e}"
    );
}

#[tokio::test]
async fn the_distributor_is_read_from_the_leaf() {
    let lib = lib().await;
    let a = lib.get(LEAF).unwrap();
    assert_eq!(distributor_of(a).unwrap(), DISTRIBUTOR);

    let mut no_claim = a.clone();
    no_claim.instruction.commands[0].0[0] ^= 0xff;
    assert!(
        distributor_of(&no_claim)
            .unwrap_err()
            .to_string()
            .contains("no Merkl Distributor claim call")
    );

    let mut two = a.clone();
    let mut other = a.instruction.commands[0];
    other.0[31] ^= 0x01;
    two.instruction.commands.push(other);
    assert!(
        distributor_of(&two)
            .unwrap_err()
            .to_string()
            .contains("more than one Merkl Distributor")
    );
}
