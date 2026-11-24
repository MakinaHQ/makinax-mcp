use alloy::primitives::{address, b256};

#[test]
fn set_root_encodes_the_module_selector() {
    let m = address!("0000000000000000000000000000000000000002");
    let call = makinax::owner::encode::set_allowed_instr_root(
        m,
        b256!("00000000000000000000000000000000000000000000000000000000000000aa"),
    );
    assert_eq!(call.to, m);
    assert!(call.sig.starts_with("setAllowedInstrRoot("));
    let generic = makinax::owner::encode::encode_owner_call(m, &call.sig, &call.args).unwrap();
    assert_eq!(generic.sig, call.sig);
}

#[test]
fn unknown_owner_function_is_refused() {
    let m = address!("0000000000000000000000000000000000000002");
    let e = makinax::owner::encode::encode_owner_call(m, "notAFunction(uint256)", &["1".into()])
        .unwrap_err();
    assert!(e.to_string().contains("notAFunction"));
}

/// Every name `owner_function_names()` returns must have a matching
/// `sol!` binding on the actual module interface — and the selector
/// `encode_owner_call` produces for it must equal that binding's OWN
/// `SolCall::SELECTOR`, an independent source computed by the `sol!` macro
/// from `chain::module`'s interface declaration, not from the string under
/// test. Comparing against `keccak256` of the same string being tested (an
/// earlier version of this test) cannot catch a misspelled or
/// type-drifted entry in `owner_function_names()`: it would hash whatever
/// wrong string is there and "agree with itself". Comparing against the
/// compiled `sol!` binding can, because that binding comes from a
/// completely separate declaration (`chain::module.rs`'s interface, typed
/// and checked by the Solidity ABI parser at compile time).
#[test]
fn every_named_owner_function_encodes_through_the_generic_path() {
    use alloy::primitives::{Address, B256, address};
    use alloy::sol_types::SolCall;
    use makinax::chain::module::{IMakinaXModule, IOracleRegistry};

    let m = address!("0000000000000000000000000000000000000002");
    let a = address!("0000000000000000000000000000000000000003");

    // The independent oracle: each signature's selector as the `sol!`
    // macro computed it from `chain::module`'s own interface declaration —
    // never from the string literal being tested here.
    let expected_selector = |sig: &str| -> [u8; 4] {
        match sig {
            "setAllowedInstrRoot(bytes32)" => IMakinaXModule::setAllowedInstrRootCall::SELECTOR,
            "addOperator(address)" => IMakinaXModule::addOperatorCall::SELECTOR,
            "removeOperator(address)" => IMakinaXModule::removeOperatorCall::SELECTOR,
            "addGuardian(address)" => IMakinaXModule::addGuardianCall::SELECTOR,
            "removeGuardian(address)" => IMakinaXModule::removeGuardianCall::SELECTOR,
            "setOperatingMode(uint8)" => IMakinaXModule::setOperatingModeCall::SELECTOR,
            "pause()" => IMakinaXModule::pauseCall::SELECTOR,
            "unpause()" => IMakinaXModule::unpauseCall::SELECTOR,
            "setFeedRoute(address,address,uint256,address,uint256)" => {
                IOracleRegistry::setFeedRouteCall::SELECTOR
            }
            "clearFeedRoute(address)" => IMakinaXModule::clearFeedRouteCall::SELECTOR,
            "setFeedStaleThreshold(address,uint256)" => {
                IMakinaXModule::setFeedStaleThresholdCall::SELECTOR
            }
            "setAccountingCurrency(address)" => IMakinaXModule::setAccountingCurrencyCall::SELECTOR,
            "setMaxPositionIncreaseLossBps(uint256)" => {
                IMakinaXModule::setMaxPositionIncreaseLossBpsCall::SELECTOR
            }
            "setMaxPositionDecreaseLossBps(uint256)" => {
                IMakinaXModule::setMaxPositionDecreaseLossBpsCall::SELECTOR
            }
            "setInstrCooldownDuration(uint256)" => {
                IMakinaXModule::setInstrCooldownDurationCall::SELECTOR
            }
            "setMaxSwapLossBps(uint256)" => IMakinaXModule::setMaxSwapLossBpsCall::SELECTOR,
            "setSwapCooldownDuration(uint256)" => {
                IMakinaXModule::setSwapCooldownDurationCall::SELECTOR
            }
            "setSwapperTargets(uint16,address,address)" => {
                IMakinaXModule::setSwapperTargetsCall::SELECTOR
            }
            "addRecipient(uint256,address)" => IMakinaXModule::addRecipientCall::SELECTOR,
            "removeRecipient(uint256,address)" => IMakinaXModule::removeRecipientCall::SELECTOR,
            "setMaxBridgeLossBps(uint16,uint256)" => {
                IMakinaXModule::setMaxBridgeLossBpsCall::SELECTOR
            }
            "setBridgeCooldownDuration(uint256)" => {
                IMakinaXModule::setBridgeCooldownDurationCall::SELECTOR
            }
            "sweepERC20(address)" => IMakinaXModule::sweepERC20Call::SELECTOR,
            "sweepERC721(address,uint256)" => IMakinaXModule::sweepERC721Call::SELECTOR,
            "sweepERC6909(address,uint256)" => IMakinaXModule::sweepERC6909Call::SELECTOR,
            "sweepNative()" => IMakinaXModule::sweepNativeCall::SELECTOR,
            other => panic!(
                "no sol! binding registered for `{other}` — add one so this test keeps \
                 independently verifying every owner function"
            ),
        }
    };

    // Type-appropriate sample args per signature — arity and types must
    // match exactly, or `encode_owner_call` (via `dyn_call::encode_call`)
    // refuses them before a selector could ever be compared.
    let sample_args = |sig: &str| -> Vec<String> {
        match sig {
            "setAllowedInstrRoot(bytes32)" => vec![format!("{:?}", B256::ZERO)],
            "addOperator(address)"
            | "removeOperator(address)"
            | "addGuardian(address)"
            | "removeGuardian(address)"
            | "clearFeedRoute(address)"
            | "setAccountingCurrency(address)"
            | "sweepERC20(address)" => vec![format!("{a:?}")],
            "setOperatingMode(uint8)" => vec!["1".into()],
            "pause()" | "unpause()" | "sweepNative()" => vec![],
            "setFeedRoute(address,address,uint256,address,uint256)" => vec![
                format!("{a:?}"),
                format!("{a:?}"),
                "3600".into(),
                format!("{:?}", Address::ZERO),
                "0".into(),
            ],
            "setFeedStaleThreshold(address,uint256)" => vec![format!("{a:?}"), "3600".into()],
            "setMaxPositionIncreaseLossBps(uint256)"
            | "setMaxPositionDecreaseLossBps(uint256)"
            | "setInstrCooldownDuration(uint256)"
            | "setMaxSwapLossBps(uint256)"
            | "setSwapCooldownDuration(uint256)"
            | "setBridgeCooldownDuration(uint256)" => vec!["100".into()],
            "setSwapperTargets(uint16,address,address)" => {
                vec!["1".into(), format!("{a:?}"), format!("{a:?}")]
            }
            "addRecipient(uint256,address)" | "removeRecipient(uint256,address)" => {
                vec!["8453".into(), format!("{a:?}")]
            }
            "setMaxBridgeLossBps(uint16,uint256)" => vec!["1".into(), "100".into()],
            "sweepERC721(address,uint256)" | "sweepERC6909(address,uint256)" => {
                vec![format!("{a:?}"), "1".into()]
            }
            other => panic!(
                "no sample args registered for `{other}` — add one so this test keeps \
                 covering every owner function"
            ),
        }
    };

    for sig in makinax::owner::encode::owner_function_names() {
        assert!(sig.contains('('), "{sig}");
        let args = sample_args(sig);

        let call = makinax::owner::encode::encode_owner_call(m, sig, &args).unwrap_or_else(|e| {
            panic!("`{sig}` refused its own type-appropriate sample args: {e}")
        });
        assert_eq!(&call.sig, sig);

        // Re-derive the calldata through the same dyn_call path
        // `build_batch`/`single_call` use, and check the selector against
        // the sol! macro's own compiled binding for this function — a
        // misspelled or type-drifted entry in owner_function_names() would
        // either fail to compile above (no matching sol! binding to name)
        // or produce a selector mismatch here.
        let data = makinax::owner::dyn_call::encode_call(sig, &args)
            .unwrap_or_else(|e| panic!("`{sig}`: dyn_call::encode_call failed: {e}"));
        assert_eq!(
            &data[..4],
            expected_selector(sig).as_slice(),
            "{sig}: selector does not match the sol! binding"
        );
    }
}
