//! The Tx-Builder batch shape: the standard Safe Transaction Builder JSON
//! format, built from an ordered list of owner calls.
//!
//! Architectural guard: `TxBuilderBatch` can only be CONSTRUCTED here (all
//! fields private; callers use [`build_batch`]/[`single_call`]) — a single,
//! auditable place that emits this shape, so nothing downstream can
//! assemble a look-alike batch that skips whatever a consumer layers on
//! top (fork-simulation before delivery, post-state assertions, and so
//! on). A consumer that wants that guarantee enforced repo-wide can pin it
//! with its own source-scan test over the source tree IT owns; this crate
//! only owns the constructor.

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

use crate::{Result, owner::dyn_call::encode_call};

/// One owner call: target + solidity signature + string args (encoded via
/// the dyn_call signature encoder) + the human summary line naming the
/// parameter sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerCall {
    pub to: Address,
    pub sig: String,
    pub args: Vec<String>,
    pub summary: String,
}

/// The standard Safe Tx-Builder import format (camelCase). Fields are
/// PRIVATE by design — only this module constructs it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxBuilderBatch {
    version: String,
    chain_id: String,
    created_at: u64,
    meta: BatchMeta,
    transactions: Vec<TxEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchMeta {
    name: String,
    description: String,
    tx_builder_version: String,
    created_from_safe_address: String,
}

/// A single entry — fields readable (consumers inspect/execute), but a batch
/// cannot be assembled outside this module (the containing Vec is private).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxEntry {
    pub to: String,
    pub value: String,
    pub data: String,
}

impl TxBuilderBatch {
    pub fn transactions(&self) -> &[TxEntry] {
        &self.transactions
    }
    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }
    pub fn name(&self) -> &str {
        &self.meta.name
    }
    pub fn is_empty(&self) -> bool {
        self.transactions.is_empty()
    }
    pub fn len(&self) -> usize {
        self.transactions.len()
    }

    /// TEST-ONLY seam for adversarial fixtures (mis-ordered batches etc.).
    #[doc(hidden)]
    pub fn reorder_for_test(&mut self, from: usize, to: usize) {
        let tx = self.transactions.remove(from);
        self.transactions.insert(to, tx);
    }
}

/// Build a Tx-Builder batch from an ordered call list. The ONLY way to
/// construct one (single calls are just a one-element list).
pub fn build_batch(
    name: &str,
    description: &str,
    chain_id: u64,
    safe: Address,
    now_ms: u64,
    calls: &[OwnerCall],
) -> Result<(TxBuilderBatch, Vec<String>)> {
    let mut batch = TxBuilderBatch {
        version: "1.0".into(),
        chain_id: chain_id.to_string(),
        created_at: now_ms,
        meta: BatchMeta {
            name: name.into(),
            description: description.into(),
            tx_builder_version: "1.16.5".into(),
            created_from_safe_address: format!("{safe:?}"),
        },
        transactions: Vec::with_capacity(calls.len()),
    };
    let mut summary = Vec::with_capacity(calls.len());
    for c in calls {
        let data = encode_call(&c.sig, &c.args)?;
        batch.transactions.push(TxEntry {
            to: format!("{:?}", c.to),
            value: "0".into(),
            data: format!("0x{}", alloy::hex::encode(&data)),
        });
        summary.push(call_summary(batch.transactions.len(), c));
    }
    Ok((batch, summary))
}

/// An owner call that may send native value, with its calldata already
/// encoded, for a call that must carry native value.
#[derive(Debug, Clone)]
pub struct ValueCall {
    pub to: Address,
    pub value: alloy::primitives::U256,
    pub data: Vec<u8>,
    pub summary: String,
}

/// Build a Tx-Builder batch whose entries may carry native value.
pub fn build_value_batch(
    name: &str,
    description: &str,
    chain_id: u64,
    safe: Address,
    now_ms: u64,
    calls: &[ValueCall],
) -> Result<(TxBuilderBatch, Vec<String>)> {
    if calls.is_empty() {
        return Err(crate::Error::InvalidParams(
            "an owner batch needs at least one call".into(),
        ));
    }
    let mut batch = TxBuilderBatch {
        version: "1.0".into(),
        chain_id: chain_id.to_string(),
        created_at: now_ms,
        meta: BatchMeta {
            name: name.into(),
            description: description.into(),
            tx_builder_version: "1.16.5".into(),
            created_from_safe_address: format!("{safe:?}"),
        },
        transactions: Vec::with_capacity(calls.len()),
    };
    let mut summary = Vec::with_capacity(calls.len());
    for (i, c) in calls.iter().enumerate() {
        batch.transactions.push(TxEntry {
            to: format!("{:?}", c.to),
            value: c.value.to_string(),
            data: format!("0x{}", alloy::hex::encode(&c.data)),
        });
        summary.push(format!(
            "[{}] to={:?} value={}: {}",
            i + 1,
            c.to,
            c.value,
            c.summary
        ));
    }
    Ok((batch, summary))
}

/// Public (not `pub(crate)`, unlike its pre-move visibility): a consuming
/// product calls this across the crate boundary now, e.g. to compare a
/// rendered summary line against what a batch would produce.
pub fn call_summary(one_based_index: usize, call: &OwnerCall) -> String {
    format!("[{}] to={:?}: {}", one_based_index, call.to, call.summary)
}

/// Single owner-call artifact for a caller's own fix rendering (a readiness
/// report naming one remediation, say): `{to, data}` for the Tx Builder —
/// same encoder, same module, same rule.
pub fn single_call(to: Address, sig: &str, args: &[String]) -> Result<serde_json::Value> {
    let data = encode_call(sig, args)?;
    Ok(serde_json::json!({
        "to": format!("{to:?}"),
        "data": format!("0x{}", alloy::hex::encode(&data)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_builder_encodes_and_summarizes() {
        let safe: Address = "0x5aFE00000000000000000000000000000000A001"
            .parse()
            .unwrap();
        let (batch, summary) = build_batch(
            "t",
            "d",
            8453,
            safe,
            1_000,
            &[OwnerCall {
                to: safe,
                sig: "enableModule(address)".into(),
                args: vec![format!("{safe:?}")],
                summary: "enable it — source: test".into(),
            }],
        )
        .unwrap();
        assert_eq!(batch.len(), 1);
        assert!(batch.transactions()[0].data.starts_with("0x610b5925")); // enableModule selector
        assert_eq!(summary.len(), 1);
        assert!(summary[0].contains("source: test"));
        let v = serde_json::to_value(&batch).unwrap();
        assert_eq!(v["meta"]["txBuilderVersion"], "1.16.5");
        assert_eq!(v["chainId"], "8453");
    }
}
