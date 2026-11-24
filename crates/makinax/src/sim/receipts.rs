//! Simulation receipts: a link a consumer can require between simulating a
//! call and sending it. A receipt binds the EXACT tx params (a caller-computed
//! call key) to the block it was simulated at; `verify_and_consume` refuses
//! without a fresh matching one. Single-use.
//!
//! This module only issues and checks receipts. Whether a send REQUIRES one is
//! the consumer's decision, made at its own send path.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use alloy::primitives::{B256, keccak256};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimReceipt {
    pub id: String,
    pub issued_block: u64,
    pub expires_block: u64,
}

#[derive(Debug)]
pub struct SimReceiptStore {
    ttl_blocks: u64,
    by_id: Mutex<HashMap<String, (B256, u64)>>, // id -> (call key, issued_block)
    /// Where receipts live so they OUTLIVE THIS PROCESS.
    ///
    /// An in-memory map alone means a receipt issued by a simulation in one
    /// process is unknown to a send in another: one process could simulate
    /// and never spend it. A real report put it exactly -- "receipts are
    /// in-process; a separate CLI process cannot spend them" -- and no amount
    /// of locking fixes that, because the receipt is not there to be found.
    ///
    /// It cannot be made stateless instead: the id is
    /// `keccak(call_key ‖ issued_block)`, so anyone able to recompute it
    /// could mint a receipt for a simulation that never ran, which is the one
    /// thing this type exists to prevent. The record has to be kept; it just
    /// has to be kept somewhere both processes can see.
    ///
    /// `None` keeps the in-memory behaviour (a single process).
    dir: Option<PathBuf>,
}

impl SimReceiptStore {
    pub fn new(ttl_blocks: u64) -> Self {
        Self {
            ttl_blocks,
            by_id: Mutex::new(HashMap::new()),
            dir: None,
        }
    }

    /// A store whose receipts are readable by every process sharing `dir`:
    /// one file per receipt, directly inside `dir` (created 0700, files
    /// 0600). The caller chooses the directory; this crate keeps no state
    /// location of its own.
    pub fn in_dir(ttl_blocks: u64, dir: &Path) -> Self {
        Self {
            ttl_blocks,
            by_id: Mutex::new(HashMap::new()),
            dir: Some(dir.to_path_buf()),
        }
    }

    /// One file per receipt. A file per id rather than one index because
    /// single-use then falls out of `remove_file`: exactly one caller can
    /// remove a given path, so two processes racing to spend one receipt
    /// cannot both win, with no lock and no read-modify-write.
    fn path(&self, id: &str) -> Option<PathBuf> {
        // The id is this type's own `sim_<64 hex>`; anything else never
        // reaches a path.
        let rest = id.strip_prefix("sim_")?;
        (rest.len() == 64 && rest.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| self.dir.as_ref().map(|d| d.join(id)))
            .flatten()
    }

    fn read_record(&self, id: &str) -> Option<(B256, u64)> {
        if let Some(found) = self.by_id.lock().unwrap().get(id).copied() {
            return Some(found);
        }
        let text = std::fs::read_to_string(self.path(id)?).ok()?;
        let (key, issued) = text.trim().split_once(' ')?;
        Some((key.parse().ok()?, issued.parse().ok()?))
    }

    pub fn issue(&self, key: B256, block: u64) -> SimReceipt {
        let mut buf = [0u8; 40];
        buf[..32].copy_from_slice(key.as_slice());
        buf[32..].copy_from_slice(&block.to_be_bytes());
        let id = format!("sim_{:x}", keccak256(buf));
        self.by_id.lock().unwrap().insert(id.clone(), (key, block));
        if let Some(path) = self.path(&id) {
            // Best effort: a receipt directory that cannot be written still
            // issues a receipt this process can spend, which is exactly the
            // behaviour before receipts were durable. It must never be a
            // reason to refuse a simulation that succeeded.
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = crate::util::perms::write_private(&path, format!("{key:x} {block}").as_bytes());
        }
        SimReceipt {
            id,
            issued_block: block,
            expires_block: block + self.ttl_blocks,
        }
    }

    /// Non-consuming fail-fast, run BEFORE any fork work: a
    /// missing or mismatched receipt refuses typed without paying fork
    /// spin-up, and infra failure can never mask the refusal. Block staleness
    /// is deliberately NOT checked here — that check is only authoritative
    /// against the execution target's own head (a dev fork's head lags the
    /// read chain), so it stays in `verify_and_consume`.
    pub fn peek(&self, id: &str, key: B256) -> Result<()> {
        let (stored_key, _) = self.read_record(id).ok_or(Error::SimulationRequired)?;
        if stored_key != key {
            return Err(Error::SimulationRequired);
        }
        Ok(())
    }

    /// Typed refusals: missing id or key mismatch → SimulationRequired;
    /// block expiry → SimulationStale. Consumed on success (single-use).
    pub fn verify_and_consume(&self, id: &str, key: B256, current_block: u64) -> Result<()> {
        let (stored_key, issued) = self.read_record(id).ok_or(Error::SimulationRequired)?;
        if stored_key != key {
            return Err(Error::SimulationRequired);
        }
        let consume = |store: &Self| {
            store.by_id.lock().unwrap().remove(id);
            // The REMOVAL is what makes a receipt single-use across processes:
            // exactly one caller can unlink a path. A durable store that
            // cannot remove has not consumed anything, so it refuses.
            match store.path(id) {
                Some(path) => match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    // Already spent, by this process or another.
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(Error::SimulationRequired)
                    }
                    Err(_) => Err(Error::SimulationRequired),
                },
                None => Ok(()),
            }
        };
        if current_block > issued + self.ttl_blocks {
            let _ = consume(self);
            return Err(Error::SimulationStale {
                issued,
                current: current_block,
                ttl: self.ttl_blocks,
            });
        }
        consume(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_verify_consume() {
        let s = SimReceiptStore::new(20);
        let key = B256::repeat_byte(1);
        let r = s.issue(key, 100);
        assert!(r.id.starts_with("sim_"));
        assert_eq!(r.expires_block, 120);
        s.verify_and_consume(&r.id, key, 110).unwrap();
        // single-use
        assert!(matches!(
            s.verify_and_consume(&r.id, key, 110),
            Err(Error::SimulationRequired)
        ));
    }

    #[test]
    fn stale_receipt_rejected() {
        let s = SimReceiptStore::new(20);
        let key = B256::repeat_byte(1);
        let r = s.issue(key, 100);
        match s.verify_and_consume(&r.id, key, 121) {
            Err(Error::SimulationStale {
                issued: 100,
                current: 121,
                ttl: 20,
            }) => {}
            other => panic!("expected SimulationStale, got {:?}", other),
        }
    }

    #[test]
    fn key_mismatch_is_simulation_required() {
        let s = SimReceiptStore::new(20);
        let r = s.issue(B256::repeat_byte(1), 100);
        assert!(matches!(
            s.verify_and_consume(&r.id, B256::repeat_byte(2), 105),
            Err(Error::SimulationRequired)
        ));
        assert!(matches!(
            s.verify_and_consume("sim_nonexistent", B256::repeat_byte(1), 105),
            Err(Error::SimulationRequired)
        ));
    }
}

#[cfg(test)]
mod durable_tests {
    use super::*;

    /// The constraint a real report named -- "receipts are in-process;
    /// a separate CLI process cannot spend them".
    ///
    /// Two stores over ONE directory stand in for two processes: the second
    /// knows nothing of the first's memory, which is the whole point.
    #[test]
    fn a_receipt_one_process_issues_is_spendable_by_another_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let key = B256::repeat_byte(7);

        let simulator = SimReceiptStore::in_dir(20, dir.path());
        let receipt = simulator.issue(key, 100);

        // A different process: same directory, empty memory.
        let executor = SimReceiptStore::in_dir(20, dir.path());
        executor
            .peek(&receipt.id, key)
            .expect("the other process must be able to find it");
        executor
            .verify_and_consume(&receipt.id, key, 110)
            .expect("and spend it");

        // SINGLE USE holds across processes, in both directions.
        assert!(matches!(
            executor.verify_and_consume(&receipt.id, key, 110),
            Err(Error::SimulationRequired)
        ));
        assert!(
            matches!(
                simulator.verify_and_consume(&receipt.id, key, 110),
                Err(Error::SimulationRequired)
            ),
            "the issuer must not be able to spend what another process already spent"
        );
    }

    #[test]
    fn a_durable_receipt_still_refuses_a_different_call_and_a_stale_block() {
        let dir = tempfile::tempdir().unwrap();
        let key = B256::repeat_byte(1);
        let other = SimReceiptStore::in_dir(20, dir.path());
        let r = other.issue(key, 100);

        let reader = SimReceiptStore::in_dir(20, dir.path());
        assert!(matches!(
            reader.verify_and_consume(&r.id, B256::repeat_byte(2), 110),
            Err(Error::SimulationRequired)
        ));
        assert!(
            matches!(
                reader.verify_and_consume(&r.id, key, 200),
                Err(Error::SimulationStale { .. })
            ),
            "expiry is judged the same way whoever reads it"
        );
    }

    /// An id is this type's own shape or it never becomes a path. Durability
    /// must not turn a caller-supplied string into a filesystem read.
    #[test]
    fn a_supplied_id_can_never_escape_the_receipt_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = SimReceiptStore::in_dir(20, dir.path());
        for hostile in [
            "sim_../../../etc/passwd",
            "../../etc/passwd",
            "sim_",
            "sim_zz",
            &format!("sim_{}", "g".repeat(64)),
            "",
        ] {
            assert!(store.path(hostile).is_none(), "{hostile} became a path");
            assert!(matches!(
                store.peek(hostile, B256::repeat_byte(1)),
                Err(Error::SimulationRequired)
            ));
        }
    }
}
