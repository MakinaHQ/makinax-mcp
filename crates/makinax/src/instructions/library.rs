//! Loads a rootfile via the `transpiler` crate and exposes its actions as a
//! makinaX-module instruction library, keyed by `protocol.action.token`.
//!
//! Only `local:` rootfile sources are loaded; a `github:` source is refused
//! with an explicit error.

use std::{collections::BTreeMap, path::Path};

use alloy::primitives::{B256, keccak256};
use serde::Serialize;
use transpiler::{
    merkletree::MerkleTree,
    types::{InputSlotType, InstructionType, MakinaInstruction, Rootfile},
};

use crate::{
    config::RootfileSource,
    error::{Error, Result},
};

/// One named action loaded from the rootfile, with its precomputed leaf and proof.
#[derive(Debug, Clone)]
pub struct Action {
    pub qualified_name: String, // e.g. "across-v2.deposit.WBTC"
    pub protocol: String,
    pub action: String,
    pub token: String,
    pub kind: ActionKind,
    pub leaf_hash: B256,
    pub params: Vec<ActionParam>,
    pub proof: Vec<B256>,
    pub instruction: MakinaInstruction, // template; runtime params are spliced into a clone
}

/// Categorical mirror of `transpiler::types::InstructionType`. Same byte values
/// expected by the on-chain enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Management = 0,
    Accounting = 1,
    Harvest = 2,
    FlashloanManagement = 3,
}

impl ActionKind {
    pub fn from_transpiler(t: InstructionType) -> Self {
        match t {
            InstructionType::Management => Self::Management,
            InstructionType::Accounting => Self::Accounting,
            InstructionType::Harvest => Self::Harvest,
            InstructionType::FlashloanManagement => Self::FlashloanManagement,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// One runtime-fillable input slot on an action.
#[derive(Debug, Clone, Serialize)]
pub struct ActionParam {
    pub name: String,
    pub state_index: usize,
    pub kind: String, // human-readable: "uint256", "address", "bytes", "swap", ...
    pub has_meta_type: bool,
}

/// In-memory library of actions keyed by qualified name, with the merkle tree built.
pub struct InstructionLibrary {
    actions: Vec<Action>,
    by_name: BTreeMap<String, usize>,
    root: B256,
    source_label: String,
    accounting_by_position: BTreeMap<String, usize>,
}

impl InstructionLibrary {
    /// Load a rootfile from the given source and build the module's merkle tree.
    pub async fn load(src: &RootfileSource) -> Result<Self> {
        let (label, raw) = match src {
            RootfileSource::Local(p) => {
                let content = read_local_rootfile(p)?;
                (format!("local:{}", p.display()), content)
            }
            RootfileSource::Github(g) => {
                return Err(Error::Rootfile(format!(
                    "github: rootfile sources are not supported (got `{}`); use `local:`",
                    g
                )));
            }
        };

        let rootfile: Rootfile = toml::from_str(&raw).map_err(|e| Error::Config(e.to_string()))?;
        let named = rootfile.all_instructions();

        let mut actions = Vec::with_capacity(named.len());
        let mut leaves = Vec::with_capacity(named.len());
        let mut by_name = BTreeMap::new();
        let mut accounting_by_position = BTreeMap::new();

        for (i, ni) in named.into_iter().enumerate() {
            // The transpiler crate is the canonical source of leaf hashing AND
            // tree construction — the same code that generated the on-chain
            // root. We compute nothing merkle-related ourselves.
            let ih = ni.inner.hash();
            let h = keccak256(ih.as_slice()); // on-chain leaf = keccak(inner)
            leaves.push(ih);

            let kind = ActionKind::from_transpiler(ni.inner.instruction_type);
            let qn = format!(
                "{}.{}.{}",
                ni.protocol_name, ni.instruction_name, ni.token_name
            );

            let params = ni
                .inner
                .inputs_slots
                .iter()
                .map(|s| ActionParam {
                    name: s.name.clone(),
                    state_index: s.index,
                    kind: input_slot_kind_label(&s.r#type),
                    has_meta_type: s.meta_type.is_some(),
                })
                .collect();

            actions.push(Action {
                qualified_name: qn.clone(),
                protocol: ni.protocol_name.clone(),
                action: ni.instruction_name.clone(),
                token: ni.token_name.clone(),
                kind,
                leaf_hash: h,
                params,
                proof: Vec::new(), // filled after tree is built
                instruction: ni.inner.clone(),
            });

            if by_name.insert(qn.clone(), i).is_some() {
                return Err(Error::Rootfile(format!(
                    "duplicate qualified name in rootfile: {}",
                    qn
                )));
            }

            if matches!(kind, ActionKind::Accounting) {
                let key = ni.inner.position_id.to_string();
                accounting_by_position.insert(key, i);
            }
        }

        let tree = MerkleTree::new(leaves.clone());
        let root = *tree.get_root();

        // Backfill proofs now that the tree is built (looked up by inner hash —
        // the tree sorts/dedups internally, so insertion indices don't apply).
        for (a, ih) in actions.iter_mut().zip(&leaves) {
            a.proof = tree.get_proof(ih).expect("element in tree");
        }

        Ok(Self {
            actions,
            by_name,
            root,
            source_label: label,
            accounting_by_position,
        })
    }

    /// The library of a module with no rootfile: valid, and permitting nothing.
    ///
    /// Useful wherever a caller must hold *some* library for a module before
    /// its rootfile exists (for example, while building that rootfile).
    ///
    /// Zero actions means `get()` fails for every name and `iter()` yields
    /// nothing: an empty whitelist permits NOTHING, which is the correct
    /// reading. It is emphatically not "unrestricted".
    pub fn empty(source_label: impl Into<String>) -> Self {
        Self {
            actions: Vec::new(),
            by_name: BTreeMap::new(),
            // The empty merkle root is 0x00…00 — the SAME value a module that
            // has never had setAllowedInstrRoot called reports. Comparing them
            // naively would call a completely unconfigured module "root matches".
            // `is_placeholder()` exists so that comparison can refuse; see
            // `an_empty_library_never_matches_an_unset_on_chain_root`.
            root: B256::ZERO,
            source_label: source_label.into(),
            accounting_by_position: BTreeMap::new(),
        }
    }

    /// Is this the stand-in for a module with no rootfile?
    ///
    /// Kept as a property of the library rather than a lookup elsewhere,
    /// because the dangerous comparison (`root == on-chain root`) often
    /// happens far from anything that knows where the library came from.
    pub fn is_placeholder(&self) -> bool {
        self.actions.is_empty() && self.root == B256::ZERO
    }

    pub fn root(&self) -> B256 {
        self.root
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn count(&self) -> usize {
        self.actions.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.actions.iter()
    }

    pub fn get(&self, qualified_name: &str) -> Result<&Action> {
        let i = *self
            .by_name
            .get(qualified_name)
            .ok_or_else(|| Error::UnknownAction(qualified_name.to_string()))?;
        Ok(&self.actions[i])
    }

    /// Every position id with an accounting instruction in this library
    /// (decimal strings): the positions the module can value.
    pub fn position_ids(&self) -> Vec<String> {
        self.accounting_by_position.keys().cloned().collect()
    }

    /// Find the accounting action paired with a management/harvest action's position id.
    pub fn paired_accounting(&self, position_id_decimal: &str) -> Result<&Action> {
        let i = *self
            .accounting_by_position
            .get(position_id_decimal)
            .ok_or_else(|| {
                Error::Rootfile(format!(
                    "no accounting instruction for position_id {}",
                    position_id_decimal
                ))
            })?;
        Ok(&self.actions[i])
    }
}

fn read_local_rootfile(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).map_err(|e| Error::Rootfile(format!("read {}: {}", p.display(), e)))
}

fn input_slot_kind_label(t: &InputSlotType) -> String {
    // We render whatever Display the transpiler exposes; fallback to debug.
    format!("{:?}", t).to_lowercase()
}
