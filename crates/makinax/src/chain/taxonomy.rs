//! Revert-decode taxonomy: every custom error in the makinaX module ABIs mapped
//! to a machine-readable code, a one-sentence explanation, and remediation.
//! Unknown selectors are reported with selector + raw data, flagged
//! `known: false` — never swallowed. Single error path for simulate + execute.

use alloy::{
    primitives::hex,
    sol,
    sol_types::{SolError, SolInterface},
};
use serde::{Deserialize, Serialize};

sol! {
    interface IMakinaXErrors {
        error AccountingMandatory();
        error AlreadyGuardian();
        error AlreadyOperator();
        error AmountOutTooLow();
        error BridgeEncoderDoesNotExist();
        error CctpDomainNotRegistered();
        error ExceededMaxFee(uint256 fee, uint256 max);
        error InstructionsMismatch();
        error InvalidAccounting();
        error InvalidBpsValue();
        error InvalidBridgeTransferRoute();
        error InvalidDecimals();
        error InvalidFeedRoute();
        error InvalidFeeRate();
        error InvalidInputToken();
        error InvalidInstructionProof();
        error InvalidInstructionType();
        error InvalidLzSentAmount();
        error OftMismatch();
        error InvalidPositionChangeDirection();
        error LzEndpointIdNotRegistered();
        error MaxValueLossExceeded();
        error MismatchedLengths();
        error NegativeTokenPrice(address priceFeed);
        error NotGuardian();
        error NotOperator();
        error OftAlreadyRegistered();
        error OftNotRegistered();
        error Paused();
        error ProtectedCctpDomain();
        error ProtectedChainId();
        error ProtectedGuardian();
        error PriceFeedRouteNotRegistered(address token);
        error PriceFeedStale(address priceFeed, uint256 updatedAt);
        error RecipientAlreadyWhitelisted();
        error RecipientNotWhitelisted();
        error RouteAlreadyRegistered();
        error RouteNotRegistered();
        error Suspended();
        error SwapFailed();
        error SwapperTargetsNotSet();
        error SweepNativeFailed();
        error TransferFromSafeFailed();
        error UnauthorizedCaller();
        error ZeroAddress();
        error ZeroChainId();
        error ZeroRefundAddress();
    }
}

sol! {
    /// Canonical Weiroll VM error (upstream weiroll, used by the module's VM):
    /// surfaces the failing command, its target, and the inner revert reason.
    interface IWeirollVMErrors {
        error ExecutionFailed(uint256 command_index, address target, string message);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecodedError {
    /// Machine-readable snake_case code, e.g. "max_value_loss_exceeded".
    pub code: String,
    /// 4-byte selector as 0x-hex.
    pub selector: String,
    pub explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// Full revert data as 0x-hex (always included).
    pub raw: String,
    pub known: bool,
    /// Structured fields for errors that carry them (e.g. weiroll
    /// ExecutionFailed: command_index/target/message) plus advisory
    /// diagnosis probes appended by the simulator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl DecodedError {
    /// For non-revert failures that still flow through the single error path
    /// (RPC transport problems, backend-specific failures).
    pub fn infrastructure(code: &str, explanation: String) -> Self {
        Self {
            code: code.to_string(),
            selector: "0x".into(),
            explanation,
            remediation: None,
            raw: "0x".into(),
            known: false,
            details: None,
        }
    }
}

/// Decode an alloy contract-call error into the taxonomy. Single error path
/// for read AND write calls: module custom errors, Error(string), Panic —
/// anything else is reported as infrastructure, never swallowed.
pub fn decode_contract_error(e: &alloy::contract::Error) -> DecodedError {
    if let alloy::contract::Error::TransportError(te) = e
        && let Some(bytes) = te.as_error_resp().and_then(|p| p.as_revert_data())
    {
        return decode_revert(&bytes);
    }
    DecodedError::infrastructure("rpc_error", crate::util::redact::text(&e.to_string()))
}

/// (explanation, remediation) for a known error code.
fn meta(code: &str) -> (&'static str, Option<&'static str>) {
    match code {
        "accounting_mandatory" => (
            "The current operating mode requires a paired ACCOUNTING instruction with executable commands for this managePosition call.",
            Some(
                // Neutral wording; a consumer may substitute its own.
                "Choose the matching accounting action. If it compiles to zero commands, naming it is insufficient: obtain a corrected instruction set and activate its new root. Keep WALLED mode enabled.",
            ),
        ),
        "already_guardian" => ("The address is already a guardian.", None),
        "already_operator" => ("The address is already an operator.", None),
        "amount_out_too_low" => (
            "The swap produced less than minOutputAmount.",
            Some("Refresh the quote and retry with a realistic min_output_amount, or reduce size."),
        ),
        "bridge_encoder_does_not_exist" => (
            "No bridge encoder is registered for this bridgeId.",
            Some("Check the bridgeId against the registry's configured encoders."),
        ),
        "cctp_domain_not_registered" => {
            ("The destination chain has no registered CCTP domain.", None)
        }
        "exceeded_max_fee" => (
            "The bridge/swap fee exceeds the configured maximum.",
            Some("Retry later or raise the fee cap deliberately."),
        ),
        "instructions_mismatch" => (
            "The management and accounting instructions do not refer to the same position (positionId/isDebt mismatch).",
            Some(
                "Use the accounting instruction whose position_id matches the management instruction.",
            ),
        ),
        "invalid_accounting" => (
            "The accounting instruction's output is malformed (does not match the affectedTokens layout).",
            Some("The rootfile's accounting instruction is broken — contact the curator."),
        ),
        "invalid_bps_value" => ("A BPS parameter is out of range.", None),
        "invalid_bridge_transfer_route" => {
            ("The bridge route (chain/token) is not registered.", None)
        }
        "invalid_decimals" => ("A token reports unsupported decimals.", None),
        "invalid_feed_route" => ("The oracle feed route configuration is invalid.", None),
        "invalid_fee_rate" => ("The fee rate is out of range.", None),
        "invalid_input_token" => ("The input token is not valid for this operation.", None),
        "invalid_instruction_proof" => (
            "The instruction's merkle proof does not verify against the on-chain allowedInstrRoot — the instruction is not whitelisted in the current root.",
            Some(
                "Reload the rootfile (the root may have rotated) and rebuild the instruction; check health_check root_matches.",
            ),
        ),
        "invalid_instruction_type" => (
            "The instruction's type is wrong for this entrypoint (e.g. ACCOUNTING passed as the management instruction).",
            Some("Check instruction kinds via list_actions."),
        ),
        "invalid_lz_sent_amount" => ("LayerZero sent amount does not match the order.", None),
        "oft_mismatch" => ("The LayerZero OFT does not match the token.", None),
        "invalid_position_change_direction" => (
            "The position value moved in a direction inconsistent with the token flow (e.g. outflow but position decreased) — blocked by the lockdown validation matrix.",
            Some(
                "Re-check the instruction pairing and current position state; simulate again at the latest block.",
            ),
        ),
        "lz_endpoint_id_not_registered" => (
            "The destination chain has no registered LayerZero endpoint.",
            None,
        ),
        "max_value_loss_exceeded" => (
            "The value-loss check failed: the position delta vs tokens moved exceeds the configured loss bps.",
            Some(
                "Reduce the size, check oracle freshness, or verify the accounting instruction reflects the position.",
            ),
        ),
        "mismatched_lengths" => ("Two arrays that must be equal length are not.", None),
        "negative_token_price" => (
            "A price feed returned a negative price.",
            Some("The feed is broken — do not trade this token until resolved."),
        ),
        // Raised only by `removeGuardian` / `removeOperator` when the address
        // being removed does not hold the role (makina-x @ 6168e175,
        // MakinaXGovernable). A caller without a role gets `UnauthorizedCaller`.
        "not_guardian" => (
            "The address is not a guardian on this module, so it cannot be removed.",
            Some("Check the address: only a current guardian can be removed."),
        ),
        "not_operator" => (
            "The address is not an operator on this module, so it cannot be removed.",
            Some("Check the address: only a current operator can be removed."),
        ),
        "oft_already_registered" => ("The OFT is already registered.", None),
        "oft_not_registered" => (
            "The OFT is not registered for lockdown-mode transfer.",
            None,
        ),
        "paused" => (
            "The module is paused by a guardian — all operator actions are blocked on-chain.",
            Some("Wait for the guardian to unpause; do not retry until then."),
        ),
        "protected_cctp_domain" => (
            "This CCTP domain mapping is protected and cannot be changed.",
            None,
        ),
        "protected_chain_id" => ("This chain id is protected and cannot be changed.", None),
        "protected_guardian" => ("This guardian is protected and cannot be removed.", None),
        "price_feed_route_not_registered" => (
            "No oracle feed route is registered for the token.",
            Some("The Safe must register a feed route before this token can be valued."),
        ),
        "price_feed_stale" => (
            "The oracle feed has not updated within its staleness threshold.",
            Some(
                "Wait for the feed to update; if persistent, the feed or its threshold needs attention.",
            ),
        ),
        "recipient_already_whitelisted" => ("The bridge recipient is already whitelisted.", None),
        "recipient_not_whitelisted" => (
            "The bridge recipient is not whitelisted for the destination chain.",
            Some("The Safe must whitelist the recipient first."),
        ),
        "route_already_registered" => ("The bridge route is already registered.", None),
        "route_not_registered" => ("The bridge route is not registered.", None),
        "suspended" => (
            "The module is suspended by the provider — all operator actions are blocked on-chain.",
            Some("Contact the provider; do not retry until unsuspended."),
        ),
        "swap_failed" => (
            "The swapper call reverted.",
            Some("Refresh the swap calldata (quotes expire) and retry."),
        ),
        "swapper_targets_not_set" => (
            "No approval/execution targets are configured for this swapperId.",
            None,
        ),
        "sweep_native_failed" => ("Native token sweep to the Safe failed.", None),
        "transfer_from_safe_failed" => (
            "Pulling tokens from the Safe failed (allowance/balance).",
            Some("Check the Safe's token balance."),
        ),
        // One error for all four role gates: onlySafe, onlyProvider,
        // onlyOperator and onlyGuardian (makina-x @ 6168e175).
        "unauthorized_caller" => (
            "The caller is not allowed to call this function; it may require the Safe, the provider, an operator or a guardian.",
            Some(
                "Send the call from an address that holds the role the function requires. Operator-gated calls, including the accounting reads accountForPosition[Batch], must come from a registered operator: check isOperator(<address>) on the module.",
            ),
        ),
        "zero_address" => (
            "A zero address was passed where a real address is required.",
            None,
        ),
        "zero_chain_id" => ("A zero chain id was passed.", None),
        "zero_refund_address" => ("A zero refund address was passed.", None),
        _ => ("", None),
    }
}

fn pascal_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode revert data. Order: known makinaX module custom errors → standard
/// Error(string) → Panic(uint256) → unknown (selector + raw, never swallowed).
pub fn decode_revert(data: &[u8]) -> DecodedError {
    let raw = format!("0x{}", hex::encode(data));
    let selector = if data.len() >= 4 {
        format!("0x{}", hex::encode(&data[..4]))
    } else {
        "0x".to_string()
    };

    // Weiroll VM ExecutionFailed: the single most diagnostic error the module
    // can produce — decode into which command, which target, and the inner
    // reason (empty reasons labeled as such, never dropped).
    if let Ok(IWeirollVMErrors::IWeirollVMErrorsErrors::ExecutionFailed(e)) =
        IWeirollVMErrors::IWeirollVMErrorsErrors::abi_decode(data)
    {
        // The VM writes "Unknown" itself when the target's revert carried no
        // string reason (a bare custom error, or nothing at all), so that word
        // is never the target's own and is not quoted as one. A target that
        // really reverts with the string "Unknown" reads as giving no reason
        // too; the two cannot be told apart from the bytes.
        let reason_given = !e.message.is_empty() && e.message != VM_NO_REASON_PLACEHOLDER;
        let inner = if e.message.is_empty() {
            "(empty revert reason — the target reverted without a message)".to_string()
        } else if !reason_given {
            REFUSED_WITHOUT_A_REASON.to_string()
        } else {
            e.message.clone()
        };
        return DecodedError {
            code: "weiroll_execution_failed".into(),
            selector,
            explanation: format!(
                "Weiroll command #{} reverted calling {:?}: {}",
                e.command_index, e.target, inner
            ),
            remediation: Some(
                "The failure is inside the instruction's command list — check the target contract's \
                 state gates (vault caps, pauses, allowances). The simulator appends 4626 probe \
                 results in `details` when the target is a vault."
                    .into(),
            ),
            raw,
            known: true,
            details: Some(serde_json::json!({
                "command_index": e.command_index.to_string(),
                "target": format!("{:?}", e.target),
                "inner_message": inner,
                "reason_given": reason_given,
            })),
        };
    }

    if let Ok(err) = IMakinaXErrors::IMakinaXErrorsErrors::abi_decode(data) {
        let code = pascal_to_snake(err_name(&err));
        let (explanation, remediation) = meta(&code);
        return DecodedError {
            code,
            selector,
            explanation: explanation.to_string(),
            remediation: remediation.map(String::from),
            raw,
            known: true,
            details: None,
        };
    }

    if let Ok(r) = alloy::sol_types::Revert::abi_decode(data) {
        return DecodedError {
            code: "revert_string".into(),
            selector,
            explanation: format!("Revert: {}", r.reason),
            remediation: None,
            raw,
            known: true,
            details: None,
        };
    }
    if let Ok(p) = alloy::sol_types::Panic::abi_decode(data) {
        let code_u64 = u64::try_from(p.code).unwrap_or(u64::MAX);
        return DecodedError {
            code: "panic".into(),
            selector,
            explanation: format!(
                "Solidity panic 0x{:x} ({})",
                code_u64,
                panic_label(code_u64)
            ),
            remediation: None,
            raw,
            known: true,
            details: None,
        };
    }

    if data.is_empty() {
        return DecodedError {
            code: "unknown".into(),
            selector,
            explanation: format!("The revert carried no data: {REFUSED_WITHOUT_A_REASON}."),
            remediation: None,
            raw,
            known: false,
            details: None,
        };
    }

    DecodedError {
        code: "unknown".into(),
        selector,
        explanation: "Unrecognized revert selector — not in the known makinaX module ABI set."
            .into(),
        remediation: Some(
            "Check whether the deployed module is newer than the local ABI; report the selector."
                .into(),
        ),
        raw,
        known: false,
        details: None,
    }
}

/// The text the Weiroll VM puts in `ExecutionFailed.message` when the
/// target's revert carried no string reason.
const VM_NO_REASON_PLACEHOLDER: &str = "Unknown";

/// What a revert with no reason the decoder can read says instead.
pub const REFUSED_WITHOUT_A_REASON: &str = "the contract refused without giving a reason";

/// The `code` of a revert matched to a caller-declared error.
pub const DECLARED_ERROR: &str = "declared_error";

/// An error a contract can revert with, declared by its Solidity signature
/// in a caller's configuration, with optional plain words for what it means.
///
/// A revert is named only from a declaration: this crate keeps no list of
/// other contracts' errors, and a selector that matches nothing declared
/// stays unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredError {
    /// `AmountTooLow()`, `BelowMin(uint256 min)` or `error BelowMin(uint256)`.
    pub signature: String,
    /// What the error means, in the declarer's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The contracts (addresses) that raise this error. Empty: any contract
    /// on the chain (the error is not tied to one). See
    /// [`decode_failing_frame`] for how a scope is matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contracts: Vec<String>,
}

impl DeclaredError {
    /// Whether this declaration covers a revert from `address` (any address
    /// when it names no contract). Case-insensitive.
    pub fn covers(&self, address: &str) -> bool {
        self.contracts.is_empty()
            || self
                .contracts
                .iter()
                .any(|c| c.trim().eq_ignore_ascii_case(address.trim()))
    }

    fn parsed(&self) -> Option<alloy::json_abi::Error> {
        let sig = self.signature.trim();
        let sig = sig.strip_prefix("error ").unwrap_or(sig).trim();
        alloy::json_abi::Error::parse(sig).ok()
    }

    /// The error's name, or `None` when the signature does not parse.
    pub fn name(&self) -> Option<String> {
        self.parsed().map(|e| e.name)
    }

    /// The 4-byte selector, or `None` when the signature does not parse.
    pub fn selector(&self) -> Option<[u8; 4]> {
        self.parsed().map(|e| e.selector().0)
    }
}

/// Decode revert data against caller-declared errors first, then as
/// [`decode_revert`] does.
///
/// A declared error matches when the selector is its own and the data
/// decodes as its arguments. The match has `code` [`DECLARED_ERROR`],
/// `known: true`, an explanation of `Name: description` (or `Name`), and
/// `details` with `name`, `signature`, `description` and the decoded `args`
/// as strings. Pass the errors declared for the contract that produced the
/// data; a declaration that does not parse is skipped.
pub fn decode_revert_declared(data: &[u8], declared: &[DeclaredError]) -> DecodedError {
    use alloy::dyn_abi::{DynSolValue, ErrorExt};

    if data.len() >= 4 {
        for d in declared {
            let Some(err) = d.parsed() else { continue };
            if err.selector().0 != data[..4] {
                continue;
            }
            let Ok(decoded) = err.decode_error(data) else {
                continue;
            };
            let args: Vec<String> = decoded
                .body
                .iter()
                .map(|v| match v {
                    DynSolValue::Uint(u, _) => u.to_string(),
                    DynSolValue::Int(i, _) => i.to_string(),
                    DynSolValue::Address(a) => format!("{a:?}"),
                    DynSolValue::Bool(b) => b.to_string(),
                    DynSolValue::String(s) => s.clone(),
                    other => format!("{other:?}"),
                })
                .collect();
            let description = d
                .description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let explanation = match description {
                Some(words) => format!("{}: {words}", err.name),
                None => err.name.clone(),
            };
            let mut details = serde_json::json!({
                "name": err.name,
                "signature": d.signature,
                "args": args,
            });
            if let Some(words) = description {
                details["description"] = serde_json::json!(words);
            }
            return DecodedError {
                code: DECLARED_ERROR.into(),
                selector: format!("0x{}", hex::encode(&data[..4])),
                explanation,
                remediation: None,
                raw: format!("0x{}", hex::encode(data)),
                known: true,
                details: Some(details),
            };
        }
    }
    decode_revert(data)
}

/// Decode the failing call a simulator attached to an error
/// (`details.failing_frame.output`, the revert bytes of the call the failure
/// started in) against caller-declared errors. `None` when no failing frame
/// was attached, for example because the backend could not trace the call.
///
/// Scope: a declaration that names `contracts` is used only when the failing
/// frame ran at one of them (its `to`, or its `context` — the proxy a
/// DELEGATECALL ran in). Scoped declarations are tried first; a match from
/// one carries `details.scope = "contract"`. A match from a declaration
/// with no `contracts` carries `details.scope = "chain"`: the error is named,
/// but nothing says which contract raised it.
pub fn decode_failing_frame(
    err: &DecodedError,
    declared: &[DeclaredError],
) -> Option<DecodedError> {
    let frame = err.details.as_ref()?.get("failing_frame")?;
    let output = frame.get("output")?.as_str()?;
    let bytes = hex::decode(output.trim_start_matches("0x")).ok()?;
    let at = |k: &str| frame.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let (to, context) = (at("to"), at("context"));
    let scoped: Vec<DeclaredError> = declared
        .iter()
        .filter(|d| !d.contracts.is_empty())
        .filter(|d| (!to.is_empty() && d.covers(to)) || (!context.is_empty() && d.covers(context)))
        .cloned()
        .collect();
    let unscoped: Vec<DeclaredError> = declared
        .iter()
        .filter(|d| d.contracts.is_empty())
        .cloned()
        .collect();
    for (list, scope) in [(scoped, "contract"), (unscoped, "chain")] {
        let mut d = decode_revert_declared(&bytes, &list);
        if d.code == DECLARED_ERROR {
            if let Some(serde_json::Value::Object(det)) = d.details.as_mut() {
                det.insert("scope".into(), serde_json::json!(scope));
            }
            return Some(d);
        }
    }
    Some(decode_revert(&bytes))
}

fn panic_label(code: u64) -> &'static str {
    match code {
        0x01 => "assert failed",
        0x11 => "arithmetic overflow/underflow",
        0x12 => "division by zero",
        0x21 => "invalid enum value",
        0x31 => "pop on empty array",
        0x32 => "array index out of bounds",
        0x41 => "out of memory",
        0x51 => "uninitialized function pointer",
        _ => "unknown panic code",
    }
}

fn err_name(e: &IMakinaXErrors::IMakinaXErrorsErrors) -> &'static str {
    use IMakinaXErrors::IMakinaXErrorsErrors as E;
    match e {
        E::AccountingMandatory(_) => "AccountingMandatory",
        E::AlreadyGuardian(_) => "AlreadyGuardian",
        E::AlreadyOperator(_) => "AlreadyOperator",
        E::AmountOutTooLow(_) => "AmountOutTooLow",
        E::BridgeEncoderDoesNotExist(_) => "BridgeEncoderDoesNotExist",
        E::CctpDomainNotRegistered(_) => "CctpDomainNotRegistered",
        E::ExceededMaxFee(_) => "ExceededMaxFee",
        E::InstructionsMismatch(_) => "InstructionsMismatch",
        E::InvalidAccounting(_) => "InvalidAccounting",
        E::InvalidBpsValue(_) => "InvalidBpsValue",
        E::InvalidBridgeTransferRoute(_) => "InvalidBridgeTransferRoute",
        E::InvalidDecimals(_) => "InvalidDecimals",
        E::InvalidFeedRoute(_) => "InvalidFeedRoute",
        E::InvalidFeeRate(_) => "InvalidFeeRate",
        E::InvalidInputToken(_) => "InvalidInputToken",
        E::InvalidInstructionProof(_) => "InvalidInstructionProof",
        E::InvalidInstructionType(_) => "InvalidInstructionType",
        E::InvalidLzSentAmount(_) => "InvalidLzSentAmount",
        E::OftMismatch(_) => "OftMismatch",
        E::InvalidPositionChangeDirection(_) => "InvalidPositionChangeDirection",
        E::LzEndpointIdNotRegistered(_) => "LzEndpointIdNotRegistered",
        E::MaxValueLossExceeded(_) => "MaxValueLossExceeded",
        E::MismatchedLengths(_) => "MismatchedLengths",
        E::NegativeTokenPrice(_) => "NegativeTokenPrice",
        E::NotGuardian(_) => "NotGuardian",
        E::NotOperator(_) => "NotOperator",
        E::OftAlreadyRegistered(_) => "OftAlreadyRegistered",
        E::OftNotRegistered(_) => "OftNotRegistered",
        E::Paused(_) => "Paused",
        E::ProtectedCctpDomain(_) => "ProtectedCctpDomain",
        E::ProtectedChainId(_) => "ProtectedChainId",
        E::ProtectedGuardian(_) => "ProtectedGuardian",
        E::PriceFeedRouteNotRegistered(_) => "PriceFeedRouteNotRegistered",
        E::PriceFeedStale(_) => "PriceFeedStale",
        E::RecipientAlreadyWhitelisted(_) => "RecipientAlreadyWhitelisted",
        E::RecipientNotWhitelisted(_) => "RecipientNotWhitelisted",
        E::RouteAlreadyRegistered(_) => "RouteAlreadyRegistered",
        E::RouteNotRegistered(_) => "RouteNotRegistered",
        E::Suspended(_) => "Suspended",
        E::SwapFailed(_) => "SwapFailed",
        E::SwapperTargetsNotSet(_) => "SwapperTargetsNotSet",
        E::SweepNativeFailed(_) => "SweepNativeFailed",
        E::TransferFromSafeFailed(_) => "TransferFromSafeFailed",
        E::UnauthorizedCaller(_) => "UnauthorizedCaller",
        E::ZeroAddress(_) => "ZeroAddress",
        E::ZeroChainId(_) => "ZeroChainId",
        E::ZeroRefundAddress(_) => "ZeroRefundAddress",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{Address, U256};
    use alloy::sol_types::SolError;

    /// Acceptance: every custom error in the ABI decodes to a known code with
    /// a non-empty explanation.
    #[test]
    fn every_abi_error_is_covered() {
        use IMakinaXErrors as E;
        let addr = Address::repeat_byte(0x11);
        let samples: Vec<(&str, Vec<u8>)> = vec![
            (
                "accounting_mandatory",
                E::AccountingMandatory {}.abi_encode(),
            ),
            ("already_guardian", E::AlreadyGuardian {}.abi_encode()),
            ("already_operator", E::AlreadyOperator {}.abi_encode()),
            ("amount_out_too_low", E::AmountOutTooLow {}.abi_encode()),
            (
                "bridge_encoder_does_not_exist",
                E::BridgeEncoderDoesNotExist {}.abi_encode(),
            ),
            (
                "cctp_domain_not_registered",
                E::CctpDomainNotRegistered {}.abi_encode(),
            ),
            (
                "exceeded_max_fee",
                E::ExceededMaxFee {
                    fee: U256::from(2u64),
                    max: U256::from(1u64),
                }
                .abi_encode(),
            ),
            (
                "instructions_mismatch",
                E::InstructionsMismatch {}.abi_encode(),
            ),
            ("invalid_accounting", E::InvalidAccounting {}.abi_encode()),
            ("invalid_bps_value", E::InvalidBpsValue {}.abi_encode()),
            (
                "invalid_bridge_transfer_route",
                E::InvalidBridgeTransferRoute {}.abi_encode(),
            ),
            ("invalid_decimals", E::InvalidDecimals {}.abi_encode()),
            ("invalid_feed_route", E::InvalidFeedRoute {}.abi_encode()),
            ("invalid_fee_rate", E::InvalidFeeRate {}.abi_encode()),
            ("invalid_input_token", E::InvalidInputToken {}.abi_encode()),
            (
                "invalid_instruction_proof",
                E::InvalidInstructionProof {}.abi_encode(),
            ),
            (
                "invalid_instruction_type",
                E::InvalidInstructionType {}.abi_encode(),
            ),
            (
                "invalid_lz_sent_amount",
                E::InvalidLzSentAmount {}.abi_encode(),
            ),
            ("oft_mismatch", E::OftMismatch {}.abi_encode()),
            (
                "invalid_position_change_direction",
                E::InvalidPositionChangeDirection {}.abi_encode(),
            ),
            (
                "lz_endpoint_id_not_registered",
                E::LzEndpointIdNotRegistered {}.abi_encode(),
            ),
            (
                "max_value_loss_exceeded",
                E::MaxValueLossExceeded {}.abi_encode(),
            ),
            ("mismatched_lengths", E::MismatchedLengths {}.abi_encode()),
            (
                "negative_token_price",
                E::NegativeTokenPrice { priceFeed: addr }.abi_encode(),
            ),
            ("not_guardian", E::NotGuardian {}.abi_encode()),
            ("not_operator", E::NotOperator {}.abi_encode()),
            (
                "oft_already_registered",
                E::OftAlreadyRegistered {}.abi_encode(),
            ),
            ("oft_not_registered", E::OftNotRegistered {}.abi_encode()),
            ("paused", E::Paused {}.abi_encode()),
            (
                "protected_cctp_domain",
                E::ProtectedCctpDomain {}.abi_encode(),
            ),
            ("protected_chain_id", E::ProtectedChainId {}.abi_encode()),
            ("protected_guardian", E::ProtectedGuardian {}.abi_encode()),
            (
                "price_feed_route_not_registered",
                E::PriceFeedRouteNotRegistered { token: addr }.abi_encode(),
            ),
            (
                "price_feed_stale",
                E::PriceFeedStale {
                    priceFeed: addr,
                    updatedAt: U256::from(0u64),
                }
                .abi_encode(),
            ),
            (
                "recipient_already_whitelisted",
                E::RecipientAlreadyWhitelisted {}.abi_encode(),
            ),
            (
                "recipient_not_whitelisted",
                E::RecipientNotWhitelisted {}.abi_encode(),
            ),
            (
                "route_already_registered",
                E::RouteAlreadyRegistered {}.abi_encode(),
            ),
            (
                "route_not_registered",
                E::RouteNotRegistered {}.abi_encode(),
            ),
            ("suspended", E::Suspended {}.abi_encode()),
            ("swap_failed", E::SwapFailed {}.abi_encode()),
            (
                "swapper_targets_not_set",
                E::SwapperTargetsNotSet {}.abi_encode(),
            ),
            ("sweep_native_failed", E::SweepNativeFailed {}.abi_encode()),
            (
                "transfer_from_safe_failed",
                E::TransferFromSafeFailed {}.abi_encode(),
            ),
            ("unauthorized_caller", E::UnauthorizedCaller {}.abi_encode()),
            ("zero_address", E::ZeroAddress {}.abi_encode()),
            ("zero_chain_id", E::ZeroChainId {}.abi_encode()),
            ("zero_refund_address", E::ZeroRefundAddress {}.abi_encode()),
        ];
        assert_eq!(
            samples.len(),
            47,
            "keep this list in sync with the sol! block"
        );
        for (expected_code, data) in samples {
            let d = decode_revert(&data);
            assert!(d.known, "{} should be known", expected_code);
            assert_eq!(d.code, expected_code);
            assert!(
                !d.explanation.is_empty(),
                "{} needs an explanation",
                expected_code
            );
            assert_eq!(d.selector.len(), 10, "{}", expected_code);
        }
    }

    /// The three role errors say what the contract means (makina-x @
    /// 6168e175, MakinaXGovernable): NotOperator/NotGuardian come only from
    /// removing an address that does not hold the role; UnauthorizedCaller is
    /// every role gate's refusal.
    #[test]
    fn role_errors_explain_what_the_contract_raises_them_for() {
        let d = decode_revert(&IMakinaXErrors::NotOperator {}.abi_encode());
        assert_eq!(
            d.explanation,
            "The address is not an operator on this module, so it cannot be removed."
        );
        assert_eq!(
            d.remediation.as_deref(),
            Some("Check the address: only a current operator can be removed.")
        );
        let d = decode_revert(&IMakinaXErrors::NotGuardian {}.abi_encode());
        assert_eq!(
            d.explanation,
            "The address is not a guardian on this module, so it cannot be removed."
        );
        assert_eq!(
            d.remediation.as_deref(),
            Some("Check the address: only a current guardian can be removed.")
        );
        let d = decode_revert(&IMakinaXErrors::UnauthorizedCaller {}.abi_encode());
        assert_eq!(
            d.explanation,
            "The caller is not allowed to call this function; it may require the Safe, the provider, an operator or a guardian."
        );
        assert!(
            d.remediation
                .as_deref()
                .is_some_and(|r| r.contains("isOperator(<address>)"))
        );
    }

    #[test]
    fn revert_string_and_panic_and_unknown() {
        let r = alloy::sol_types::Revert::from("nope").abi_encode();
        let d = decode_revert(&r);
        assert_eq!(d.code, "revert_string");
        assert!(d.explanation.contains("nope"));

        let unknown = [0xde, 0xad, 0xbe, 0xef, 0x00];
        let d = decode_revert(&unknown);
        assert_eq!(d.code, "unknown");
        assert!(!d.known);
        assert_eq!(d.selector, "0xdeadbeef");
        assert!(d.raw.starts_with("0x"));
    }
}

#[cfg(test)]
mod weiroll_tests {
    use super::*;
    use alloy::{
        primitives::{Address, U256},
        sol_types::SolError,
    };

    #[test]
    fn execution_failed_decodes_with_structure() {
        let data = IWeirollVMErrors::ExecutionFailed {
            command_index: U256::from(3u64),
            target: Address::repeat_byte(0xBE),
            message: "ERC4626: deposit more than max".into(),
        }
        .abi_encode();
        let d = decode_revert(&data);
        assert_eq!(d.code, "weiroll_execution_failed");
        assert!(d.known);
        assert!(d.explanation.contains("command #3"), "{}", d.explanation);
        assert!(
            d.explanation.contains("deposit more than max"),
            "{}",
            d.explanation
        );
        let det = d.details.expect("structured details");
        assert_eq!(det["command_index"], "3");
        assert_eq!(det["inner_message"], "ERC4626: deposit more than max");

        // Empty inner reasons are labeled, never dropped.
        let data = IWeirollVMErrors::ExecutionFailed {
            command_index: U256::from(0u64),
            target: Address::ZERO,
            message: String::new(),
        }
        .abi_encode();
        let d = decode_revert(&data);
        assert!(
            d.explanation.contains("empty revert reason"),
            "{}",
            d.explanation
        );
    }

    /// The VM writes the placeholder "Unknown" when the target's revert
    /// carried no string reason (a bare custom error, or nothing). That word
    /// is the VM's, not the target's: it is never quoted as the reason.
    #[test]
    fn the_vm_placeholder_is_not_quoted_as_a_reason() {
        let data = IWeirollVMErrors::ExecutionFailed {
            command_index: U256::from(1u64),
            target: Address::repeat_byte(0xAB),
            message: "Unknown".into(),
        }
        .abi_encode();
        let d = decode_revert(&data);
        assert_eq!(d.code, "weiroll_execution_failed");
        assert!(
            !d.explanation.contains("Unknown"),
            "the VM's placeholder must not be quoted: {}",
            d.explanation
        );
        assert!(
            d.explanation.contains(REFUSED_WITHOUT_A_REASON),
            "{}",
            d.explanation
        );
        let det = d.details.expect("structured details");
        assert_eq!(det["inner_message"], REFUSED_WITHOUT_A_REASON);
        assert_eq!(det["reason_given"], false);
    }
}

#[cfg(test)]
mod declared_tests {
    use super::*;
    use alloy::{
        primitives::{Address, U256},
        sol_types::SolError,
    };

    fn declared(sig: &str, description: Option<&str>) -> DeclaredError {
        DeclaredError {
            signature: sig.into(),
            description: description.map(str::to_string),
            contracts: vec![],
        }
    }

    /// A declaration as a configuration file carries it: a signature and,
    /// optionally, plain words. Both spellings of a signature are accepted.
    #[test]
    fn a_declaration_parses_from_json_and_names_its_selector() {
        let d: DeclaredError = serde_json::from_value(serde_json::json!({
            "signature": "AmountTooLow()",
            "description": "The amount is below the minimum."
        }))
        .unwrap();
        assert_eq!(d.name().as_deref(), Some("AmountTooLow"));
        assert_eq!(d.selector(), Some([0x1f, 0xba, 0xba, 0x35]));
        let bare: DeclaredError =
            serde_json::from_value(serde_json::json!({ "signature": "error Foo(uint256 a)" }))
                .unwrap();
        assert_eq!(bare.name().as_deref(), Some("Foo"));
        assert!(bare.description.is_none());
        assert!(declared("not a signature(", None).selector().is_none());
    }

    #[test]
    fn a_declared_error_is_named_with_its_description() {
        let list = [declared(
            "AmountTooLow()",
            Some("The amount is below the minimum."),
        )];
        let d = decode_revert_declared(&[0x1f, 0xba, 0xba, 0x35], &list);
        assert_eq!(d.code, DECLARED_ERROR);
        assert!(d.known);
        assert_eq!(d.selector, "0x1fbaba35");
        assert_eq!(d.raw, "0x1fbaba35");
        assert_eq!(
            d.explanation,
            "AmountTooLow: The amount is below the minimum."
        );
        let det = d.details.expect("details");
        assert_eq!(det["name"], "AmountTooLow");
        assert_eq!(det["signature"], "AmountTooLow()");
        assert_eq!(det["description"], "The amount is below the minimum.");
        assert_eq!(det["args"], serde_json::json!([]));
    }

    #[test]
    fn a_declared_error_with_arguments_reports_them() {
        sol! { error BelowMin(uint256 min, address queue); }
        let data = BelowMin {
            min: U256::from(10_000_000_000_000_000u64),
            queue: Address::repeat_byte(0x22),
        }
        .abi_encode();
        let d = decode_revert_declared(
            &data,
            &[declared("BelowMin(uint256 min, address queue)", None)],
        );
        assert_eq!(d.code, DECLARED_ERROR);
        assert_eq!(d.explanation, "BelowMin");
        let det = d.details.unwrap();
        assert_eq!(det["args"][0], "10000000000000000");
        assert_eq!(det["args"][1], format!("{:?}", Address::repeat_byte(0x22)));
        assert!(det.get("description").is_none());
    }

    /// Not declared: no name is guessed. The ordinary decode applies, and an
    /// unrecognised selector stays unknown.
    #[test]
    fn an_undeclared_error_is_never_named() {
        let d = decode_revert_declared(
            &[0x1f, 0xba, 0xba, 0x35],
            &[declared("SomethingElse()", Some("words"))],
        );
        assert_eq!(d.code, "unknown");
        assert!(!d.known);
        assert_eq!(d.selector, "0x1fbaba35");
        let d = decode_revert_declared(&[0x1f, 0xba, 0xba, 0x35], &[]);
        assert_eq!(d.code, "unknown");
    }

    /// A selector match whose data does not fit the declared arguments is
    /// not that error.
    #[test]
    fn a_selector_match_with_data_that_does_not_fit_is_not_named() {
        let mut data = vec![0x1f, 0xba, 0xba, 0x35];
        data.extend([0u8; 3]);
        let d = decode_revert_declared(&data, &[declared("AmountTooLow(uint256)", None)]);
        assert_ne!(d.code, DECLARED_ERROR);
    }

    /// A revert string and a module error decode as before; a bad
    /// declaration is skipped rather than failing the decode.
    #[test]
    fn the_ordinary_decode_still_applies() {
        let list = [declared("not a signature(", None)];
        let r = alloy::sol_types::Revert::from("nope").abi_encode();
        assert_eq!(decode_revert_declared(&r, &list).code, "revert_string");
        let m = IMakinaXErrors::Paused {}.abi_encode();
        assert_eq!(decode_revert_declared(&m, &list).code, "paused");
    }

    /// No data at all: the contract gave no reason, and says so.
    #[test]
    fn an_empty_revert_says_no_reason_was_given() {
        let d = decode_revert_declared(&[], &[]);
        assert!(!d.known);
        assert!(
            d.explanation.contains(REFUSED_WITHOUT_A_REASON),
            "{}",
            d.explanation
        );
    }

    /// The failing step a simulator attached: its revert bytes decode
    /// against the declarations; without them there is nothing to decode.
    #[test]
    fn the_failing_frame_attached_to_an_error_decodes_against_declarations() {
        let mut outer = decode_revert(
            &IWeirollVMErrors::ExecutionFailed {
                command_index: U256::from(1u64),
                target: Address::repeat_byte(0xAB),
                message: "Unknown".into(),
            }
            .abi_encode(),
        );
        let list = [declared("AmountTooLow()", Some("Below the minimum."))];
        assert!(decode_failing_frame(&outer, &list).is_none());
        outer.details.as_mut().unwrap()["failing_frame"] = serde_json::json!({
            "depth": 3, "to": "0xqueue", "output": "0x1fbaba35"
        });
        let inner = decode_failing_frame(&outer, &list).expect("a frame to decode");
        assert_eq!(inner.code, DECLARED_ERROR);
        assert_eq!(inner.explanation, "AmountTooLow: Below the minimum.");
        let inner = decode_failing_frame(&outer, &[]).expect("a frame to decode");
        assert_eq!(inner.code, "unknown");
    }

    fn frame_err(to: &str, context: &str, output: &str) -> DecodedError {
        let mut outer = decode_revert(
            &IWeirollVMErrors::ExecutionFailed {
                command_index: U256::from(1u64),
                target: Address::repeat_byte(0xAB),
                message: "Unknown".into(),
            }
            .abi_encode(),
        );
        outer.details.as_mut().unwrap()["failing_frame"] =
            serde_json::json!({ "to": to, "context": context, "output": output });
        outer
    }

    /// A standard token error is declared for the vault. Raised by the token
    /// underneath, it is never attributed to the vault.
    #[test]
    fn an_error_scoped_to_one_contract_is_not_named_for_another() {
        const VAULT: &str = "0x00000000000000000000000000000000000000aa";
        const TOKEN: &str = "0x00000000000000000000000000000000000000bb";
        let mut d = declared(
            "AmountTooLow()",
            Some("Your vault share balance is too low."),
        );
        d.contracts = vec![VAULT.to_ascii_uppercase().replace("0X", "0x")];
        let from_token = frame_err(TOKEN, TOKEN, "0x1fbaba35");
        let r = decode_failing_frame(&from_token, std::slice::from_ref(&d)).unwrap();
        assert_ne!(
            r.code, DECLARED_ERROR,
            "the token's revert is not the vault's"
        );
        let from_vault = frame_err(VAULT, VAULT, "0x1fbaba35");
        let r = decode_failing_frame(&from_vault, std::slice::from_ref(&d)).unwrap();
        assert_eq!(r.code, DECLARED_ERROR);
        assert_eq!(r.details.unwrap()["scope"], "contract");
        // The vault's implementation, run through the vault proxy.
        let via_proxy = frame_err(
            "0x00000000000000000000000000000000000000cc",
            VAULT,
            "0x1fbaba35",
        );
        let r = decode_failing_frame(&via_proxy, std::slice::from_ref(&d)).unwrap();
        assert_eq!(r.code, DECLARED_ERROR);
    }

    /// No `contracts`: named for any contract, and marked chain-wide. A
    /// scoped declaration that covers the frame wins over an unscoped one.
    #[test]
    fn an_unscoped_error_is_named_chain_wide_and_a_scoped_one_wins() {
        const VAULT: &str = "0x00000000000000000000000000000000000000aa";
        let wide = declared("AmountTooLow()", Some("Too low."));
        let r = decode_failing_frame(
            &frame_err(VAULT, VAULT, "0x1fbaba35"),
            std::slice::from_ref(&wide),
        )
        .unwrap();
        assert_eq!(r.details.unwrap()["scope"], "chain");
        let mut scoped = declared("AmountTooLow()", Some("Below the vault minimum."));
        scoped.contracts = vec![VAULT.into()];
        let r =
            decode_failing_frame(&frame_err(VAULT, VAULT, "0x1fbaba35"), &[wide, scoped]).unwrap();
        assert_eq!(r.explanation, "AmountTooLow: Below the vault minimum.");
        assert_eq!(r.details.unwrap()["scope"], "contract");
    }

    #[test]
    fn contracts_are_optional_in_a_declaration() {
        let d: DeclaredError = serde_json::from_value(serde_json::json!({
            "signature": "AmountTooLow()",
            "contracts": ["0x00000000000000000000000000000000000000aa"]
        }))
        .unwrap();
        assert!(d.covers("0x00000000000000000000000000000000000000AA"));
        assert!(!d.covers("0x00000000000000000000000000000000000000bb"));
        let bare = declared("AmountTooLow()", None);
        assert!(bare.covers("0xanything"));
        assert!(
            serde_json::to_value(&bare)
                .unwrap()
                .get("contracts")
                .is_none()
        );
    }
}
