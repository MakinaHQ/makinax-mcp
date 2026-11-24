//! A makinaX module's configuration and operating state, read live from its
//! own view functions (makina-x @ 6168e175), all at one block.
//!
//! Every value comes from a getter the contract has. The module keeps its
//! role, swapper and recipient sets in mappings with point getters only, so
//! those sets cannot be enumerated from views: they are filled by checking
//! the candidates the caller's config names (see each field), and event
//! history is never scanned. Any failed read is an error naming the call; no
//! value is ever defaulted or guessed.

use std::collections::{BTreeMap, BTreeSet};

use alloy::{
    network::Ethereum,
    primitives::{Address, B256, U256},
    providers::Provider,
};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    bridges::BridgeKind,
    chain::module::IMakinaXModule,
    chains,
    config::ModuleConfig,
    error::{Error, Result},
};

/// One configured swapper id and the targets the module holds for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SwapperTargets {
    /// The name the config gives this id.
    pub name: String,
    pub swapper_id: u16,
    /// `getSwapperTargets(id).approvalTarget`; zero when unset.
    pub approval_target: Address,
    /// `getSwapperTargets(id).executionTarget`; zero when unset.
    pub execution_target: Address,
}

/// A module's live state. See the module docs for how sets are filled.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleState {
    pub module: Address,
    /// `safe()`: the Safe the module serves, as the module reports it.
    pub safe: Address,
    /// Whether `safe()` equals the configured `safe`.
    pub safe_matches_config: bool,
    /// The block every read below was taken at.
    pub block: u64,
    /// `provider()`: the MakinaX service account (it alone can suspend the
    /// module and set the swap fee).
    pub service_provider: Address,
    /// `registry()`: the core registry (bridge encoders live there).
    pub registry: Address,
    /// `weirollVm()`: the Weiroll VM instructions run in.
    pub weiroll_vm: Address,
    /// `allowedInstrRoot()`.
    pub allowed_instr_root: B256,
    /// `operatingMode()` as a label (see [`mode_label`]).
    pub operating_mode: String,
    /// `paused()`: paused by a guardian.
    pub paused: bool,
    /// `suspendedByProvider()`: suspended by the service account.
    pub suspended_by_provider: bool,
    /// Operators among the candidates: the configured `operator_address`,
    /// checked with `isOperator`. The contract cannot enumerate operators, so
    /// an operator the config does not name is not listed.
    pub operators: Vec<Address>,
    /// Guardians among the candidates: the Safe (`safe()` and the configured
    /// `safe`, which the contract makes a guardian at initialize) and the
    /// configured `guardian_address`, each checked with `isGuardian`. Others
    /// cannot be enumerated and are not listed.
    pub guardians: Vec<Address>,
    /// Loss caps in basis points: `position_increase`
    /// (`maxPositionIncreaseLossBps`), `position_decrease`
    /// (`maxPositionDecreaseLossBps`), `swap` (`maxSwapLossBps`), and
    /// `bridge:<id>` (`getMaxBridgeLossBps(id)`) for every bridge id this
    /// crate knows (see [`BridgeKind`]). 0 is the module's value, not a gap.
    pub loss_caps_bps: BTreeMap<String, u64>,
    /// Cooldowns in seconds: `instruction` (`instrCooldownDuration`), `swap`
    /// (`swapCooldownDuration`), `bridge` (`bridgeCooldownDuration`).
    pub cooldowns_s: BTreeMap<String, u64>,
    /// `swapFeeRate()`, where 1e18 = 100%, as a decimal string.
    pub swap_fee_rate: String,
    /// `getSwapperTargets(id)` for every swapper id the config names.
    pub swappers: Vec<SwapperTargets>,
    /// Whitelisted bridge recipients among the candidates: the Safe (`safe()`
    /// and the configured `safe`) on every chain in [`chains::KNOWN`] other
    /// than the module's own (the config's chain, or `eth_chainId` when this
    /// crate does not know the config's chain name), checked with `isWhitelistedRecipient`. The
    /// contract cannot enumerate recipients; others are not listed. Each is
    /// `{"chain", "chain_id", "recipient"}`.
    pub recipients: Vec<Value>,
    /// `accountingCurrency()`; `None` when it is zero, which the contract
    /// defines as "use the oracle registry's reference currency".
    pub accounting_currency: Option<Address>,
}

/// Read `cfg.module`'s state at the current block.
///
/// Reads are sequential and pinned to the block read first. Any RPC failure
/// or revert returns [`Error::Rpc`] whose message starts with the call that
/// failed (e.g. `operatingMode(): ...`).
pub async fn read_module_state<P: Provider<Ethereum>>(
    provider: &P,
    cfg: &ModuleConfig,
) -> Result<ModuleState> {
    let block = provider
        .get_block_number()
        .await
        .map_err(|e| Error::rpc(format!("block number: {e}")))?;
    let m = IMakinaXModule::new(cfg.module, provider);
    let at = alloy::eips::BlockId::number(block);

    // `$name` is a literal or a String naming the call; it is only
    // formatted when the read fails.
    macro_rules! view {
        ($name:expr, $call:expr) => {
            $call
                .block(at)
                .call()
                .await
                .map_err(|e| Error::rpc(format!("{}: {e}", $name)))?
        };
    }

    let safe = view!("safe()", m.safe());
    let service_provider = view!(
        "provider()",
        // `provider()` on the instance is its RPC handle, so the contract's
        // own `provider()` getter goes through the generic call builder.
        m.call_builder(&IMakinaXModule::providerCall {})
    );
    let registry = view!("registry()", m.registry());
    let weiroll_vm = view!("weirollVm()", m.weirollVm());
    let allowed_instr_root = view!("allowedInstrRoot()", m.allowedInstrRoot());
    let mode = view!("operatingMode()", m.operatingMode());
    let paused = view!("paused()", m.paused());
    let suspended_by_provider = view!("suspendedByProvider()", m.suspendedByProvider());
    let accounting_currency = view!("accountingCurrency()", m.accountingCurrency());

    let mut loss_caps_bps = BTreeMap::new();
    let incr = view!(
        "maxPositionIncreaseLossBps()",
        m.maxPositionIncreaseLossBps()
    );
    loss_caps_bps.insert(
        "position_increase".to_string(),
        to_u64("maxPositionIncreaseLossBps()", incr)?,
    );
    let decr = view!(
        "maxPositionDecreaseLossBps()",
        m.maxPositionDecreaseLossBps()
    );
    loss_caps_bps.insert(
        "position_decrease".to_string(),
        to_u64("maxPositionDecreaseLossBps()", decr)?,
    );
    let swap = view!("maxSwapLossBps()", m.maxSwapLossBps());
    loss_caps_bps.insert("swap".to_string(), to_u64("maxSwapLossBps()", swap)?);
    for kind in BridgeKind::ALL {
        let id = kind.id();
        let call = format!("getMaxBridgeLossBps({id})");
        let bps = view!(call, m.getMaxBridgeLossBps(id));
        loss_caps_bps.insert(format!("bridge:{id}"), to_u64(&call, bps)?);
    }

    let mut cooldowns_s = BTreeMap::new();
    let instr = view!("instrCooldownDuration()", m.instrCooldownDuration());
    cooldowns_s.insert(
        "instruction".to_string(),
        to_u64("instrCooldownDuration()", instr)?,
    );
    let swap_cd = view!("swapCooldownDuration()", m.swapCooldownDuration());
    cooldowns_s.insert(
        "swap".to_string(),
        to_u64("swapCooldownDuration()", swap_cd)?,
    );
    let bridge_cd = view!("bridgeCooldownDuration()", m.bridgeCooldownDuration());
    cooldowns_s.insert(
        "bridge".to_string(),
        to_u64("bridgeCooldownDuration()", bridge_cd)?,
    );
    let swap_fee_rate = view!("swapFeeRate()", m.swapFeeRate()).to_string();

    let mut operators = Vec::new();
    if let Some(op) = cfg.operator_address
        && view!(format!("isOperator({op})"), m.isOperator(op))
    {
        operators.push(op);
    }

    let safes: BTreeSet<Address> = [safe, cfg.safe].into_iter().collect();
    let mut guardian_candidates = safes.clone();
    guardian_candidates.extend(cfg.guardian_address);
    let mut guardians = Vec::new();
    for g in guardian_candidates {
        if view!(format!("isGuardian({g})"), m.isGuardian(g)) {
            guardians.push(g);
        }
    }

    let mut swappers = Vec::new();
    for (name, id) in &cfg.swappers {
        let t = view!(format!("getSwapperTargets({id})"), m.getSwapperTargets(*id));
        swappers.push(SwapperTargets {
            name: name.clone(),
            swapper_id: *id,
            approval_target: t.approvalTarget,
            execution_target: t.executionTarget,
        });
    }

    // The module's own chain is never a foreign one. A config chain name this
    // crate does not know is resolved by asking the RPC.
    let own_chain = match chains::by_key(&cfg.chain) {
        Some(c) => c.chain_id,
        None => provider
            .get_chain_id()
            .await
            .map_err(|e| Error::rpc(format!("chain id: {e}")))?,
    };
    let mut recipients = Vec::new();
    for chain in chains::KNOWN.iter().filter(|c| c.chain_id != own_chain) {
        for recipient in &safes {
            let listed = view!(
                format!("isWhitelistedRecipient({}, {recipient})", chain.chain_id),
                m.isWhitelistedRecipient(U256::from(chain.chain_id), *recipient)
            );
            if listed {
                recipients.push(json!({
                    "chain": chain.key,
                    "chain_id": chain.chain_id,
                    "recipient": recipient,
                }));
            }
        }
    }

    Ok(ModuleState {
        module: cfg.module,
        safe,
        safe_matches_config: safe == cfg.safe,
        block,
        service_provider,
        registry,
        weiroll_vm,
        allowed_instr_root,
        operating_mode: mode_label(mode),
        paused,
        suspended_by_provider,
        operators,
        guardians,
        loss_caps_bps,
        cooldowns_s,
        swap_fee_rate,
        swappers,
        recipients,
        accounting_currency: (accounting_currency != Address::ZERO).then_some(accounting_currency),
    })
}

/// A bps or seconds value as `u64`, refused (not saturated) when it does not
/// fit: a saturated value would be a number the module never held.
fn to_u64(call: &str, v: U256) -> Result<u64> {
    u64::try_from(v).map_err(|_| Error::Rpc(format!("{call}: value {v} does not fit in 64 bits")))
}

/// Map `operatingMode()` to its label. 0=OPEN, 1=FENCED, 2=WALLED; any other
/// value is `UNKNOWN(n)`, never a guessed mode.
pub fn mode_label(mode: u8) -> String {
    match mode {
        0 => "OPEN".into(),
        1 => "FENCED".into(),
        2 => "WALLED".into(),
        other => format!("UNKNOWN({})", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_its_label_and_an_unknown_one_says_so() {
        assert_eq!(mode_label(0), "OPEN");
        assert_eq!(mode_label(1), "FENCED");
        assert_eq!(mode_label(2), "WALLED");
        assert_eq!(mode_label(7), "UNKNOWN(7)");
    }
}
