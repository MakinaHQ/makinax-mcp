//! Generic on-chain check runner for a caller-declared runtime requirement:
//! a requirement carries `check`/`set` as a human-readable solidity
//! signature + string args (`{"fn":"isFeedRouteRegistered(address)",
//! "args":["0x…"],"expect":"true"}`); this module turns that into an
//! eth_call (checks) or ready-to-paste Tx-Builder calldata (fixes).
//!
//! Reliability contract (unchanged from the rc series): a transport failure
//! is an `Err` that the probe maps to `unverified`, NEVER `missing`.

use alloy::{
    dyn_abi::{DynSolType, DynSolValue, FunctionExt, JsonAbiExt, Specifier},
    json_abi::Function,
    primitives::{Address, U256},
    providers::Provider,
    rpc::types::TransactionRequest,
};

use crate::{Error, Result, chain::provider::RootProvider};

/// Parse a solidity signature + string args into calldata.
pub fn encode_call(fn_sig: &str, args: &[String]) -> Result<Vec<u8>> {
    let f = Function::parse(fn_sig).map_err(|e| {
        Error::Config(format!(
            "declared requirement: bad fn signature `{fn_sig}`: {e}"
        ))
    })?;
    if f.inputs.len() != args.len() {
        return Err(Error::ArgCount {
            sig: fn_sig.to_string(),
            expected: f.inputs.len(),
            supplied: args.len(),
        });
    }
    let mut values = Vec::with_capacity(args.len());
    for (input, raw) in f.inputs.iter().zip(args) {
        let ty: DynSolType = input.resolve().map_err(|e| {
            Error::Config(format!(
                "declared requirement: unsupported type in `{fn_sig}`: {e}"
            ))
        })?;
        values.push(coerce(&ty, raw).map_err(|e| {
            Error::Config(format!(
                "declared requirement: arg `{raw}` does not fit `{}`: {e}",
                input.ty
            ))
        })?);
    }
    f.abi_encode_input(&values).map_err(|e| {
        Error::Config(format!(
            "declared requirement: encoding `{fn_sig}` failed: {e}"
        ))
    })
}

/// String → typed value, matching how callers write args (decimal for
/// integers, 0x-hex for addresses/bytes, true/false for bools).
fn coerce(ty: &DynSolType, raw: &str) -> std::result::Result<DynSolValue, String> {
    let raw = raw.trim();
    match ty {
        DynSolType::Address => raw
            .parse::<Address>()
            .map(DynSolValue::Address)
            .map_err(|e| e.to_string()),
        DynSolType::Bool => match raw {
            "true" | "1" => Ok(DynSolValue::Bool(true)),
            "false" | "0" => Ok(DynSolValue::Bool(false)),
            _ => Err("expected true/false".into()),
        },
        DynSolType::Uint(bits) => {
            let v = if let Some(hex) = raw.strip_prefix("0x") {
                U256::from_str_radix(hex, 16).map_err(|e| e.to_string())?
            } else {
                U256::from_str_radix(raw, 10).map_err(|e| e.to_string())?
            };
            Ok(DynSolValue::Uint(v, *bits))
        }
        DynSolType::Int(bits) => {
            let v: alloy::primitives::I256 = raw
                .parse()
                .map_err(|e: alloy::primitives::ParseSignedError| e.to_string())?;
            Ok(DynSolValue::Int(v, *bits))
        }
        DynSolType::Bytes => {
            let b = alloy::hex::decode(raw.trim_start_matches("0x")).map_err(|e| e.to_string())?;
            Ok(DynSolValue::Bytes(b))
        }
        DynSolType::FixedBytes(n) => {
            let b = alloy::hex::decode(raw.trim_start_matches("0x")).map_err(|e| e.to_string())?;
            if b.len() != *n {
                return Err(format!("expected {n} bytes, got {}", b.len()));
            }
            let mut word = [0u8; 32];
            word[..*n].copy_from_slice(&b);
            Ok(DynSolValue::FixedBytes(word.into(), *n))
        }
        DynSolType::String => Ok(DynSolValue::String(raw.to_string())),
        // A struct argument, written the way solidity writes one:
        // `(0xabc…,0,0x00…,1,2)`. Nested tuples nest.
        //
        // The stakes are not the usual encoder stakes. This path carries
        // `createModuleFree`'s init params, and `chain::module_factory` states
        // them plainly: **the wrong calldata here deploys a module nobody
        // controls.** A silently mis-ordered or mis-sized field produces
        // perfectly valid calldata for the wrong module — so every failure
        // below names the position, and the round-trip test encodes the same
        // struct through alloy's typed `sol!` path and demands byte equality
        // rather than trusting bytes written by hand.
        DynSolType::Tuple(inner) => {
            let body = raw
                .strip_prefix('(')
                .and_then(|r| r.strip_suffix(')'))
                .ok_or_else(|| {
                    format!(
                        "expected a parenthesised tuple of {} values, e.g. `(a,b)` — got `{raw}`",
                        inner.len()
                    )
                })?;
            let parts = split_top_level(body);
            if parts.len() != inner.len() {
                return Err(format!(
                    "tuple takes {} values, got {}: {parts:?}",
                    inner.len(),
                    parts.len()
                ));
            }
            let mut out = Vec::with_capacity(inner.len());
            for (i, (ty, part)) in inner.iter().zip(&parts).enumerate() {
                // Positional, because a mis-placed field is the failure this
                // is most exposed to and "field 3" is what makes it findable.
                out.push(coerce(ty, part).map_err(|e| format!("tuple field {i} (`{ty}`): {e}"))?);
            }
            Ok(DynSolValue::Tuple(out))
        }
        other => Err(format!("unsupported declared-arg type {other}")),
    }
}

/// Split a tuple body on commas that are NOT inside a nested tuple or array.
///
/// A naive `split(',')` cuts `(1,(2,3))` into three pieces and then reports an
/// arity error about a tuple that was written correctly — the error would send
/// the reader to the wrong place entirely.
///
/// An empty body is zero values, not one empty one.
fn split_top_level(body: &str) -> Vec<String> {
    if body.trim().is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in body.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => parts.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    parts.push(current);
    parts
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail { got: String },
}

/// Compare a decoded first return value against the caller's declared `expect`.
/// Semantics: `"true"`/`"false"` (bool), `"nonzero"` (any non-zero scalar/
/// address/bytes), anything else = exact match (case-insensitive for hex).
/// `None` = `"true"`.
fn compare(expect: Option<&str>, value: &DynSolValue) -> CheckOutcome {
    let rendered = render(value);
    let pass = match expect.unwrap_or("true") {
        "true" => matches!(value, DynSolValue::Bool(true)),
        "false" => matches!(value, DynSolValue::Bool(false)),
        "nonzero" => match value {
            DynSolValue::Bool(b) => *b,
            DynSolValue::Uint(v, _) => !v.is_zero(),
            DynSolValue::Int(v, _) => !v.is_zero(),
            DynSolValue::Address(a) => *a != Address::ZERO,
            DynSolValue::FixedBytes(b, _) => b.iter().any(|x| *x != 0),
            DynSolValue::Bytes(b) => b.iter().any(|x| *x != 0),
            _ => false,
        },
        exact => rendered.eq_ignore_ascii_case(exact.trim()),
    };
    if pass {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail { got: rendered }
    }
}

fn render(value: &DynSolValue) -> String {
    match value {
        DynSolValue::Bool(b) => b.to_string(),
        DynSolValue::Uint(v, _) => v.to_string(),
        DynSolValue::Int(v, _) => v.to_string(),
        DynSolValue::Address(a) => format!("{a:?}"),
        DynSolValue::FixedBytes(b, n) => format!("0x{}", alloy::hex::encode(&b[..*n])),
        DynSolValue::Bytes(b) => format!("0x{}", alloy::hex::encode(b)),
        DynSolValue::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Run one declared `check` as an eth_call. `Err` = transport-class failure
/// (⇒ `unverified` upstream). A revert is NOT a pass — a check the module
/// cannot answer counts as a failed expectation only when the call returns
/// data; reverts also surface as `Err` here and stay `unverified`, because a
/// wrong-target declaration must not read as "missing config" with a
/// paste-ready fix attached.
pub async fn run_check(
    provider: &RootProvider,
    target: Address,
    fn_sig: &str,
    args: &[String],
    expect: Option<&str>,
) -> Result<CheckOutcome> {
    let f = Function::parse(fn_sig).map_err(|e| {
        Error::Config(format!(
            "declared requirement: bad fn signature `{fn_sig}`: {e}"
        ))
    })?;
    let calldata = encode_call(fn_sig, args)?;
    let req = TransactionRequest::default()
        .to(target)
        .input(calldata.into());
    let ret = provider
        .call(req)
        .await
        .map_err(|e| Error::rpc(format!("declared check `{fn_sig}` on {target:?}: {e}")))?;
    // Callers usually write input-only signatures (`isFeedRouteRegistered(
    // address)`) — no declared outputs to decode against. Use the typed
    // decode when the signature declares returns, else compare the raw
    // return word (bool/uint/address all occupy one 32-byte word).
    if let Ok(decoded) = f.abi_decode_output(&ret)
        && let Some(first) = decoded.first()
    {
        return Ok(compare(expect, first));
    }
    Ok(compare_raw(expect, &ret))
}

/// Raw-return comparison for input-only signatures. `bytes` is the eth_call
/// return data; scalars occupy the first 32-byte word.
fn compare_raw(expect: Option<&str>, bytes: &[u8]) -> CheckOutcome {
    let got = format!("0x{}", alloy::hex::encode(bytes));
    let word = bytes.get(..32);
    let word_nonzero = word.is_some_and(|w| w.iter().any(|b| *b != 0));
    let pass = match expect.unwrap_or("true") {
        "true" => word_nonzero,
        "false" => word.is_some() && !word_nonzero,
        "nonzero" => bytes.iter().any(|b| *b != 0),
        exact => {
            let exact = exact.trim();
            if let Ok(want) = U256::from_str_radix(exact, 10) {
                word.is_some_and(|w| U256::from_be_slice(w) == want)
            } else if let Ok(addr) = exact.parse::<Address>() {
                word.is_some_and(|w| w[12..32] == addr.as_slice()[..])
            } else {
                got.eq_ignore_ascii_case(exact)
            }
        }
    };
    if pass {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail { got }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{primitives::address, sol, sol_types::SolCall};

    /// The exact argument the encoder refused before this unit, measured while
    /// wiring a real module-creation call:
    ///
    /// ```text
    /// ENCODER REFUSED: arg `(0x5aFE…,0,0x00…,0,0,0,0,0)` does not fit `tuple`:
    /// unsupported declared-arg type (address,uint8,bytes32,uint256,…)
    /// ```
    ///
    /// Kept verbatim so the red is preserved rather than described.
    #[test]
    fn the_struct_argument_that_used_to_be_refused() {
        let sig = "createModuleFree((address,uint8,bytes32,uint256,uint256,uint256,uint256,uint256),bytes32,bytes32)";
        let args = vec![
            "(0x5aFE00000000000000000000000000000000A001,0,0x0000000000000000000000000000000000000000000000000000000000000000,0,0,0,0,0)".to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000001".to_string(),
            "0x0000000000000000000000000000000000000000000000000000000000000000".to_string(),
        ];
        encode_call(sig, &args).expect("the tuple argument must now encode");
    }

    /// **The oracle.** Bytes written by hand only prove the encoder agrees with
    /// whoever wrote them; if I mis-order a field in both places the test is
    /// green and the calldata deploys a module nobody controls.
    ///
    /// So the same struct is encoded a second way — through alloy's typed
    /// `sol!` path, which knows the field order from the declaration — and the
    /// two must be byte-identical, selector included.
    #[test]
    fn string_args_encode_exactly_as_alloys_typed_path_does() {
        sol! {
            struct InitParams {
                address safe;
                uint8 initialOperatingMode;
                bytes32 initialAllowedInstrRoot;
                uint256 initialMaxPositionIncreaseLossBps;
                uint256 initialMaxPositionDecreaseLossBps;
                uint256 initialInstrCooldownDuration;
                uint256 initialMaxSwapLossBps;
                uint256 initialSwapCooldownDuration;
            }
            function createModuleFree(InitParams params, bytes32 salt, bytes32 referralKey) external returns (address);
        }

        let safe = address!("5aFE00000000000000000000000000000000A001");
        // Deliberately DISTINCT values per field: an all-zero struct would
        // encode identically under almost any field ordering, so it cannot
        // detect the exact mistake this test exists for.
        let typed = createModuleFreeCall {
            params: InitParams {
                safe,
                initialOperatingMode: 2,
                initialAllowedInstrRoot: alloy::primitives::b256!(
                    "1111111111111111111111111111111111111111111111111111111111111111"
                ),
                initialMaxPositionIncreaseLossBps: U256::from(300),
                initialMaxPositionDecreaseLossBps: U256::from(400),
                initialInstrCooldownDuration: U256::from(500),
                initialMaxSwapLossBps: U256::from(600),
                initialSwapCooldownDuration: U256::from(700),
            },
            salt: alloy::primitives::b256!(
                "2222222222222222222222222222222222222222222222222222222222222222"
            ),
            referralKey: alloy::primitives::b256!(
                "3333333333333333333333333333333333333333333333333333333333333333"
            ),
        }
        .abi_encode();

        let ours = encode_call(
            "createModuleFree((address,uint8,bytes32,uint256,uint256,uint256,uint256,uint256),bytes32,bytes32)",
            &[
                format!("({safe},2,0x1111111111111111111111111111111111111111111111111111111111111111,300,400,500,600,700)"),
                "0x2222222222222222222222222222222222222222222222222222222222222222".to_string(),
                "0x3333333333333333333333333333333333333333333333333333333333333333".to_string(),
            ],
        )
        .expect("encodes");

        assert_eq!(
            alloy::hex::encode(&ours),
            alloy::hex::encode(&typed),
            "the string-arg encoding diverged from alloy's typed encoding of the same struct"
        );
    }

    /// A mis-sized tuple must name the arity rather than encode something
    /// shorter and let the chain decide.
    #[test]
    fn a_tuple_of_the_wrong_length_is_refused_by_count() {
        let err = encode_call(
            "f((address,uint256))",
            &["(0x5aFE00000000000000000000000000000000A001)".into()],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("takes 2 values, got 1"), "{err}");
    }

    /// A bad field must say WHICH field. "does not fit tuple" sends the reader
    /// to re-read all eight.
    #[test]
    fn a_bad_field_names_its_position() {
        let err = encode_call(
            "f((address,uint256))",
            &["(0x5aFE00000000000000000000000000000000A001,notanumber)".into()],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("tuple field 1"), "{err}");
    }

    /// Nested tuples split on the right commas. A naive `split(',')` reports an
    /// arity error about a tuple that was written correctly, which sends the
    /// reader to entirely the wrong place.
    #[test]
    fn a_nested_tuple_is_not_split_by_its_inner_commas() {
        encode_call("f((uint256,(uint256,uint256)))", &["(1,(2,3))".into()])
            .expect("nested tuple encodes");
        assert_eq!(split_top_level("1,(2,3)"), vec!["1", "(2,3)"]);
        assert_eq!(split_top_level(""), Vec::<String>::new());
    }

    /// Without the parentheses there is no tuple, and guessing at one would
    /// mean inventing a grouping the caller never wrote.
    #[test]
    fn an_unparenthesised_tuple_is_refused() {
        let err = encode_call("f((uint256,uint256))", &["1,2".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected a parenthesised tuple"), "{err}");
    }

    sol! {
        function isFeedRouteRegistered(address token) external view returns (bool);
        function setFeedRoute(address token, address feed, uint256 staleness, address fallbackFeed, uint256 fallbackStaleness) external;
    }

    #[test]
    fn encodes_known_selector() {
        // The dynamic encoder must produce byte-identical calldata to the
        // compile-time sol! path a typed caller uses.
        let token = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        let want = isFeedRouteRegisteredCall { token }.abi_encode();
        let got = encode_call("isFeedRouteRegistered(address)", &[format!("{token:?}")]).unwrap();
        assert_eq!(got, want);

        let want = setFeedRouteCall {
            token,
            feed: address!("0000000000000000000000000000000000000fed"),
            staleness: U256::from(3600),
            fallbackFeed: Address::ZERO,
            fallbackStaleness: U256::ZERO,
        }
        .abi_encode();
        let got = encode_call(
            "setFeedRoute(address,address,uint256,address,uint256)",
            &[
                format!("{token:?}"),
                "0x0000000000000000000000000000000000000fed".into(),
                "3600".into(),
                "0x0000000000000000000000000000000000000000".into(),
                "0".into(),
            ],
        )
        .unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn arity_and_type_mismatches_are_config_errors() {
        assert!(encode_call("isFeedRouteRegistered(address)", &[]).is_err());
        assert!(encode_call("isFeedRouteRegistered(address)", &["not-an-address".into()]).is_err());
        assert!(encode_call("notasignature", &[]).is_err());
    }

    #[test]
    fn expect_semantics() {
        let t = |e: Option<&str>, v: DynSolValue| compare(e, &v);
        assert_eq!(t(None, DynSolValue::Bool(true)), CheckOutcome::Pass);
        assert_eq!(
            t(Some("true"), DynSolValue::Bool(false)),
            CheckOutcome::Fail {
                got: "false".into()
            }
        );
        assert_eq!(
            t(Some("false"), DynSolValue::Bool(false)),
            CheckOutcome::Pass
        );
        assert_eq!(
            t(Some("nonzero"), DynSolValue::Uint(U256::from(7), 256)),
            CheckOutcome::Pass
        );
        assert_eq!(
            t(Some("nonzero"), DynSolValue::Address(Address::ZERO)),
            CheckOutcome::Fail {
                got: format!("{:?}", Address::ZERO)
            }
        );
        assert_eq!(
            t(Some("42"), DynSolValue::Uint(U256::from(42), 256)),
            CheckOutcome::Pass
        );
        // Hex compare is case-insensitive (checksummed vs lowercase).
        let a = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
        assert_eq!(
            t(
                Some("0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"),
                DynSolValue::Address(a)
            ),
            CheckOutcome::Pass
        );
    }
}
