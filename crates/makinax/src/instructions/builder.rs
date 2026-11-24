//! Resolve a named action + runtime params into the on-chain `Instruction` struct.
//!
//! Mirrors spellcaster's `select_inputs` substitution loop but accepts params from a JSON map
//! instead of interactive prompts. Handles the simple `Uint` / `Int` / `Address` / `Bool` /
//! `Bytes` slot types; a slot with a meta-type (Swap, MerklClaim, FluidClaim, KingClaim,
//! FxSave) is refused with [`Error::MetaTypeSlot`].
//!
//! [`build_instruction_with_meta`] also fills meta-type slots, from state words
//! the caller has already encoded (e.g. `fetch::merkl::ClaimData::to_map`).
//! Each word is written into `state[slot.index]` unchanged, as spellcaster's
//! claim filler does. Only slots the leaf leaves uncommitted (bitmap bit
//! clear) can be filled, so the leaf hash and its proof are unchanged.

use std::collections::BTreeMap;

use alloy::{
    dyn_abi::DynSolType,
    primitives::{Address, B256, Bytes},
};
use serde::{Deserialize, Serialize};
use transpiler::types::InputSlotType;

use crate::{
    chain::module::IMakinaXModule,
    error::{Error, Result},
    instructions::library::Action,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ParamValue {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl ParamValue {
    fn as_str(&self) -> String {
        match self {
            ParamValue::Str(s) => s.clone(),
            ParamValue::Num(n) => n.to_string(),
            ParamValue::Bool(b) => b.to_string(),
        }
    }
}

pub type ParamMap = BTreeMap<String, ParamValue>;

/// Pre-encoded state words for meta-type slots, keyed by the slot's meta
/// name, then by its field (`meta_type.name` → `meta_type.field` → word).
///
/// A word is the slot's value in weiroll state form: for a dynamic type
/// (array, bytes) the ABI encoding without its leading offset word, i.e.
/// length then data; for a static type the plain ABI encoding.
pub type MetaFill = BTreeMap<String, BTreeMap<String, Bytes>>;

/// Resolve a single action with runtime params into the on-chain Instruction struct.
///
/// Refuses any action with a meta-type slot ([`Error::MetaTypeSlot`]); see
/// [`build_instruction_with_meta`].
pub fn build_instruction(
    action: &Action,
    params: &ParamMap,
) -> Result<IMakinaXModule::Instruction> {
    build(action, params, None)
}

/// [`build_instruction`], also filling every meta-type slot from `meta`.
///
/// Refused, so a call is never sent with a placeholder or a guessed value:
/// - a meta-type slot with no word in `meta` ([`Error::MetaTypeSlot`]);
/// - a word in `meta` that names no meta-type slot of the action;
/// - a param in `params` that names a meta-type slot;
/// - a word that is not a canonical encoding of the slot's declared type;
/// - a meta-type slot the leaf commits to (its bitmap bit is set): writing it
///   would change the leaf hash, so the proof would no longer verify.
///
/// The words are not otherwise checked: what they claim is the caller's
/// concern, and what the call does is decided on chain.
pub fn build_instruction_with_meta(
    action: &Action,
    params: &ParamMap,
    meta: &MetaFill,
) -> Result<IMakinaXModule::Instruction> {
    build(action, params, Some(meta))
}

fn build(
    action: &Action,
    params: &ParamMap,
    meta: Option<&MetaFill>,
) -> Result<IMakinaXModule::Instruction> {
    for name in params.keys() {
        if !action
            .instruction
            .inputs_slots
            .iter()
            .any(|slot| &slot.name == name)
        {
            return Err(Error::UnknownParam {
                action: action.qualified_name.clone(),
                param: name.clone(),
            });
        }
    }
    if let Some(meta) = meta {
        check_meta_names(action, meta)?;
    }
    let mut state: Vec<Bytes> = action.instruction.state.to_vec();

    for slot in &action.instruction.inputs_slots {
        if let Some(mt) = &slot.meta_type {
            let word = meta
                .and_then(|m| m.get(&mt.name))
                .and_then(|fields| fields.get(&mt.field))
                .ok_or_else(|| Error::MetaTypeSlot {
                    action: action.qualified_name.clone(),
                    slot: slot.name.clone(),
                })?;
            if params.contains_key(&slot.name) {
                return Err(Error::InvalidParams(format!(
                    "action `{}`: slot `{}` is a meta-type slot and is filled only from pre-encoded meta words, not from params",
                    action.qualified_name, slot.name
                )));
            }
            if slot_is_committed(action.instruction.bitmap.to(), slot.index) {
                return Err(Error::InvalidParams(format!(
                    "action `{}`: meta-type slot `{}` (state {}) is committed by the leaf; filling it would change the leaf hash",
                    action.qualified_name, slot.name, slot.index
                )));
            }
            check_state_word(&slot.r#type, word).map_err(|e| {
                Error::InvalidParams(format!(
                    "action `{}`: meta word `{}.{}` for slot `{}`: {e}",
                    action.qualified_name, mt.name, mt.field, slot.name
                ))
            })?;
            if slot.index >= state.len() {
                return Err(Error::InvalidParams(format!(
                    "slot {} out of range for state[{}]",
                    slot.index,
                    state.len()
                )));
            }
            state[slot.index] = word.clone();
            continue;
        }

        let raw = params.get(&slot.name).ok_or_else(|| {
            Error::InvalidParams(format!(
                "action `{}` requires param `{}` (slot {}, type {:?})",
                action.qualified_name, slot.name, slot.index, slot.r#type
            ))
        })?;

        let encoded = encode_slot_value(&slot.r#type, &raw.as_str()).map_err(|e| {
            Error::InvalidParams(format!(
                "param `{}` for action `{}`: {}",
                slot.name, action.qualified_name, e
            ))
        })?;

        if slot.index >= state.len() {
            return Err(Error::InvalidParams(format!(
                "slot {} out of range for state[{}]",
                slot.index,
                state.len()
            )));
        }
        state[slot.index] = encoded;
    }

    Ok(IMakinaXModule::Instruction {
        positionId: action.instruction.position_id,
        isDebt: action.instruction.is_debt,
        groupId: action.instruction.group_id,
        instructionType: action.kind.as_u8(),
        affectedTokens: action.instruction.affected_tokens.clone(),
        positionTokens: action.instruction.position_tokens.clone(),
        commands: action.instruction.commands.clone(),
        state,
        stateBitmap: action.instruction.bitmap.to(),
        merkleProof: action.proof.clone(),
    })
}

/// Every word in `meta` must name a meta-type slot of `action`: a word for a
/// slot the action does not have would otherwise be silently dropped.
fn check_meta_names(action: &Action, meta: &MetaFill) -> Result<()> {
    for (name, fields) in meta {
        for field in fields.keys() {
            let known = action.instruction.inputs_slots.iter().any(|slot| {
                slot.meta_type
                    .as_ref()
                    .is_some_and(|mt| &mt.name == name && &mt.field == field)
            });
            if !known {
                return Err(Error::InvalidParams(format!(
                    "action `{}` has no meta-type slot `{name}.{field}`",
                    action.qualified_name
                )));
            }
        }
    }
    Ok(())
}

/// Whether the leaf commits to state word `index`: bit `127 - index` of the
/// bitmap, the same bit the transpiler's leaf hash reads.
fn slot_is_committed(bitmap: u128, index: usize) -> bool {
    index < 128 && bitmap & ((1u128 << 127) >> index) != 0
}

/// A state word is a canonical encoding of the slot's declared type: it
/// decodes as that type and re-encodes to the same bytes.
fn check_state_word(kind: &InputSlotType, word: &Bytes) -> std::result::Result<(), String> {
    let ty = DynSolType::from(kind.clone());
    let mut full = Vec::with_capacity(word.len() + 32);
    if ty.is_dynamic() {
        // Restore the offset word the state form leaves out.
        full.extend_from_slice(&B256::with_last_byte(32).0);
    }
    full.extend_from_slice(word);
    let value = ty
        .abi_decode(&full)
        .map_err(|e| format!("does not decode as {}: {e}", ty.sol_type_name()))?;
    if value.abi_encode() != full {
        return Err(format!(
            "is not a canonical {} encoding",
            ty.sol_type_name()
        ));
    }
    Ok(())
}

fn encode_slot_value(kind: &InputSlotType, raw: &str) -> Result<Bytes> {
    let dyn_type: DynSolType = match kind {
        InputSlotType::Uint => DynSolType::Uint(256),
        InputSlotType::Int => DynSolType::Int(256),
        InputSlotType::Address => DynSolType::Address,
        InputSlotType::Bool => DynSolType::Bool,
        InputSlotType::Bytes => DynSolType::Bytes,
        // Meta-typed and swap/specialty slots fall through here in spellcaster's flow;
        // they are refused rather than guessed at.
        other => {
            return Err(Error::InvalidParams(format!(
                "slot kind {:?} is not yet supported; expected uint/int/address/bool/bytes",
                other
            )));
        }
    };

    let parsed = dyn_type
        .coerce_str(raw)
        .map_err(|e| Error::InvalidParams(format!("could not parse `{}`: {}", raw, e)))?;
    let encoded = parsed.abi_encode();
    Ok(Bytes::from(encoded))
}

/// A serializable view of a built instruction: the action, its leaf and the
/// raw `Instruction` fields, with every wide integer rendered as a string.
#[derive(Debug, Clone, Serialize)]
pub struct PreviewedInstruction {
    pub action: String,
    pub kind: super::library::ActionKind,
    pub leaf_hash: B256,
    pub proof_len: usize,
    pub instruction: PreviewedRawInstruction,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewedRawInstruction {
    pub position_id: String,
    pub is_debt: bool,
    pub group_id: String,
    pub instruction_type: u8,
    pub affected_tokens: Vec<Address>,
    pub position_tokens: Vec<Address>,
    pub commands: Vec<B256>,
    pub state: Vec<String>, // hex-encoded
    /// DECIMAL STRING, not a number — and that is load-bearing.
    ///
    /// `serde_json::Value` has no u128 arm, so `to_value` on anything above
    /// `u64::MAX` fails with "number out of range" — and every weiroll bitmap
    /// is above it, because the flags live in the HIGH bits (real roots carry
    /// 127605887595351923798765477786913079296 and
    /// 159507359494189904748456847233641349120). A numeric field would make
    /// this view unserializable for every real action.
    ///
    /// `position_id` and `group_id` above are strings for the same reason
    /// (U256 does not fit a JSON number either). A 39-digit integer would also
    /// silently lose precision in any JavaScript consumer, which is the second
    /// reason not to emit it as a number.
    #[serde(serialize_with = "bitmap_as_string")]
    pub state_bitmap: u128,
    pub merkle_proof: Vec<B256>,
}

fn bitmap_as_string<S: serde::Serializer>(v: &u128, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_str(&v.to_string())
}

impl PreviewedInstruction {
    pub fn from(action: &Action, instruction: &IMakinaXModule::Instruction) -> Self {
        Self {
            action: action.qualified_name.clone(),
            kind: action.kind,
            leaf_hash: action.leaf_hash,
            proof_len: instruction.merkleProof.len(),
            instruction: PreviewedRawInstruction {
                position_id: instruction.positionId.to_string(),
                is_debt: instruction.isDebt,
                group_id: instruction.groupId.to_string(),
                instruction_type: instruction.instructionType,
                affected_tokens: instruction.affectedTokens.clone(),
                position_tokens: instruction.positionTokens.clone(),
                commands: instruction.commands.clone(),
                state: instruction
                    .state
                    .iter()
                    .map(|b| format!("0x{}", alloy::hex::encode(b)))
                    .collect(),
                state_bitmap: instruction.stateBitmap,
                merkle_proof: instruction.merkleProof.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::RootfileSource, instructions::InstructionLibrary};

    async fn lib() -> InstructionLibrary {
        InstructionLibrary::load(&RootfileSource::Local(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rootfile.toml"),
        ))
        .await
        .expect("fixture loads")
    }

    fn params_for(action: &Action) -> ParamMap {
        action
            .instruction
            .inputs_slots
            .iter()
            .map(|s| (s.name.clone(), ParamValue::Str("1000".into())))
            .collect()
    }

    /// Every action's view serializes to a JSON object, with the (above
    /// u64::MAX) bitmap as an exact decimal string.
    #[tokio::test]
    async fn every_action_previews_to_json_with_a_string_bitmap() {
        let lib = lib().await;
        for action in lib.iter() {
            let inst = build_instruction(action, &params_for(action)).unwrap();
            assert_eq!(inst.merkleProof, action.proof);
            let v = serde_json::to_value(PreviewedInstruction::from(action, &inst))
                .unwrap_or_else(|e| panic!("{}: {e}", action.qualified_name));
            assert_eq!(v["action"], action.qualified_name.as_str());
            assert_eq!(
                v["instruction"]["state_bitmap"].as_str(),
                Some(inst.stateBitmap.to_string().as_str())
            );
        }
    }

    #[tokio::test]
    async fn an_unknown_param_is_refused_naming_it() {
        let lib = lib().await;
        let action = lib.iter().next().unwrap();
        let mut params = params_for(action);
        params.insert("bogus".into(), ParamValue::Num(1.0));
        let e = build_instruction(action, &params)
            .err()
            .expect("refused")
            .to_string();
        assert!(e.contains("no runtime param `bogus`"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_param_is_refused_naming_it() {
        let lib = lib().await;
        let action = lib
            .iter()
            .find(|a| !a.instruction.inputs_slots.is_empty())
            .expect("fixture has an action with a runtime slot");
        let e = build_instruction(action, &ParamMap::new())
            .err()
            .expect("refused")
            .to_string();
        assert!(e.contains("requires param `amount_to_deposit`"), "{e}");
    }
}
