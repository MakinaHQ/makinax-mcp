//! Call building: resolve a [`CallSpec`] into concrete calldata for a
//! makinaX module ([`BuiltCall`]), plus the identity key a simulation
//! receipt binds to ([`call_key`]).
//!
//! Pure: no RPC, no state. A swap or bridge order must already be concrete
//! (a quote resolved into an order, a recipient chosen) before it reaches
//! [`build_call`]. [`build_swap_from_quote`] and [`build_bridge`] take the
//! fetched data and what was asked for, and refuse a mismatch
//! ([`crate::fetch::validate`]) before encoding.

use alloy::{
    primitives::{Address, B256, Bytes, U256, keccak256},
    sol_types::SolCall,
};
use serde::{Deserialize, Serialize};

use crate::{
    bridges::{
        BridgeKind,
        across::encode_extra_data,
        cctp::{self, CctpFinality, CctpRoute},
    },
    chain::module::IMakinaXModule,
    config::ModuleConfig,
    error::{Error, Result},
    instructions::{
        InstructionLibrary,
        builder::{self, ParamMap},
        library::{Action, ActionKind},
    },
};

/// What to build. `kind` discriminates.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallSpec {
    ManagePosition {
        /// Qualified management action name, e.g. "aavev3.add_collateral.USDC".
        mgmt_action: String,
        #[serde(default)]
        mgmt_params: ParamMap,
        /// Accounting action override. Default: the rootfile's paired accounting
        /// instruction for the position, or (if none exists) the contract's
        /// empty-accounting sentinel — legal outside lockdown/WALLED mode.
        acct_action: Option<String>,
        #[serde(default)]
        acct_params: ParamMap,
    },
    Harvest {
        action: String,
        #[serde(default)]
        params: ParamMap,
        #[serde(default)]
        swap_orders: Vec<SwapOrderArgs>,
    },
    Swap {
        /// An unresolved quote reference a caller may carry; building needs
        /// `order`.
        #[serde(default)]
        quote_id: Option<String>,
        /// The concrete swap order.
        #[serde(default)]
        order: Option<SwapOrderArgs>,
    },
    Raw {
        to: String,
        /// 0x-hex calldata.
        data: String,
        /// Decimal wei; default 0.
        value: Option<String>,
    },
    Bridge {
        order: BridgeOrderArgs,
    },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BridgeOrderArgs {
    /// On-chain bridge id: 1 = Across v4, 2 = LayerZero OFT, 3 = Circle CCTP v2
    /// (see [`BridgeKind`]). Across and CCTP are buildable.
    pub bridge_id: u16,
    /// Destination chain NAME (e.g. "mainnet"), resolved via
    /// [`crate::chain::chain_id_for`].
    pub dest_chain: String,
    /// The destination recipient. Must be set before building.
    #[serde(default)]
    pub recipient: Option<String>,
    pub input_token: String,
    pub output_token: String,
    /// uint256 decimal string, raw input-token units.
    pub input_amount: String,
    /// uint256 decimal string, raw output-token units — the on-chain floor.
    ///
    /// For CCTP this chooses SELF-CLAIM or FORWARDED delivery: equality with
    /// input_amount gives maxFee=0 (a 1:1 self-claim); a lower floor assigns
    /// the difference as the forwarding fee budget.
    pub min_output_amount: String,
    /// Across: seconds past deposit for the fill deadline. Default 3600.
    #[serde(default)]
    pub fill_deadline_offset_s: Option<u32>,
    /// CCTP only: "standard" (DEFAULT) or "fast". Fast needs a nonzero fee
    /// budget and a route that serves it.
    #[serde(default)]
    pub cctp_finality: Option<String>,
}

impl BridgeOrderArgs {
    /// The requested finality tier; unset means Standard.
    pub fn cctp_finality(&self) -> Result<CctpFinality> {
        match self.cctp_finality.as_deref() {
            None | Some("standard") => Ok(CctpFinality::Standard),
            Some("fast") => Ok(CctpFinality::Fast),
            _ => Err(Error::InvalidParams(
                "cctp_finality must be standard or fast".into(),
            )),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SwapOrderArgs {
    pub swapper_id: u16,
    pub input_token: String,
    pub output_token: String,
    /// uint256 as decimal string.
    pub input_amount: String,
    /// uint256 as decimal string.
    pub min_output_amount: String,
    /// Hex-encoded calldata blob the swapper expects (`0x...`).
    pub data: String,
}

/// A resolved call, ready to simulate or execute.
#[derive(Debug, Clone)]
pub struct BuiltCall {
    pub to: Address,
    pub data: Bytes,
    pub value: U256,
    pub label: &'static str,
    pub summary_json: String,
    /// Tokens whose Safe balance the call may move (for fork diffing).
    pub affected_tokens: Vec<Address>,
    pub is_manage_position: bool,
    /// False when the empty-accounting sentinel was used: the contract then
    /// returns (value=0, change=0) STRUCTURALLY — consumers must not treat
    /// that zero as a real position-change measurement.
    pub has_real_accounting: bool,
}

/// THE label for a call spec. `BuiltCall::label` is set from this, so a
/// caller that keys anything on the label keys it on one table.
pub fn label_for(spec: &CallSpec) -> &'static str {
    match spec {
        CallSpec::ManagePosition { .. } => "manage_position",
        CallSpec::Harvest { .. } => "harvest",
        CallSpec::Swap { .. } => "swap",
        CallSpec::Bridge { .. } => "bridge",
        CallSpec::Raw { .. } => "raw",
    }
}

/// A refusal [`build_call`] gives for a bridge order naming CCTP without a
/// route. Exported so a consumer can match it exactly.
pub const CCTP_ROUTE_REQUIRED: &str = "CCTP requires a route (both deployments and the path)";

/// Build `spec` against the module in `cfg`. A `Swap` order (and every
/// `Harvest` swap order) must name a swapper id this module's config maps
/// (`ModuleConfig::swappers`), else [`Error::SwapperNotConfigured`]. A CCTP
/// bridge order needs a route: use [`build_call_with_cctp`].
pub fn build_call(
    cfg: &ModuleConfig,
    lib: &InstructionLibrary,
    spec: &CallSpec,
) -> Result<BuiltCall> {
    build_call_with_cctp(cfg, lib, spec, None)
}

/// [`build_call`], with the [`CctpRoute`] a CCTP bridge order is built
/// against.
pub fn build_call_with_cctp(
    cfg: &ModuleConfig,
    lib: &InstructionLibrary,
    spec: &CallSpec,
    cctp_route: Option<&CctpRoute>,
) -> Result<BuiltCall> {
    check_swappers_configured(cfg, spec)?;
    encode_call(cfg, lib, spec, cctp_route)
}

/// Materialize a fetched quote into the order [`build_call`] encodes, after
/// [`validate_swap_quote`](crate::fetch::validate::validate_swap_quote)
/// checks it against `want`. `min_output_amount` is the quote's `min_out`
/// (the request's slippage), never a floor of this library's own.
#[cfg(feature = "quote")]
pub fn swap_order_from_quote(
    quote: &crate::fetch::quote::Quote,
    swapper_id: u16,
    want: &crate::fetch::validate::SwapExpectation,
) -> Result<SwapOrderArgs> {
    crate::fetch::validate::validate_swap_quote(quote, want)?;
    Ok(SwapOrderArgs {
        swapper_id,
        input_token: format!("{:?}", quote.input_token),
        output_token: format!("{:?}", quote.output_token),
        input_amount: quote.input_amount.to_string(),
        min_output_amount: quote.min_out.to_string(),
        data: format!("0x{}", alloy::primitives::hex::encode(&quote.data)),
    })
}

/// A swap with a quote: `want` must be for this module, the quote must
/// match it ([`swap_order_from_quote`]), and `swapper_id` must be the id the
/// module's config maps to the quote's adapter. Every refusal comes before
/// encoding.
#[cfg(feature = "quote")]
pub fn build_swap_from_quote(
    cfg: &ModuleConfig,
    lib: &InstructionLibrary,
    quote: &crate::fetch::quote::Quote,
    swapper_id: u16,
    want: &crate::fetch::validate::SwapExpectation,
) -> Result<BuiltCall> {
    crate::fetch::validate::check_field("module", cfg.module, want.module)?;
    let order = swap_order_from_quote(quote, swapper_id, want)?;
    match cfg.swappers.get(quote.adapter) {
        Some(id) => crate::fetch::validate::check_field("swapper_id", *id, swapper_id)?,
        None => {
            return Err(crate::fetch::validate::mismatch(
                "swapper_id",
                format!(
                    "the id config maps to adapter `{}` (none is mapped)",
                    quote.adapter
                ),
                swapper_id,
            ));
        }
    }
    build_call(
        cfg,
        lib,
        &CallSpec::Swap {
            quote_id: None,
            order: Some(order),
        },
    )
}

/// A bridge, checked: an unset recipient defaults to the module's Safe,
/// then the order must match `want`
/// ([`validate_bridge_order`](crate::fetch::validate::validate_bridge_order))
/// before it is built with [`build_call_with_cctp`].
pub fn build_bridge(
    cfg: &ModuleConfig,
    lib: &InstructionLibrary,
    order: &BridgeOrderArgs,
    want: &crate::fetch::validate::BridgeExpectation,
    cctp_route: Option<&CctpRoute>,
) -> Result<BuiltCall> {
    let mut order = order.clone();
    if order.recipient.is_none() {
        order.recipient = Some(format!("{:?}", cfg.safe));
    }
    crate::fetch::validate::validate_bridge_order(&order, want)?;
    build_call_with_cctp(cfg, lib, &CallSpec::Bridge { order }, cctp_route)
}

/// What `check_swappers_configured` puts in the fields an order cannot
/// supply (an order names neither an adapter, a router nor a module name).
pub(crate) const NOT_NAMED: &str = "(not named by the order)";

/// Every swap order in `spec` must name a swapper id the module's config
/// maps to an adapter — the config half of a swap. The error carries no
/// adapter name or router (an order names neither), and no guessed id.
pub(crate) fn check_swappers_configured(cfg: &ModuleConfig, spec: &CallSpec) -> Result<()> {
    let orders: Vec<&SwapOrderArgs> = match spec {
        CallSpec::Swap {
            order: Some(order), ..
        } => vec![order],
        CallSpec::Harvest { swap_orders, .. } => swap_orders.iter().collect(),
        _ => Vec::new(),
    };
    for order in orders {
        if !cfg.swappers.values().any(|id| *id == order.swapper_id) {
            let id = order.swapper_id;
            // An order names a swapper id, never an adapter or a module
            // name, and no chain read happens here: the fields that would
            // need those say so rather than invent them.
            return Err(Error::SwapperNotConfigured {
                machine: NOT_NAMED.into(),
                adapter: NOT_NAMED.into(),
                swapper_id: Some(id),
                registration: crate::chain::swappers::REG_NOT_CHECKED,
                router: NOT_NAMED.into(),
                module: format!("{:?}", cfg.module),
                calldata: None,
                remedy: format!(
                    "The order names swapperId {id}, and no adapter in the `swappers` config \
                     of module {:?} maps to it. Configure a swapper id mapping for {id} (the \
                     adapter whose router the order's calldata targets) after confirming with \
                     getSwapperTargets({id}) on the module that {id} holds that router; this \
                     check did not read the chain.",
                    cfg.module
                ),
            });
        }
    }
    Ok(())
}

/// [`build_call_with_cctp`] WITHOUT the swapper-configuration check, for a
/// caller that binds each swap order's swapper id to its router itself
/// (e.g. reads `getSwapperTargets` on-chain against the quote).
pub fn encode_call(
    cfg: &ModuleConfig,
    lib: &InstructionLibrary,
    spec: &CallSpec,
    cctp_route: Option<&CctpRoute>,
) -> Result<BuiltCall> {
    match spec {
        CallSpec::ManagePosition {
            mgmt_action,
            mgmt_params,
            acct_action,
            acct_params,
        } => {
            let mgmt = lib.get(mgmt_action)?;
            ensure_kind(mgmt, ActionKind::Management)?;
            let mgmt_inst = builder::build_instruction(mgmt, mgmt_params)?;
            // Accounting resolution: explicit name > paired-by-position-id >
            // EMPTY accounting instruction (contract semantics: commands.length
            // == 0 means "no accounting" — legal in OPEN/FENCED, reverts
            // AccountingMandatory on-chain in WALLED mode).
            let (acct_inst, acct_label, _) = match acct_action {
                Some(name) => {
                    let acct = lib.get(name)?;
                    ensure_kind(acct, ActionKind::Accounting)?;
                    (
                        builder::build_instruction(acct, acct_params)?,
                        name.clone(),
                        true,
                    )
                }
                None => match lib.paired_accounting(&mgmt.instruction.position_id.to_string()) {
                    Ok(acct) => (
                        builder::build_instruction(acct, acct_params)?,
                        acct.qualified_name.clone(),
                        true,
                    ),
                    Err(_) => (
                        empty_accounting_for(&mgmt_inst),
                        "<none>".to_string(),
                        false,
                    ),
                },
            };
            let has_real_accounting = !acct_inst.commands.is_empty();
            let mut affected = mgmt_inst.affectedTokens.clone();
            affected.extend(acct_inst.affectedTokens.iter().copied());
            affected.sort();
            affected.dedup();
            let data = IMakinaXModule::managePositionCall {
                mgmt: mgmt_inst,
                acct: acct_inst,
            }
            .abi_encode();
            Ok(BuiltCall {
                to: cfg.module,
                data: data.into(),
                value: U256::ZERO,
                label: label_for(spec),
                summary_json: serde_json::to_string(&serde_json::json!({
                    "kind": "manage_position", "mgmt_action": mgmt_action, "mgmt_params": mgmt_params,
                    "acct_action": acct_label, "acct_params": acct_params,
                }))
                .unwrap_or_default(),
                affected_tokens: affected,
                is_manage_position: true,
                has_real_accounting,
            })
        }
        CallSpec::Harvest {
            action,
            params,
            swap_orders,
        } => {
            let a = lib.get(action)?;
            ensure_kind(a, ActionKind::Harvest)?;
            let inst = builder::build_instruction(a, params)?;
            let mut affected = inst.affectedTokens.clone();
            for order in swap_orders {
                affected.push(parse_addr(&order.input_token)?);
                affected.push(parse_addr(&order.output_token)?);
            }
            affected.sort();
            affected.dedup();
            let orders: Vec<IMakinaXModule::SwapOrder> = swap_orders
                .iter()
                .map(swap_order_from)
                .collect::<Result<_>>()?;
            let data = IMakinaXModule::harvestCall {
                instruction: inst,
                swapOrders: orders,
            }
            .abi_encode();
            Ok(BuiltCall {
                to: cfg.module,
                data: data.into(),
                value: U256::ZERO,
                label: label_for(spec),
                summary_json: serde_json::to_string(&serde_json::json!({
                    "kind": "harvest", "action": action, "params": params, "swap_orders": swap_orders,
                }))
                .unwrap_or_default(),
                affected_tokens: affected,
                is_manage_position: false,
                has_real_accounting: false,
            })
        }
        CallSpec::Swap { order, quote_id } => {
            let order = order.as_ref().ok_or_else(|| {
                Error::InvalidParams(format!(
                    "swap has no order (quote_id {:?}): resolve the quote into an order before building",
                    quote_id
                ))
            })?;
            let o = swap_order_from(order)?;
            let affected = vec![o.inputToken, o.outputToken];
            let data = IMakinaXModule::swapCall { order: o }.abi_encode();
            Ok(BuiltCall {
                to: cfg.module,
                data: data.into(),
                value: U256::ZERO,
                label: label_for(spec),
                summary_json: serde_json::to_string(
                    &serde_json::json!({ "kind": "swap", "order": order }),
                )
                .unwrap_or_default(),
                affected_tokens: affected,
                is_manage_position: false,
                has_real_accounting: false,
            })
        }
        CallSpec::Bridge { order } => {
            // The recipient is the caller's decision, made before building.
            let recipient = order.recipient.as_deref().ok_or_else(|| {
                Error::InvalidParams(
                    "bridge order has no recipient: choose one before building".to_string(),
                )
            })?;
            let dest_chain_id = crate::chain::chain_id_for(&order.dest_chain).ok_or_else(|| {
                Error::InvalidParams(format!("unknown dest_chain `{}`", order.dest_chain))
            })?;
            let source_chain_id = crate::chain::chain_id_for(&cfg.chain).ok_or_else(|| {
                Error::InvalidParams(format!("unknown source chain `{}`", cfg.chain))
            })?;
            let input_token = parse_addr(&order.input_token)?;
            let output_token = parse_addr(&order.output_token)?;
            let input_amount = parse_uint(&order.input_amount)?;
            let min_output_amount = parse_uint(&order.min_output_amount)?;

            // `extraData` is PER PROTOCOL, and this is the point at which the
            // bridge id decides which bytes reach an encoder — so the measured
            // id mapping in `bridges::BridgeKind` matters here. Checked here
            // whatever a caller checked before, so no caller can construct
            // calldata for an unsupported bridge.
            let extra_data = match BridgeKind::from_id(order.bridge_id) {
                Some(BridgeKind::AcrossV4) => {
                    encode_extra_data(output_token, order.fill_deadline_offset_s.unwrap_or(3600))
                }
                Some(BridgeKind::CctpV2) => {
                    // Validates USDC-only and derives the path from
                    // `minOutputAmount`; refuses rather than adjusting.
                    let route = cctp_route
                        .ok_or_else(|| Error::InvalidParams(CCTP_ROUTE_REQUIRED.into()))?;
                    if route.from_chain != source_chain_id || route.to_chain != dest_chain_id {
                        return Err(Error::InvalidParams(
                            "CCTP route differs from bridge order chains".into(),
                        ));
                    }
                    let _send = cctp::validate_route(
                        route,
                        input_token,
                        output_token,
                        input_amount,
                        min_output_amount,
                        order.cctp_finality()?,
                    )?;
                    cctp::encode_extra_data(order.cctp_finality()?)
                }
                Some(BridgeKind::LzV2) => {
                    return Err(Error::InvalidParams(format!(
                        "{} is not buildable: LayerZero send support is a follow-up. Use Across (1) or CCTP (3)",
                        BridgeKind::describe_id(order.bridge_id)
                    )));
                }
                None => {
                    return Err(Error::InvalidParams(format!(
                        "{} is not buildable",
                        BridgeKind::describe_id(order.bridge_id)
                    )));
                }
            };
            let bridge_order = IMakinaXModule::BridgeOrder {
                bridgeId: order.bridge_id,
                destinationChainId: U256::from(dest_chain_id),
                recipient: parse_addr(recipient)?,
                inputToken: input_token,
                inputAmount: input_amount,
                minOutputAmount: min_output_amount,
                extraData: extra_data,
            };
            let data = IMakinaXModule::sendOutBridgeTransferCall {
                order: bridge_order,
            }
            .abi_encode();
            let mut summary = serde_json::json!({ "kind": "bridge", "order": order });
            if let Some(route) = cctp_route {
                let mut recorded = route.clone();
                recorded.message_sender = Some(cfg.module);
                summary["cctp_route"] = serde_json::to_value(recorded)
                    .map_err(|e| Error::InvalidParams(e.to_string()))?;
            }
            Ok(BuiltCall {
                to: cfg.module,
                data: data.into(),
                value: U256::ZERO,
                label: label_for(spec),
                summary_json: serde_json::to_string(&summary).unwrap_or_default(),
                affected_tokens: vec![input_token],
                is_manage_position: false,
                has_real_accounting: false,
            })
        }
        CallSpec::Raw { to, data, value } => {
            let to: Address = to
                .parse()
                .map_err(|e| Error::InvalidParams(format!("raw.to: {}", e)))?;
            let data_b = parse_hex(data)?;
            let value = value
                .as_deref()
                .map(|v| v.parse::<U256>())
                .transpose()
                .map_err(|e| Error::InvalidParams(format!("raw.value: {}", e)))?
                .unwrap_or(U256::ZERO);
            Ok(BuiltCall {
                to,
                data: data_b,
                value,
                label: label_for(spec),
                summary_json: serde_json::to_string(
                    &serde_json::json!({ "kind": "raw", "to": to, "data": data }),
                )
                .unwrap_or_default(),
                affected_tokens: vec![],
                is_manage_position: false,
                has_real_accounting: false,
            })
        }
    }
}

/// The contract's "no accounting" sentinel: an ACCOUNTING-typed instruction
/// with empty commands (positionId/isDebt mirror the mgmt instruction).
pub fn empty_accounting_for(mgmt: &IMakinaXModule::Instruction) -> IMakinaXModule::Instruction {
    IMakinaXModule::Instruction {
        positionId: mgmt.positionId,
        isDebt: mgmt.isDebt,
        groupId: mgmt.groupId,
        instructionType: 1, // ACCOUNTING
        affectedTokens: vec![],
        positionTokens: vec![],
        commands: vec![],
        state: vec![],
        stateBitmap: 0,
        merkleProof: vec![],
    }
}

/// The identity a simulation receipt binds to: exact (chain, module name,
/// from, to, value, data).
pub fn call_key(chain: &str, module_name: &str, from: Address, built: &BuiltCall) -> B256 {
    let mut buf = Vec::with_capacity(96 + built.data.len());
    buf.extend_from_slice(chain.as_bytes());
    buf.push(0);
    buf.extend_from_slice(module_name.as_bytes());
    buf.push(0);
    buf.extend_from_slice(from.as_slice());
    buf.extend_from_slice(built.to.as_slice());
    buf.extend_from_slice(&built.value.to_be_bytes::<32>());
    buf.extend_from_slice(&built.data);
    keccak256(buf)
}

fn ensure_kind(action: &Action, expected: ActionKind) -> Result<()> {
    if action.kind != expected {
        return Err(Error::InvalidParams(format!(
            "action `{}` is kind={:?}; expected {:?}",
            action.qualified_name, action.kind, expected
        )));
    }
    Ok(())
}

fn swap_order_from(args: &SwapOrderArgs) -> Result<IMakinaXModule::SwapOrder> {
    Ok(IMakinaXModule::SwapOrder {
        swapperId: args.swapper_id,
        inputToken: parse_addr(&args.input_token)?,
        outputToken: parse_addr(&args.output_token)?,
        inputAmount: parse_uint(&args.input_amount)?,
        minOutputAmount: parse_uint(&args.min_output_amount)?,
        data: parse_hex(&args.data)?,
    })
}

fn parse_addr(s: &str) -> Result<Address> {
    s.parse()
        .map_err(|e| Error::InvalidParams(format!("invalid address `{}`: {}", s, e)))
}

fn parse_uint(s: &str) -> Result<U256> {
    s.parse::<U256>()
        .map_err(|e| Error::InvalidParams(format!("invalid uint256 `{}`: {}", s, e)))
}

pub fn parse_hex(s: &str) -> Result<Bytes> {
    let t = s.trim_start_matches("0x");
    alloy::primitives::hex::decode(t)
        .map(Bytes::from)
        .map_err(|e| Error::InvalidParams(format!("invalid hex `{}`: {}", s, e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ModuleConfig {
        ModuleConfig {
            chain: "base".into(),
            safe: Address::repeat_byte(0x51),
            module: Address::repeat_byte(0x4d),
            rootfile: None,
            operator_address: None,
            guardian_address: None,
            swappers: [("kyber".to_string(), 1u16)].into(),
        }
    }

    fn across_order(bridge_id: u16, recipient: Option<String>) -> BridgeOrderArgs {
        BridgeOrderArgs {
            bridge_id,
            dest_chain: "mainnet".into(),
            recipient,
            input_token: "0x4200000000000000000000000000000000000006".into(),
            output_token: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".into(),
            input_amount: "1000000000000000".into(),
            min_output_amount: "990000000000000".into(),
            fill_deadline_offset_s: None,
            cctp_finality: None,
        }
    }

    const RECIPIENT: &str = "0x2222222222222222222222222222222222222222";

    #[test]
    fn call_key_binds_all_fields() {
        let built = |data: &[u8]| BuiltCall {
            to: Address::repeat_byte(2),
            data: Bytes::from(data.to_vec()),
            value: U256::ZERO,
            label: "raw",
            summary_json: String::new(),
            affected_tokens: vec![],
            is_manage_position: false,
            has_real_accounting: false,
        };
        let k1 = call_key("mainnet", "m", Address::repeat_byte(1), &built(&[1]));
        assert_eq!(
            k1,
            call_key("mainnet", "m", Address::repeat_byte(1), &built(&[1]))
        );
        assert_ne!(
            k1,
            call_key("mainnet", "m", Address::repeat_byte(1), &built(&[2]))
        );
        assert_ne!(
            k1,
            call_key("base", "m", Address::repeat_byte(1), &built(&[1]))
        );
        assert_ne!(
            k1,
            call_key("mainnet", "m2", Address::repeat_byte(1), &built(&[1]))
        );
        assert_ne!(
            k1,
            call_key("mainnet", "m", Address::repeat_byte(9), &built(&[1]))
        );
    }

    /// One label per spec kind, and a built call carries its spec's label:
    /// a consumer that keys anything on the label keys it on this table.
    #[test]
    fn label_for_names_each_kind_and_the_built_call_carries_it() {
        let cfg = cfg();
        let lib = InstructionLibrary::empty("test");
        let raw = CallSpec::Raw {
            to: RECIPIENT.into(),
            data: "0x01".into(),
            value: None,
        };
        let bridge = CallSpec::Bridge {
            order: across_order(1, Some(RECIPIENT.into())),
        };
        let swap = CallSpec::Swap {
            quote_id: None,
            order: None,
        };
        let manage = CallSpec::ManagePosition {
            mgmt_action: "a.b.c".into(),
            mgmt_params: Default::default(),
            acct_action: None,
            acct_params: Default::default(),
        };
        let harvest = CallSpec::Harvest {
            action: "a.b.c".into(),
            params: Default::default(),
            swap_orders: vec![],
        };
        let labels: Vec<&str> = [&raw, &bridge, &swap, &manage, &harvest]
            .into_iter()
            .map(label_for)
            .collect();
        assert_eq!(
            labels,
            ["raw", "bridge", "swap", "manage_position", "harvest"]
        );
        for spec in [&raw, &bridge] {
            let built = build_call(&cfg, &lib, spec).unwrap();
            assert_eq!(built.label, label_for(spec));
        }
    }

    #[test]
    fn bridge_call_encodes_send_out_bridge_transfer() {
        let cfg = cfg();
        let lib = InstructionLibrary::empty("test");
        let built = build_call(
            &cfg,
            &lib,
            &CallSpec::Bridge {
                order: across_order(1, Some(RECIPIENT.into())),
            },
        )
        .unwrap();
        assert_eq!(built.label, "bridge");
        assert_eq!(built.to, cfg.module);
        assert_eq!(
            &built.data[..4],
            IMakinaXModule::sendOutBridgeTransferCall::SELECTOR
        );
        let decoded = IMakinaXModule::sendOutBridgeTransferCall::abi_decode(&built.data).unwrap();
        assert_eq!(decoded.order.bridgeId, 1);
        assert_eq!(decoded.order.destinationChainId, U256::from(1u64));
        assert_eq!(
            decoded.order.recipient,
            RECIPIENT.parse::<Address>().unwrap()
        );
        let extra = encode_extra_data(
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
                .parse()
                .unwrap(),
            3600,
        );
        assert_eq!(decoded.order.extraData, extra);
    }

    #[test]
    fn bridge_call_requires_a_recipient_a_known_bridge_and_a_cctp_route() {
        let cfg = cfg();
        let lib = InstructionLibrary::empty("test");
        let err = build_call(
            &cfg,
            &lib,
            &CallSpec::Bridge {
                order: across_order(1, None),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no recipient"), "{err}");

        // Even with a recipient, an unknown bridge id is refused.
        let err = build_call(
            &cfg,
            &lib,
            &CallSpec::Bridge {
                order: across_order(99, Some(RECIPIENT.into())),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("99"), "{err}");

        // LayerZero is recognised and refused, naming the buildable ones.
        let err = build_call(
            &cfg,
            &lib,
            &CallSpec::Bridge {
                order: across_order(2, Some(RECIPIENT.into())),
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("Across (1) or CCTP (3)"), "{err}");

        // CCTP without a route is the exported refusal.
        let err = build_call(
            &cfg,
            &lib,
            &CallSpec::Bridge {
                order: across_order(3, Some(RECIPIENT.into())),
            },
        )
        .unwrap_err();
        assert!(
            matches!(&err, Error::InvalidParams(m) if m == CCTP_ROUTE_REQUIRED),
            "{err}"
        );
    }

    #[test]
    fn empty_accounting_sentinel_matches_contract_semantics() {
        let mgmt = IMakinaXModule::Instruction {
            positionId: U256::from(1u64),
            isDebt: false,
            groupId: U256::ZERO,
            instructionType: 0,
            affectedTokens: vec![Address::repeat_byte(1)],
            positionTokens: vec![],
            commands: vec![alloy::primitives::B256::repeat_byte(7)],
            state: vec![Bytes::from(vec![1u8])],
            stateBitmap: 0,
            merkleProof: vec![],
        };
        let acct = empty_accounting_for(&mgmt);
        // The contract's "provided" test is commands.length != 0.
        assert!(acct.commands.is_empty());
        assert_eq!(acct.positionId, mgmt.positionId);
        assert_eq!(acct.isDebt, mgmt.isDebt);
        assert_eq!(acct.instructionType, 1);
    }

    /// A swap order must name a configured swapper id; `encode_call` is the
    /// same build without that check.
    #[test]
    fn a_swap_needs_a_configured_swapper_id_unless_the_caller_bound_it() {
        let cfg = cfg();
        let lib = InstructionLibrary::empty("test");
        let order = |id: u16| SwapOrderArgs {
            swapper_id: id,
            input_token: "0x4200000000000000000000000000000000000006".into(),
            output_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
            input_amount: "100".into(),
            min_output_amount: "99".into(),
            data: "0x1234".into(),
        };
        let swap = |id| CallSpec::Swap {
            quote_id: None,
            order: Some(order(id)),
        };
        let built = build_call(&cfg, &lib, &swap(1)).expect("id 1 is mapped");
        assert_eq!(built.label, "swap");
        assert_eq!(built.affected_tokens.len(), 2);
        match build_call(&cfg, &lib, &swap(7)).unwrap_err() {
            e @ Error::SwapperNotConfigured { .. } => {
                assert_eq!(
                    e.to_string(),
                    "swap adapter `(not named by the order)` has no swapperId mapping on machine \
                     `(not named by the order)` — a swap needs BOTH a config mapping and an \
                     on-chain registration, and the CONFIG half is missing (registering on-chain \
                     again cannot fix that half). The order names swapperId 7, and no adapter in \
                     the `swappers` config of module 0x4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d \
                     maps to it. Configure a swapper id mapping for 7 (the adapter whose router \
                     the order's calldata targets) after confirming with getSwapperTargets(7) on \
                     the module that 7 holds that router; this check did not read the chain."
                );
                let Error::SwapperNotConfigured {
                    swapper_id,
                    calldata,
                    registration,
                    ..
                } = e
                else {
                    unreachable!()
                };
                assert_eq!(swapper_id, Some(7));
                assert_eq!(registration, crate::chain::swappers::REG_NOT_CHECKED);
                assert!(calldata.is_none(), "no transaction is ever offered here");
            }
            other => panic!("expected SwapperNotConfigured, got {other:?}"),
        }
        let harvest = CallSpec::Harvest {
            action: "any".into(),
            params: Default::default(),
            swap_orders: vec![order(7)],
        };
        assert!(matches!(
            build_call(&cfg, &lib, &harvest),
            Err(Error::SwapperNotConfigured { .. })
        ));
        let bound = encode_call(&cfg, &lib, &swap(7), None).expect("caller-bound id");
        assert_eq!(bound.data, built_with(7));

        fn built_with(id: u16) -> Bytes {
            let mut c = ModuleConfig {
                chain: "base".into(),
                safe: Address::repeat_byte(0x51),
                module: Address::repeat_byte(0x4d),
                rootfile: None,
                operator_address: None,
                guardian_address: None,
                swappers: Default::default(),
            };
            c.swappers.insert("x".into(), id);
            build_call(
                &c,
                &InstructionLibrary::empty("test"),
                &CallSpec::Swap {
                    quote_id: None,
                    order: Some(SwapOrderArgs {
                        swapper_id: id,
                        input_token: "0x4200000000000000000000000000000000000006".into(),
                        output_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                        input_amount: "100".into(),
                        min_output_amount: "99".into(),
                        data: "0x1234".into(),
                    }),
                },
            )
            .unwrap()
            .data
        }
    }

    #[test]
    fn a_swap_with_no_order_is_refused() {
        let err = build_call(
            &cfg(),
            &InstructionLibrary::empty("test"),
            &CallSpec::Swap {
                quote_id: Some("q_1".into()),
                order: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no order"), "{err}");
    }
}
