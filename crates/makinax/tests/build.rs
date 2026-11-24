//! Call building against a `ModuleConfig`, through the public API only.

use alloy::primitives::{Address, U256};
use makinax::{
    Error,
    build::{CallSpec, SwapOrderArgs, build_call},
    config::ModuleConfig,
    instructions::InstructionLibrary,
};

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

#[test]
fn a_raw_call_is_built_as_given() {
    let to = Address::repeat_byte(0x77);
    let built = build_call(
        &cfg(),
        &InstructionLibrary::empty("none"),
        &CallSpec::Raw {
            to: format!("{to:?}"),
            data: "0xdeadbeef".into(),
            value: Some("5".into()),
        },
    )
    .unwrap();
    assert_eq!(built.to, to);
    assert_eq!(built.data.as_ref(), [0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(built.value, U256::from(5u64));
    assert_eq!(built.label, "raw");
}

#[test]
fn a_swap_on_an_unconfigured_swapper_id_is_refused() {
    let err = build_call(
        &cfg(),
        &InstructionLibrary::empty("none"),
        &CallSpec::Swap {
            quote_id: None,
            order: Some(SwapOrderArgs {
                swapper_id: 9,
                input_token: "0x4200000000000000000000000000000000000006".into(),
                output_token: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                input_amount: "100".into(),
                min_output_amount: "99".into(),
                data: "0x".into(),
            }),
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            Error::SwapperNotConfigured {
                swapper_id: Some(9),
                ..
            }
        ),
        "{err:?}"
    );
}
