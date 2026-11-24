//! Fetched data is checked against what was asked for before anything is
//! encoded: a quote or a bridge order for another target, recipient, token,
//! amount or chain is refused, and the refusal names the field with its
//! expected and actual values. Public API only.

use alloy::primitives::{Address, U256};
use makinax::{
    Error,
    build::{BridgeOrderArgs, build_bridge},
    config::ModuleConfig,
    fetch::validate::{BridgeExpectation, check_field, validate_bridge_order},
    instructions::InstructionLibrary,
};

const WETH_BASE: &str = "0x4200000000000000000000000000000000000006";
const WETH_MAINNET: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const RECIPIENT: Address = Address::repeat_byte(0x22);
const OTHER: Address = Address::repeat_byte(0xee);

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

/// The refusal is `FetchRefused`, and its text names the field and both
/// values.
fn refused(e: Error, field: &str, expected: &str, actual: &str) {
    let text = e.to_string();
    assert!(matches!(e, Error::FetchRefused(_)), "{e:?}");
    assert!(text.contains(&format!("{field}: expected ")), "{text}");
    assert!(text.contains(expected), "expected value missing: {text}");
    assert!(text.contains(actual), "actual value missing: {text}");
}

fn order(recipient: Option<Address>) -> BridgeOrderArgs {
    BridgeOrderArgs {
        bridge_id: 1,
        dest_chain: "mainnet".into(),
        recipient: recipient.map(|r| format!("{r:?}")),
        input_token: WETH_BASE.into(),
        output_token: WETH_MAINNET.into(),
        input_amount: "1000000000000000".into(),
        min_output_amount: "990000000000000".into(),
        fill_deadline_offset_s: None,
        cctp_finality: None,
    }
}

fn bridge_want() -> BridgeExpectation {
    BridgeExpectation {
        recipient: RECIPIENT,
        input_token: WETH_BASE.parse().unwrap(),
        output_token: WETH_MAINNET.parse().unwrap(),
        input_amount: U256::from(1_000_000_000_000_000u64),
        dest_chain_id: 1,
        bridge_id: 1,
    }
}

#[test]
fn bridge_order_to_another_recipient_is_refused() {
    let e = validate_bridge_order(&order(Some(OTHER)), &bridge_want()).unwrap_err();
    refused(e, "recipient", &RECIPIENT.to_string(), &OTHER.to_string());
}

#[test]
fn a_matching_bridge_order_passes() {
    validate_bridge_order(&order(Some(RECIPIENT)), &bridge_want()).unwrap();
}

#[test]
fn bridge_order_with_no_recipient_is_refused_by_the_validator() {
    let e = validate_bridge_order(&order(None), &bridge_want()).unwrap_err();
    refused(e, "recipient", &RECIPIENT.to_string(), "none");
}

#[test]
fn bridge_order_for_another_token_amount_or_chain_is_refused() {
    let mut o = order(Some(RECIPIENT));
    o.input_token = WETH_MAINNET.into();
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(
        e,
        "input_token",
        "0x4200000000000000000000000000000000000006",
        WETH_MAINNET,
    );

    let mut o = order(Some(RECIPIENT));
    o.input_amount = "1000000000000001".into();
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(e, "input_amount", "1000000000000000", "1000000000000001");

    let mut o = order(Some(RECIPIENT));
    o.dest_chain = "base".into();
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(e, "dest_chain_id", "1", "8453");

    let mut o = order(Some(RECIPIENT));
    o.output_token = WETH_BASE.into();
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(e, "output_token", WETH_MAINNET, WETH_BASE);

    // CCTP's id for an Across expectation: another bridge, refused.
    let mut o = order(Some(RECIPIENT));
    o.bridge_id = 3;
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(e, "bridge_id", "expected 1", "got 3");

    // The floor is the caller's and is not compared.
    let mut o = order(Some(RECIPIENT));
    o.min_output_amount = "1".into();
    validate_bridge_order(&o, &bridge_want()).unwrap();

    let mut o = order(Some(RECIPIENT));
    o.dest_chain = "nowhere".into();
    let e = validate_bridge_order(&o, &bridge_want()).unwrap_err();
    refused(e, "dest_chain_id", "1", "nowhere");
}

/// `build_bridge` defaults an unset recipient to the module's Safe, then
/// validates: it builds when the Safe is what was asked for, and is refused
/// before encoding when another recipient was.
#[test]
fn build_bridge_defaults_the_recipient_to_the_safe_then_validates() {
    let cfg = cfg();
    let lib = InstructionLibrary::empty("test");
    let want_safe = BridgeExpectation {
        recipient: cfg.safe,
        ..bridge_want()
    };
    let built = build_bridge(&cfg, &lib, &order(None), &want_safe, None).unwrap();
    assert_eq!(built.label, "bridge");
    assert!(built.summary_json.contains(&format!("{:?}", cfg.safe)));

    let e = build_bridge(&cfg, &lib, &order(None), &bridge_want(), None).unwrap_err();
    refused(
        e,
        "recipient",
        &RECIPIENT.to_string(),
        &cfg.safe.to_string(),
    );

    let e = build_bridge(&cfg, &lib, &order(Some(OTHER)), &bridge_want(), None).unwrap_err();
    refused(e, "recipient", &RECIPIENT.to_string(), &OTHER.to_string());
}

#[cfg(feature = "quote")]
mod swap {
    use super::*;
    use alloy::primitives::Bytes;
    use makinax::{
        build::{build_swap_from_quote, swap_order_from_quote},
        chain::swappers::KYBER_ROUTER,
        fetch::{
            quote::Quote,
            validate::{SwapExpectation, validate_swap_quote, validate_swap_quote_with_spenders},
        },
    };

    const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";

    fn quote_with_defaults() -> Quote {
        Quote {
            adapter: "kyber",
            input_token: WETH_BASE.parse().unwrap(),
            output_token: USDC_BASE.parse().unwrap(),
            input_amount: U256::from(100u64),
            expected_out: U256::from(1_000u64),
            min_out: U256::from(995u64),
            data: Bytes::from(vec![0xab; 4]),
            execution_target: KYBER_ROUTER,
            approval_target: KYBER_ROUTER,
            price_impact_pct: None,
            valid_hint_s: 30,
            slippage_bps: 50,
        }
    }

    fn quote_with(f: impl FnOnce(&mut Quote)) -> Quote {
        let mut q = quote_with_defaults();
        f(&mut q);
        q
    }

    fn want() -> SwapExpectation {
        SwapExpectation {
            module: cfg().module,
            swapper_target: KYBER_ROUTER,
            input_token: WETH_BASE.parse().unwrap(),
            output_token: USDC_BASE.parse().unwrap(),
            input_amount: U256::from(100u64),
        }
    }

    #[test]
    fn swap_quote_for_another_target_is_refused() {
        let e =
            validate_swap_quote(&quote_with(|q| q.execution_target = OTHER), &want()).unwrap_err();
        refused(
            e,
            "execution_target",
            &KYBER_ROUTER.to_string(),
            &OTHER.to_string(),
        );
    }

    #[test]
    fn swap_quote_for_another_token_is_refused() {
        let e = validate_swap_quote(&quote_with(|q| q.output_token = OTHER), &want()).unwrap_err();
        refused(e, "output_token", USDC_BASE, &OTHER.to_string());
        let e = validate_swap_quote(&quote_with(|q| q.input_token = OTHER), &want()).unwrap_err();
        refused(
            e,
            "input_token",
            "0x4200000000000000000000000000000000000006",
            &OTHER.to_string(),
        );
    }

    #[test]
    fn swap_quote_for_another_amount_is_refused() {
        let e = validate_swap_quote(
            &quote_with(|q| q.input_amount = U256::from(101u64)),
            &want(),
        )
        .unwrap_err();
        refused(e, "input_amount", "100", "101");
    }

    #[test]
    fn a_matching_quote_passes() {
        validate_swap_quote(&quote_with_defaults(), &want()).unwrap();
    }

    /// The approval target is the router, or a spender the CALLER names
    /// (e.g. the approval target the module registered for this swapper id).
    /// The library itself knows no other spender.
    #[test]
    fn approval_target_is_the_router_or_a_caller_named_spender() {
        let q = quote_with(|q| q.approval_target = OTHER);
        let e = validate_swap_quote(&q, &want()).unwrap_err();
        refused(
            e,
            "approval_target",
            &KYBER_ROUTER.to_string(),
            &OTHER.to_string(),
        );
        validate_swap_quote_with_spenders(&q, &want(), &[OTHER]).unwrap();
        let e =
            validate_swap_quote_with_spenders(&q, &want(), &[Address::repeat_byte(7)]).unwrap_err();
        refused(
            e,
            "approval_target",
            &Address::repeat_byte(7).to_string(),
            &OTHER.to_string(),
        );
    }

    /// A router the library can label belongs to that adapter: a quote
    /// through Kyber's router that calls itself another adapter is refused.
    /// A router it cannot label (Odos deploys one per chain) is not guessed.
    #[test]
    fn a_labelled_router_must_match_the_quotes_adapter() {
        let e = validate_swap_quote(&quote_with(|q| q.adapter = "odos"), &want()).unwrap_err();
        refused(e, "adapter", "kyber", "odos");

        let odos_router = Address::repeat_byte(0xcf);
        let q = quote_with(|q| {
            q.adapter = "odos";
            q.execution_target = odos_router;
            q.approval_target = odos_router;
        });
        let w = SwapExpectation {
            swapper_target: odos_router,
            ..want()
        };
        validate_swap_quote(&q, &w).unwrap();
    }

    #[test]
    fn swap_order_from_quote_validates_then_materializes() {
        let order = swap_order_from_quote(&quote_with_defaults(), 1, &want()).unwrap();
        assert_eq!(order.swapper_id, 1);
        assert_eq!(order.input_amount, "100");
        assert_eq!(
            order.min_output_amount, "995",
            "min-out is the quote's, from the request's slippage"
        );
        assert_eq!(order.data, "0xabababab");
        let e =
            swap_order_from_quote(&quote_with(|q| q.output_token = OTHER), 1, &want()).unwrap_err();
        refused(e, "output_token", USDC_BASE, &OTHER.to_string());
    }

    #[test]
    fn build_swap_from_quote_refuses_before_encoding() {
        let cfg = cfg();
        let lib = InstructionLibrary::empty("test");
        let built = build_swap_from_quote(&cfg, &lib, &quote_with_defaults(), 1, &want()).unwrap();
        assert_eq!(built.label, "swap");
        assert_eq!(built.to, cfg.module);

        let e = build_swap_from_quote(
            &cfg,
            &lib,
            &quote_with(|q| q.execution_target = OTHER),
            1,
            &want(),
        )
        .unwrap_err();
        refused(
            e,
            "execution_target",
            &KYBER_ROUTER.to_string(),
            &OTHER.to_string(),
        );

        // An expectation formed for another module is not this call's.
        let other_module = SwapExpectation {
            module: OTHER,
            ..want()
        };
        let e = build_swap_from_quote(&cfg, &lib, &quote_with_defaults(), 1, &other_module)
            .unwrap_err();
        refused(e, "module", &cfg.module.to_string(), &OTHER.to_string());

        // The swapper id must be the one config maps to the quote's adapter.
        let e = build_swap_from_quote(&cfg, &lib, &quote_with_defaults(), 7, &want()).unwrap_err();
        refused(e, "swapper_id", "expected 1", "got 7");
        let mut unmapped = cfg.clone();
        unmapped.swappers = [("odos".to_string(), 1u16)].into();
        let e =
            build_swap_from_quote(&unmapped, &lib, &quote_with_defaults(), 1, &want()).unwrap_err();
        refused(e, "swapper_id", "adapter `kyber` (none is mapped)", "got 1");
    }
}

/// The one-field check every validator is built from: equal values pass,
/// and a difference is refused naming the field and both values.
#[test]
fn check_field_passes_equal_values_and_names_a_difference() {
    assert!(check_field("amount", U256::from(7u64), U256::from(7u64)).is_ok());
    assert!(check_field("recipient", RECIPIENT, RECIPIENT).is_ok());
    refused(
        check_field("amount", U256::from(7u64), U256::from(8u64)).unwrap_err(),
        "amount",
        "7",
        "8",
    );
    refused(
        check_field("recipient", RECIPIENT, OTHER).unwrap_err(),
        "recipient",
        &format!("{RECIPIENT}"),
        &format!("{OTHER}"),
    );
}
