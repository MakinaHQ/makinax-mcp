//! Reading a failed transaction's call tree, so a nested out-of-gas can be
//! NAMED instead of guessed at.
//!
//! WHY. The mined-fork preflight used to diagnose a failure by replaying an
//! UNBOUNDED `eth_call` and, when that succeeded, calling the mismatch
//! "state-dependent". For an out-of-gas failure that label is not merely
//! unhelpful, it is wrong in a specific and expensive way: the unbounded replay
//! succeeds BECAUSE it is unbounded, so the diagnostic's own evidence was
//! manufactured by the difference it failed to control for. An operator could not
//! recover what gas the production failure actually had, because the
//! simulator reverted the snapshot and discarded the transaction.
//!
//! A call trace answers the question directly: which frame ran out, how much it
//! was given, and what the chain of calls above it was. `0x2152` gas arriving at
//! a USDC transfer four frames down is not ambiguous once you can see it.
//!
//! Best-effort by construction. `debug_traceTransaction` is an Anvil/Geth
//! extension; a backend without it yields `Err` and the caller says the trace
//! could not be read — never that the failure was therefore something else.

use alloy::{primitives::B256, providers::Provider, rpc::types::TransactionRequest};

/// One frame of a `callTracer` trace, flattened to what a diagnosis needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 0 = the transaction's own frame.
    pub depth: usize,
    /// CALL | DELEGATECALL | STATICCALL | CREATE …
    pub call_type: String,
    pub from: String,
    pub to: String,
    /// Gas GIVEN to this frame — the number that matters for EIP-150 starvation.
    pub gas: u64,
    pub gas_used: u64,
    pub error: Option<String>,
    /// `to` of every frame from the root down to this one, so the remedy can
    /// show the chain that starved rather than one address in isolation.
    pub path: Vec<String>,
}

impl Frame {
    /// Did THIS frame run out of gas?
    ///
    /// Matched on the whole set of spellings rather than one: Anvil says
    /// "out of gas", Geth's tracer says "out of gas" or "OutOfGas" depending on
    /// version, and revm's own variant name leaks through some builds. A
    /// classifier that recognised only one spelling would silently fall back to
    /// "reverted" on the others — the failure mode this module was built to end.
    pub fn out_of_gas(&self) -> bool {
        self.error.as_deref().is_some_and(|e| {
            let e = e.to_ascii_lowercase().replace(['_', ' '], "");
            e.contains("outofgas") || e == "oog"
        })
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "depth": self.depth,
            "call_type": self.call_type,
            "from": self.from,
            "to": self.to,
            "gas_given": self.gas,
            "gas_used": self.gas_used,
            "error": self.error,
            "call_path": self.path,
        })
    }

    /// `0xmodule → 0xsafe → 0xrouter → 0xtoken`, for a human-facing sentence.
    pub fn path_arrow(&self) -> String {
        self.path.join(" → ")
    }
}

/// A flattened call tree, pre-order (parents before children).
#[derive(Debug, Clone, Default)]
pub struct CallTrace {
    pub frames: Vec<Frame>,
}

impl CallTrace {
    /// The DEEPEST frame that ran out of gas — the one that actually starved.
    ///
    /// Deepest, not first: under EIP-150's 63/64 rule an outer frame reports OOG
    /// too once its child fails, so the outermost match names the wrong contract.
    pub fn starved_frame(&self) -> Option<&Frame> {
        self.frames
            .iter()
            .filter(|f| f.out_of_gas())
            .max_by_key(|f| f.depth)
    }

    /// The deepest frame carrying ANY error — what to show when the failure was
    /// not gas.
    pub fn terminal_frame(&self) -> Option<&Frame> {
        self.frames
            .iter()
            .filter(|f| f.error.is_some())
            .max_by_key(|f| f.depth)
    }
}

fn hex_u64(v: Option<&serde_json::Value>) -> u64 {
    v.and_then(|x| x.as_str())
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .or_else(|| v.and_then(|x| x.as_u64()))
        .unwrap_or(0)
}

fn addr(v: Option<&serde_json::Value>) -> String {
    v.and_then(|x| x.as_str())
        .unwrap_or("0x")
        .to_ascii_lowercase()
}

/// Flatten one `callTracer` node and its children, pre-order.
pub fn flatten(node: &serde_json::Value, depth: usize, parents: &[String]) -> Vec<Frame> {
    let to = addr(node.get("to"));
    let mut path = parents.to_vec();
    path.push(to.clone());
    let frame = Frame {
        depth,
        call_type: node
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("CALL")
            .to_string(),
        from: addr(node.get("from")),
        to,
        gas: hex_u64(node.get("gas")),
        gas_used: hex_u64(node.get("gasUsed")),
        error: node
            .get("error")
            .and_then(|e| e.as_str())
            .map(str::to_string),
        path: path.clone(),
    };
    let mut out = vec![frame];
    if let Some(calls) = node.get("calls").and_then(|c| c.as_array()) {
        for c in calls {
            out.extend(flatten(c, depth + 1, &path));
        }
    }
    out
}

/// `debug_traceTransaction` with the call tracer. Best effort: `Err` means the
/// trace could not be read, which is a statement about the backend and never
/// evidence about the failure.
pub async fn fetch<P: Provider>(provider: &P, tx: B256) -> std::result::Result<CallTrace, String> {
    let root = fetch_tree(provider, tx).await?;
    Ok(CallTrace {
        frames: flatten(&root, 0, &[]),
    })
}

/// The raw `callTracer` tree of a mined transaction (see [`fetch`]).
pub(crate) async fn fetch_tree<P: Provider>(
    provider: &P,
    tx: B256,
) -> std::result::Result<serde_json::Value, String> {
    let opts = serde_json::json!({ "tracer": "callTracer" });
    provider
        .raw_request("debug_traceTransaction".into(), (tx, opts))
        .await
        .map_err(|e| crate::util::redact::text(&format!("debug_traceTransaction: {e}")))
}

/// `debug_traceCall` with the call tracer: the raw call tree of a call that
/// was never mined, at the latest block. Best effort, like [`fetch`]: `Err`
/// says the backend could not trace it, nothing about the call.
pub async fn fetch_call<P: Provider>(
    provider: &P,
    call: &TransactionRequest,
) -> std::result::Result<serde_json::Value, String> {
    let opts = serde_json::json!({ "tracer": "callTracer" });
    provider
        .raw_request("debug_traceCall".into(), (call, "latest", opts))
        .await
        .map_err(|e| crate::util::redact::text(&format!("debug_traceCall: {e}")))
}

/// The call a revert started in, with the bytes it reverted with.
///
/// A revert that passes through a wrapper (a batch executor, a proxy) is
/// re-encoded or replaced on the way out, so the transaction's own revert
/// data can lose the original reason. The innermost failing frame still
/// holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailingFrame {
    /// 0 = the transaction's own frame.
    pub depth: usize,
    pub call_type: String,
    pub to: String,
    /// The frame's revert data (`output`); empty when it reverted with none.
    pub output: Vec<u8>,
    /// The tracer's own decoding of the revert (`revertReason`), when it gave
    /// one.
    pub revert_reason: Option<String>,
    /// The address whose storage the failing code ran against: `to`, or for a
    /// DELEGATECALL frame the address the delegation ran in (a proxy).
    pub context: String,
    /// `to` of every frame from the root down to this one.
    pub path: Vec<String>,
}

impl FailingFrame {
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "depth": self.depth,
            "call_type": self.call_type,
            "to": self.to,
            "context": self.context,
            "output": format!("0x{}", alloy::primitives::hex::encode(&self.output)),
            "revert_reason": self.revert_reason,
            "call_path": self.path,
        })
    }
}

/// Safe module-execution selectors whose call can fail without reverting.
/// Both report a failed inner call through their return value, so the
/// tracer marks the frame as a success.
/// `execTransactionFromModule(address,uint256,bytes,uint8)` returns
/// `bool success`.
const EXEC_FROM_MODULE: [u8; 4] = [0x46, 0x87, 0x21, 0xa7];
/// `execTransactionFromModuleReturnData(address,uint256,bytes,uint8)`
/// returns `(bool success, bytes returnData)`.
const EXEC_FROM_MODULE_RETURN_DATA: [u8; 4] = [0x52, 0x29, 0x07, 0x3f];

fn hex_bytes(v: Option<&serde_json::Value>) -> Vec<u8> {
    v.and_then(|o| o.as_str())
        .and_then(|o| alloy::primitives::hex::decode(o.trim_start_matches("0x")).ok())
        .unwrap_or_default()
}

/// A 32-byte ABI word read as an offset or length, `None` when it is out of
/// range for `usize`.
fn abi_usize(word: &[u8]) -> Option<usize> {
    let (high, low) = word.split_at(24);
    if high.iter().any(|b| *b != 0) {
        return None;
    }
    usize::try_from(u64::from_be_bytes(low.try_into().ok()?)).ok()
}

/// Did this frame fail without reverting, and with what data? `Some` for a
/// module-execution call on a Safe (or the delegation from a Safe proxy to
/// its implementation, which carries the same input and output) that
/// returned normally with `success = false` as its first return word. The
/// data is the `returnData` it handed back: the inner call's revert bytes,
/// or nothing for the single-`bool` form.
fn reported_failure(node: &serde_json::Value) -> Option<Vec<u8>> {
    if node.get("error").is_some() {
        return None;
    }
    let input = hex_bytes(node.get("input"));
    let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
    let output = hex_bytes(node.get("output"));
    if output.len() < 32 || output[..32].iter().any(|b| *b != 0) {
        return None;
    }
    match selector {
        EXEC_FROM_MODULE => Some(Vec::new()),
        EXEC_FROM_MODULE_RETURN_DATA => {
            let offset = abi_usize(output.get(32..64)?)?;
            let len = abi_usize(output.get(offset..offset.checked_add(32)?)?)?;
            let start = offset.checked_add(32)?;
            output
                .get(start..start.checked_add(len)?)
                .map(<[u8]>::to_vec)
        }
        _ => None,
    }
}

/// Follow a failed `callTracer` tree down to the frame the failure started in.
///
/// A failure is followed into a child only when that child is the parent's
/// LAST call and it failed: a revert that propagates ends the parent's
/// execution, so nothing is called after it. A failing call followed by
/// further calls was handled (try/catch, a checked low-level call) and did
/// not cause the parent's revert, so the walk stops at the parent and keeps
/// the parent's own bytes.
///
/// A child also "failed" when it returned normally but reported failure
/// through its return value (see [`reported_failure`]): a Safe running a
/// module's call returns `success = false` instead of reverting, and the
/// caller then reverts on its behalf. Such a child is followed only when the
/// failure was passed on unchanged, that is when the data it handed back
/// equals the revert data of the deepest frame above it that reverted. A
/// caller that reverts with an error of its own instead is reported as that
/// error. A frame that only reported failure has no revert data of its own,
/// so it is never the frame reported: the walk continues through it into the
/// call that actually reverted, and falls back to the deepest frame on the
/// path that did revert when nothing below it did.
///
/// Limits: a parent that handles its last call's failure and then reverts
/// with bytes of its own looks the same as a wrapper that re-encodes the
/// child's revert on the way out; both are followed. A Safe that makes
/// another call after the failed one (a module guard's post-check) ends the
/// walk at that Safe, as any later call does. `None` when the root did not
/// fail.
pub fn failing_frame(root: &serde_json::Value) -> Option<FailingFrame> {
    root.get("error")?;
    let mut node = root;
    let mut depth = 0;
    let mut path = vec![addr(node.get("to"))];
    let mut context = addr(node.get("to"));
    // The deepest frame on the path that reverted, with its depth, path
    // length and context: what is reported if the walk ends on a frame that
    // only reported failure.
    let mut reverted = (node, depth, path.len(), context.clone());
    while let Some(child) = node
        .get("calls")
        .and_then(|c| c.as_array())
        .and_then(|calls| calls.last())
        .filter(|c| {
            c.get("error").is_some()
                || reported_failure(c)
                    .is_some_and(|data| data == hex_bytes(reverted.0.get("output")))
        })
    {
        node = child;
        depth += 1;
        let to = addr(node.get("to"));
        path.push(to.clone());
        if node.get("type").and_then(|t| t.as_str()) != Some("DELEGATECALL") {
            context = to;
        }
        if node.get("error").is_some() {
            reverted = (node, depth, path.len(), context.clone());
        }
    }
    let (node, depth, path_len, context) = reverted;
    path.truncate(path_len);
    Some(FailingFrame {
        depth,
        call_type: node
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("CALL")
            .to_string(),
        to: addr(node.get("to")),
        context,
        output: hex_bytes(node.get("output")),
        revert_reason: node
            .get("revertReason")
            .and_then(|r| r.as_str())
            .map(str::to_string),
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production shape: module → Safe → router → token, with the token
    /// frame starved. Gas figures are the measured ones from a live Base swap:
    /// limit 1,139,794, used 1,122,697, and
    /// `0x2152` (8,530) arriving at the innermost frame.
    fn nested_oog() -> serde_json::Value {
        serde_json::json!({
            "type": "CALL", "from": "0x0f9d", "to": "0xMODULE",
            "gas": "0x1160D2", "gasUsed": "0x112189", "error": "out of gas",
            "calls": [{
                "type": "CALL", "from": "0xMODULE", "to": "0xSAFE",
                "gas": "0x10E000", "gasUsed": "0x10D000", "error": "out of gas",
                "calls": [{
                    "type": "DELEGATECALL", "from": "0xSAFE", "to": "0xROUTER",
                    "gas": "0x10C000", "gasUsed": "0x10B000", "error": "out of gas",
                    "calls": [{
                        "type": "CALL", "from": "0xROUTER", "to": "0xUSDC",
                        "gas": "0x2152", "gasUsed": "0x2152", "error": "out of gas"
                    }]
                }]
            }]
        })
    }

    #[test]
    fn the_deepest_starved_frame_is_the_one_named() {
        let t = CallTrace {
            frames: flatten(&nested_oog(), 0, &[]),
        };
        assert_eq!(t.frames.len(), 4);
        let starved = t.starved_frame().expect("an OOG frame");
        // Deepest, not first: the outer frames report OOG too once the child
        // fails, and naming the module instead of the token is the wrong answer.
        assert_eq!(starved.depth, 3);
        assert_eq!(starved.to, "0xusdc");
        assert_eq!(starved.gas, 0x2152, "the measured starved budget");
        assert_eq!(starved.call_type, "CALL");
        assert_eq!(
            starved.path_arrow(),
            "0xmodule → 0xsafe → 0xrouter → 0xusdc",
            "the chain, not one address in isolation"
        );
    }

    #[test]
    fn a_revert_is_not_an_out_of_gas() {
        let node = serde_json::json!({
            "type": "CALL", "from": "0xa", "to": "0xb",
            "gas": "0x100000", "gasUsed": "0x1000",
            "calls": [{ "type": "CALL", "from": "0xb", "to": "0xc",
                        "gas": "0xF0000", "gasUsed": "0x500",
                        "error": "execution reverted" }]
        });
        let t = CallTrace {
            frames: flatten(&node, 0, &[]),
        };
        assert!(t.starved_frame().is_none(), "no frame ran out of gas");
        let terminal = t.terminal_frame().expect("an erroring frame");
        assert_eq!(terminal.to, "0xc");
        assert!(!terminal.out_of_gas());
    }

    /// Every spelling a backend might use. A classifier that knew one of these
    /// would quietly mislabel the others as reverts.
    #[test]
    fn every_spelling_of_out_of_gas_is_recognised() {
        for spelling in ["out of gas", "OutOfGas", "OUT_OF_GAS", "outofgas", "oog"] {
            let f = Frame {
                depth: 0,
                call_type: "CALL".into(),
                from: "0xa".into(),
                to: "0xb".into(),
                gas: 1,
                gas_used: 1,
                error: Some(spelling.into()),
                path: vec!["0xb".into()],
            };
            assert!(f.out_of_gas(), "{spelling} must classify as out of gas");
        }
        for other in ["execution reverted", "invalid jump destination", "reverted"] {
            let f = Frame {
                depth: 0,
                call_type: "CALL".into(),
                from: "0xa".into(),
                to: "0xb".into(),
                gas: 1,
                gas_used: 1,
                error: Some(other.into()),
                path: vec!["0xb".into()],
            };
            assert!(!f.out_of_gas(), "{other} is not out of gas");
        }
    }

    /// A step of a batch fails inside a proxied contract with a bare custom
    /// error; the batch wraps it in its own error on the way out. The frame
    /// the failure started in is the one whose bytes are kept.
    fn wrapped_custom_error() -> serde_json::Value {
        serde_json::json!({
            "type": "CALL", "from": "0xop", "to": "0xMODULE",
            "gas": "0x100000", "gasUsed": "0x9000",
            "error": "execution reverted", "output": "0x9b4cdcbd0000",
            "calls": [
                { "type": "STATICCALL", "from": "0xmodule", "to": "0xORACLE",
                  "gas": "0x1000", "gasUsed": "0x100", "output": "0x01" },
                { "type": "CALL", "from": "0xmodule", "to": "0xSAFE",
                  "gas": "0xF0000", "gasUsed": "0x8000",
                  "error": "execution reverted", "output": "0x9b4cdcbd0000",
                  "calls": [
                    { "type": "CALL", "from": "0xsafe", "to": "0xOTHER",
                      "gas": "0x1000", "gasUsed": "0x100",
                      "error": "execution reverted", "output": "0xdeadbeef" },
                    { "type": "CALL", "from": "0xsafe", "to": "0xQUEUE",
                      "gas": "0x8000", "gasUsed": "0x400",
                      "error": "execution reverted", "output": "0x1fbaba35",
                      "calls": [
                        { "type": "DELEGATECALL", "from": "0xqueue", "to": "0xIMPL",
                          "gas": "0x7000", "gasUsed": "0x300",
                          "error": "execution reverted", "output": "0x1fbaba35",
                          "revertReason": "AmountTooLow()" }
                      ] }
                  ] }
            ]
        })
    }

    #[test]
    fn the_failure_is_followed_down_to_the_frame_it_started_in() {
        let f = failing_frame(&wrapped_custom_error()).expect("a failing frame");
        assert_eq!(f.to, "0ximpl");
        assert_eq!(f.depth, 3);
        assert_eq!(f.output, vec![0x1f, 0xba, 0xba, 0x35]);
        assert_eq!(f.revert_reason.as_deref(), Some("AmountTooLow()"));
        assert_eq!(f.path, vec!["0xmodule", "0xsafe", "0xqueue", "0ximpl"]);
        assert_eq!(f.context, "0xqueue", "the proxy the implementation ran in");
        let j = f.json();
        assert_eq!(j["output"], "0x1fbaba35");
        assert_eq!(j["to"], "0ximpl");
        assert_eq!(j["revert_reason"], "AmountTooLow()");
        assert_eq!(j["context"], "0xqueue");
    }

    /// A contract catches a failed sub-call, calls on, and then reverts with
    /// its own different error. The handled failure is not the reason: the
    /// walk stops at the contract and keeps ITS bytes.
    #[test]
    fn a_handled_inner_revert_is_not_followed_past_a_later_call() {
        let tree = serde_json::json!({
            "type": "CALL", "from": "0xop", "to": "0xMODULE",
            "gas": "0x100000", "gasUsed": "0x9000",
            "error": "execution reverted", "output": "0xaaaaaaaa",
            "calls": [{
                "type": "CALL", "from": "0xmodule", "to": "0xVAULT",
                "gas": "0x8000", "gasUsed": "0x800",
                "error": "execution reverted", "output": "0xaaaaaaaa",
                "calls": [
                    { "type": "CALL", "from": "0xvault", "to": "0xTOKEN",
                      "gas": "0x1000", "gasUsed": "0x100",
                      "error": "execution reverted", "output": "0x1fbaba35" },
                    { "type": "STATICCALL", "from": "0xvault", "to": "0xORACLE",
                      "gas": "0x1000", "gasUsed": "0x100", "output": "0x01" }
                ]
            }]
        });
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, "0xvault");
        assert_eq!(
            f.output,
            vec![0xaa; 4],
            "the vault's own revert, not the token's"
        );
        assert_eq!(f.depth, 1);
    }

    /// An earlier call that failed and was handled is not the failure: the
    /// LAST failing call at each level is the one the revert came from.
    #[test]
    fn an_earlier_handled_failure_is_not_followed() {
        let f = failing_frame(&wrapped_custom_error()).unwrap();
        assert_ne!(f.to, "0xother");
    }

    #[test]
    fn a_successful_call_has_no_failing_frame_and_a_bare_revert_keeps_empty_output() {
        let ok = serde_json::json!({ "type": "CALL", "from": "0xa", "to": "0xb",
                                     "gas": "0x1", "gasUsed": "0x1", "output": "0x" });
        assert!(failing_frame(&ok).is_none());
        let bare = serde_json::json!({ "type": "CALL", "from": "0xa", "to": "0xb",
                                       "gas": "0x1", "gasUsed": "0x1",
                                       "error": "execution reverted" });
        let f = failing_frame(&bare).unwrap();
        assert!(f.output.is_empty());
        assert!(f.revert_reason.is_none());
        assert_eq!(f.json()["output"], "0x");
    }

    #[test]
    fn a_clean_trace_names_nothing() {
        let node = serde_json::json!({
            "type": "CALL", "from": "0xa", "to": "0xb", "gas": "0x1", "gasUsed": "0x1"
        });
        let t = CallTrace {
            frames: flatten(&node, 0, &[]),
        };
        assert!(t.starved_frame().is_none());
        assert!(t.terminal_frame().is_none());
    }

    // A module call that runs a command list through a Safe, trimmed from a
    // traced redeem that failed. Addresses are synthetic. Shape:
    //
    //   operator → module proxy ⇒ module impl          (reverts, VM error)
    //     … earlier successful calls …
    //     → Safe proxy  execTransactionFromModuleReturnData   (NO error:
    //       ⇒ Safe impl   returns (false, returnData))
    //         ⇒ VM        (reverts ExecutionFailed(0, vault, "Unknown"))
    //           → vault   (reverts ERC20InsufficientBalance)
    //             → market (succeeds, the vault's last call)
    const OPERATOR: &str = "0x00000000000000000000000000000000000000a1";
    const MODULE: &str = "0x00000000000000000000000000000000000000b1";
    const MODULE_IMPL: &str = "0x00000000000000000000000000000000000000b2";
    const SAFE: &str = "0x00000000000000000000000000000000000000c1";
    const SAFE_IMPL: &str = "0x00000000000000000000000000000000000000c2";
    const VM: &str = "0x00000000000000000000000000000000000000d1";
    const VAULT: &str = "0x00000000000000000000000000000000000000e1";
    const MARKET: &str = "0x00000000000000000000000000000000000000f1";
    const ORACLE: &str = "0x00000000000000000000000000000000000000f2";

    /// `ExecutionFailed(0, VAULT, "Unknown")`: the VM's wrapper when the
    /// target's revert was not a string.
    const VM_UNKNOWN: &str = "0xef3dcb2f\
        0000000000000000000000000000000000000000000000000000000000000000\
        00000000000000000000000000000000000000000000000000000000000000e1\
        0000000000000000000000000000000000000000000000000000000000000060\
        0000000000000000000000000000000000000000000000000000000000000007\
        556e6b6e6f776e00000000000000000000000000000000000000000000000000";

    /// `ERC20InsufficientBalance(SAFE, 0, 1e18)`.
    const INSUFFICIENT_BALANCE: &str = "0xe450d38c\
        00000000000000000000000000000000000000000000000000000000000000c1\
        0000000000000000000000000000000000000000000000000000000000000000\
        0000000000000000000000000000000000000000000000000de0b6b3a7640000";

    /// ABI `(bool success, bytes returnData)` with `success = false`.
    fn safe_returned_failure(return_data: &str) -> String {
        let data = return_data.trim_start_matches("0x");
        let len = data.len() / 2;
        let padded = format!("{data:0<width$}", width = len.div_ceil(32) * 64);
        format!("0x{:064x}{:064x}{:064x}{padded}", 0, 0x40, len)
    }

    fn module_through_safe(vault_output: &str) -> serde_json::Value {
        let safe_out = safe_returned_failure(VM_UNKNOWN);
        let vault = serde_json::json!({
            "type": "CALL", "from": SAFE, "to": VAULT, "input": "0xba087652",
            "gas": "0x16036d44", "gasUsed": "0x2b693",
            "error": "execution reverted", "output": vault_output,
            "calls": [{
                "type": "CALL", "from": VAULT, "to": MARKET, "input": "0x5c2bea49",
                "gas": "0x15a91eb3", "gasUsed": "0x6966", "output": "0x"
            }]
        });
        let vm = serde_json::json!({
            "type": "DELEGATECALL", "from": SAFE, "to": VM, "input": "0xde792d5f",
            "gas": "0x165cf31d", "gasUsed": "0x2cd89",
            "error": "execution reverted", "output": VM_UNKNOWN,
            "calls": [vault]
        });
        let safe = serde_json::json!({
            "type": "CALL", "from": MODULE, "to": SAFE, "input": "0x5229073f",
            "gas": "0x171444e4", "gasUsed": "0x2f7c0", "output": safe_out,
            "calls": [{
                "type": "DELEGATECALL", "from": SAFE, "to": SAFE_IMPL, "input": "0x5229073f",
                "gas": "0x16b7f259", "gasUsed": "0x2f5fd", "output": safe_out,
                "calls": [vm]
            }]
        });
        let earlier = serde_json::json!({
            "type": "STATICCALL", "from": MODULE, "to": ORACLE, "input": "0x313ce567",
            "gas": "0x1000", "gasUsed": "0x100", "output": format!("0x{:064x}", 8)
        });
        serde_json::json!({
            "type": "CALL", "from": OPERATOR, "to": MODULE, "input": "0x311b0198",
            "gas": "0x17d6f1c0", "gasUsed": "0x91694",
            "error": "execution reverted", "output": VM_UNKNOWN,
            "calls": [{
                "type": "DELEGATECALL", "from": MODULE, "to": MODULE_IMPL, "input": "0x311b0198",
                "gas": "0x17778a5b", "gasUsed": "0x87854",
                "error": "execution reverted", "output": VM_UNKNOWN,
                "calls": [earlier, safe]
            }]
        })
    }

    /// The caller's view: the module's own revert, decoded, with the traced
    /// failing frame attached as the simulator attaches it.
    fn decode_with_frame(
        root: &serde_json::Value,
        declared: &[crate::chain::taxonomy::DeclaredError],
    ) -> crate::chain::taxonomy::DecodedError {
        use crate::chain::taxonomy::{decode_failing_frame, decode_revert};
        let raw = hex_bytes(root.get("output"));
        let mut outer = decode_revert(&raw);
        let frame = failing_frame(root).expect("a failing frame");
        outer.details = Some(serde_json::json!({ "failing_frame": frame.json() }));
        decode_failing_frame(&outer, declared).expect("a frame to decode")
    }

    fn insufficient_balance() -> crate::chain::taxonomy::DeclaredError {
        crate::chain::taxonomy::DeclaredError {
            signature: "ERC20InsufficientBalance(address sender, uint256 balance, uint256 needed)"
                .into(),
            description: Some("not enough shares".into()),
            contracts: vec![],
        }
    }

    #[test]
    fn the_fixture_encodes_a_reported_failure() {
        let tree = module_through_safe(INSUFFICIENT_BALANCE);
        let safe = &tree["calls"][0]["calls"][1];
        let vm_unknown = hex_bytes(Some(&serde_json::json!(VM_UNKNOWN)));
        assert!(safe.get("error").is_none(), "the Safe frame did not revert");
        assert_eq!(reported_failure(safe), Some(vm_unknown.clone()));
        assert_eq!(
            reported_failure(&safe["calls"][0]),
            Some(vm_unknown),
            "the Safe implementation"
        );
        let ok = serde_json::json!({ "input": "0x5229073f",
            "output": format!("0x{:064x}{:064x}{:064x}", 1, 0x40, 0) });
        assert_eq!(
            reported_failure(&ok),
            None,
            "success = true is not a failure"
        );
        let two = serde_json::json!({ "input": "0x468721a7", "output": format!("0x{:064x}", 2) });
        assert_eq!(reported_failure(&two), None, "any non-zero bool is true");
        let bool_false =
            serde_json::json!({ "input": "0x468721a7", "output": format!("0x{:064x}", 0) });
        assert_eq!(
            reported_failure(&bool_false),
            Some(vec![]),
            "the single-bool form"
        );
        let short = serde_json::json!({ "input": "0x5229073f", "output": format!("0x{:064x}", 0) });
        assert_eq!(
            reported_failure(&short),
            None,
            "returnData missing: malformed"
        );
        let other = serde_json::json!({ "input": "0xa9059cbb", "output": format!("0x{:064x}", 0) });
        assert_eq!(
            reported_failure(&other),
            None,
            "only the module-execution ABI"
        );
    }

    /// The Safe reports the inner failure as `success = false` without
    /// reverting; the walk goes through it to the vault, whose declared
    /// error is what the caller gets.
    #[test]
    fn a_failure_reported_by_a_safe_is_followed_to_the_call_that_reverted() {
        let tree = module_through_safe(INSUFFICIENT_BALANCE);
        let f = failing_frame(&tree).expect("a failing frame");
        assert_eq!(f.to, VAULT);
        assert_eq!(f.context, VAULT);
        assert_eq!(f.depth, 5);
        assert_eq!(
            f.path,
            vec![MODULE, MODULE_IMPL, SAFE, SAFE_IMPL, VM, VAULT]
        );
        assert_eq!(f.output[..4], [0xe4, 0x50, 0xd3, 0x8c]);

        let d = decode_with_frame(&tree, &[insufficient_balance()]);
        assert_eq!(d.code, crate::chain::taxonomy::DECLARED_ERROR);
        assert_eq!(d.selector, "0xe450d38c");
        assert!(
            !d.explanation
                .contains(crate::chain::taxonomy::REFUSED_WITHOUT_A_REASON),
            "{}",
            d.explanation
        );
        let det = d.details.expect("details");
        assert_eq!(det["name"], "ERC20InsufficientBalance");
        assert_eq!(det["args"][1], "0");
        assert_eq!(det["args"][2], "1000000000000000000");
    }

    /// The same path where the vault reverts with no data at all: there is
    /// no reason to find, and none is invented.
    #[test]
    fn a_reasonless_failure_behind_a_safe_still_has_no_reason() {
        let tree = module_through_safe("0x");
        let f = failing_frame(&tree).expect("a failing frame");
        assert_eq!(f.to, VAULT);
        assert!(f.output.is_empty());
        let d = decode_with_frame(&tree, &[insufficient_balance()]);
        assert_ne!(d.code, crate::chain::taxonomy::DECLARED_ERROR);
        assert!(!d.known);
        assert!(
            d.explanation
                .contains(crate::chain::taxonomy::REFUSED_WITHOUT_A_REASON),
            "{}",
            d.explanation
        );
    }

    /// A Safe frame that reports failure but whose inner call is missing
    /// from the trace has no revert data of its own; the frame reported is
    /// the deepest one that did revert.
    #[test]
    fn a_reported_failure_with_nothing_below_falls_back_to_the_last_revert() {
        let mut tree = module_through_safe(INSUFFICIENT_BALANCE);
        tree["calls"][0]["calls"][1]["calls"] = serde_json::json!([]);
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, MODULE_IMPL);
        assert_eq!(f.depth, 1);
        assert_eq!(f.path, vec![MODULE, MODULE_IMPL]);
        assert_eq!(f.context, MODULE);
        assert_eq!(f.output, hex_bytes(Some(&serde_json::json!(VM_UNKNOWN))));
    }

    /// A Safe call that succeeded is not followed: the module's revert is
    /// its own.
    #[test]
    fn a_successful_safe_call_is_not_followed() {
        let mut tree = module_through_safe(INSUFFICIENT_BALANCE);
        let safe = &mut tree["calls"][0]["calls"][1];
        let ok = format!("0x{:064x}{:064x}{:064x}", 1, 0x40, 0);
        safe["output"] = serde_json::json!(ok);
        safe["calls"][0]["output"] = serde_json::json!(ok);
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, MODULE_IMPL);
    }

    /// A Safe proxy frame and its delegation to the implementation, both
    /// returning `output` for module-execution call `input`, around `inner`.
    fn safe_pair(
        from: &str,
        safe: &str,
        input: &str,
        output: &str,
        inner: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "CALL", "from": from, "to": safe, "input": input,
            "gas": "0x100000", "gasUsed": "0x8000", "output": output,
            "calls": [{
                "type": "DELEGATECALL", "from": safe, "to": SAFE_IMPL, "input": input,
                "gas": "0xF0000", "gasUsed": "0x7000", "output": output,
                "calls": [inner]
            }]
        })
    }

    /// A module frame (proxy ⇒ implementation) reverting with `output` after
    /// `last`.
    fn module(output: &str, last: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "type": "CALL", "from": OPERATOR, "to": MODULE, "input": "0x311b0198",
            "gas": "0x200000", "gasUsed": "0x9000",
            "error": "execution reverted", "output": output,
            "calls": [{
                "type": "DELEGATECALL", "from": MODULE, "to": MODULE_IMPL, "input": "0x311b0198",
                "gas": "0x1F0000", "gasUsed": "0x8800",
                "error": "execution reverted", "output": output,
                "calls": [last]
            }]
        })
    }

    fn reverting(from: &str, to: &str, output: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "CALL", "from": from, "to": to, "input": "0xba087652",
            "gas": "0x1000", "gasUsed": "0x800",
            "error": "execution reverted", "output": output
        })
    }

    /// The single-`bool` form hands nothing back, so a module that passes
    /// the failure on unchanged reverts with no data; the walk goes through
    /// the Safe to the call that reverted.
    #[test]
    fn a_single_bool_safe_failure_is_followed() {
        let tree = module(
            "0x",
            safe_pair(
                MODULE,
                SAFE,
                "0x468721a7",
                &format!("0x{:064x}", 0),
                reverting(SAFE, VAULT, INSUFFICIENT_BALANCE),
            ),
        );
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, VAULT);
        assert_eq!(f.path, vec![MODULE, MODULE_IMPL, SAFE, SAFE_IMPL, VAULT]);
        let d = decode_with_frame(&tree, &[insufficient_balance()]);
        assert_eq!(d.code, crate::chain::taxonomy::DECLARED_ERROR);
        assert_eq!(d.selector, "0xe450d38c");
    }

    /// A module that reverts with an error of its own after the single-bool
    /// form reported failure is reported as that error.
    #[test]
    fn a_module_error_after_a_single_bool_failure_is_kept() {
        let tree = module(
            "0x0badc0de",
            safe_pair(
                MODULE,
                SAFE,
                "0x468721a7",
                &format!("0x{:064x}", 0),
                reverting(SAFE, VAULT, INSUFFICIENT_BALANCE),
            ),
        );
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, MODULE_IMPL);
        assert_eq!(f.output, vec![0x0b, 0xad, 0xc0, 0xde]);
    }

    /// A module catches the Safe's `success = false` and reverts with its
    /// own, different error: that error is the reason, as before, and the
    /// inner call is not followed.
    #[test]
    fn a_module_that_reverts_with_its_own_error_is_reported_as_that_error() {
        let mut tree = module_through_safe(INSUFFICIENT_BALANCE);
        tree["output"] = serde_json::json!("0x0badc0de");
        tree["calls"][0]["output"] = serde_json::json!("0x0badc0de");
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, MODULE_IMPL);
        assert_eq!(f.depth, 1);
        assert_eq!(f.path, vec![MODULE, MODULE_IMPL]);
        assert_eq!(f.output, vec![0x0b, 0xad, 0xc0, 0xde]);
        let d = decode_with_frame(&tree, &[insufficient_balance()]);
        assert_ne!(d.code, crate::chain::taxonomy::DECLARED_ERROR);
        assert_eq!(d.selector, "0x0badc0de");
    }

    /// Two Safes, each run by a module that passes the failure on
    /// unchanged: the walk goes through both to the call that reverted.
    #[test]
    fn a_failure_through_nested_safes_is_followed() {
        const SAFE_B: &str = "0x00000000000000000000000000000000000000c3";
        const MODULE_B: &str = "0x00000000000000000000000000000000000000b3";
        let out = safe_returned_failure(VM_UNKNOWN);
        let vm = serde_json::json!({
            "type": "DELEGATECALL", "from": SAFE_B, "to": VM, "input": "0xde792d5f",
            "gas": "0x8000", "gasUsed": "0x4000",
            "error": "execution reverted", "output": VM_UNKNOWN,
            "calls": [reverting(SAFE_B, VAULT, INSUFFICIENT_BALANCE)]
        });
        let module_b = serde_json::json!({
            "type": "CALL", "from": SAFE, "to": MODULE_B, "input": "0x311b0198",
            "gas": "0xA0000", "gasUsed": "0x6000",
            "error": "execution reverted", "output": VM_UNKNOWN,
            "calls": [safe_pair(MODULE_B, SAFE_B, "0x5229073f", &out, vm)]
        });
        let tree = module(
            VM_UNKNOWN,
            safe_pair(MODULE, SAFE, "0x5229073f", &out, module_b),
        );
        let f = failing_frame(&tree).unwrap();
        assert_eq!(f.to, VAULT);
        assert_eq!(f.context, VAULT);
        assert_eq!(
            f.path,
            vec![
                MODULE,
                MODULE_IMPL,
                SAFE,
                SAFE_IMPL,
                MODULE_B,
                SAFE_B,
                SAFE_IMPL,
                VM,
                VAULT
            ]
        );
        let d = decode_with_frame(&tree, &[insufficient_balance()]);
        assert_eq!(d.code, crate::chain::taxonomy::DECLARED_ERROR);
    }
}
