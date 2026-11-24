//! `read_module_state` against a mocked RPC whose answers are encoded with
//! the real `sol!` return types, so a binding that drifts from the contract's
//! ABI fails to decode here rather than on a live module.
//!
//! The mock answers in request order; the order below is the documented read
//! order of `read_module_state`.

use std::collections::BTreeMap;

use alloy::{
    hex,
    primitives::{Address, B256, U256},
    providers::{ProviderBuilder, mock::Asserter},
    sol_types::SolCall,
};
use makinax::{
    Error,
    chain::module::IMakinaXModule as M,
    config::ModuleConfig,
    reads::{
        module_state::{ModuleState, read_module_state},
        oracle::{FeedRoute, read_feed_routes},
    },
};

const SAFE: Address = Address::with_last_byte(0x0a);
const GUARDIAN: Address = Address::with_last_byte(0x0c);
const OPERATOR: Address = Address::with_last_byte(0x0b);
const MODULE: Address = Address::with_last_byte(0x0d);
const SERVICE: Address = Address::with_last_byte(0x0e);
const REGISTRY: Address = Address::with_last_byte(0x0f);
const VM: Address = Address::with_last_byte(0x10);
const USDC: Address = Address::with_last_byte(0x11);
const ROUTER: Address = Address::with_last_byte(0x12);

fn cfg() -> ModuleConfig {
    ModuleConfig {
        chain: "base".into(),
        safe: SAFE,
        module: MODULE,
        rootfile: None,
        operator_address: Some(OPERATOR),
        guardian_address: Some(GUARDIAN),
        swappers: BTreeMap::from([("kyber".to_string(), 1u16), ("odos".to_string(), 2u16)]),
    }
}

fn ret<C: SolCall>(value: &C::Return) -> String {
    hex::encode_prefixed(C::abi_encode_returns(value))
}

fn u(v: u64) -> U256 {
    U256::from(v)
}

/// Every answer `read_module_state` asks for, in its read order.
fn answers() -> Vec<String> {
    let root = B256::repeat_byte(0xab);
    let mut a = vec![
        "0x64".to_string(), // eth_blockNumber = 100
        ret::<M::safeCall>(&SAFE),
        ret::<M::providerCall>(&SERVICE),
        ret::<M::registryCall>(&REGISTRY),
        ret::<M::weirollVmCall>(&VM),
        ret::<M::allowedInstrRootCall>(&root),
        ret::<M::operatingModeCall>(&1u8),
        ret::<M::pausedCall>(&false),
        ret::<M::suspendedByProviderCall>(&true),
        ret::<M::accountingCurrencyCall>(&USDC),
        ret::<M::maxPositionIncreaseLossBpsCall>(&u(30)),
        ret::<M::maxPositionDecreaseLossBpsCall>(&u(40)),
        ret::<M::maxSwapLossBpsCall>(&u(50)),
        ret::<M::getMaxBridgeLossBpsCall>(&u(60)), // bridge 1
        ret::<M::getMaxBridgeLossBpsCall>(&u(0)),  // bridge 2
        ret::<M::getMaxBridgeLossBpsCall>(&u(70)), // bridge 3
        ret::<M::instrCooldownDurationCall>(&u(600)),
        ret::<M::swapCooldownDurationCall>(&u(300)),
        ret::<M::bridgeCooldownDurationCall>(&u(3600)),
        ret::<M::swapFeeRateCall>(&u(1_000_000_000_000_000)),
        ret::<M::isOperatorCall>(&true),
        // guardian candidates, sorted: the Safe, then the configured guardian
        ret::<M::isGuardianCall>(&true),
        ret::<M::isGuardianCall>(&false),
        // configured swappers, by name: kyber (1), odos (2)
        ret::<M::getSwapperTargetsCall>(&M::getSwapperTargetsReturn {
            approvalTarget: ROUTER,
            executionTarget: ROUTER,
        }),
        ret::<M::getSwapperTargetsCall>(&M::getSwapperTargetsReturn {
            approvalTarget: Address::ZERO,
            executionTarget: Address::ZERO,
        }),
    ];
    // recipients: the Safe on every known chain except the module's own
    // (base): mainnet, optimism, hyperevm, robinhood.
    a.push(ret::<M::isWhitelistedRecipientCall>(&true));
    a.push(ret::<M::isWhitelistedRecipientCall>(&false));
    a.push(ret::<M::isWhitelistedRecipientCall>(&true));
    a.push(ret::<M::isWhitelistedRecipientCall>(&false));
    a
}

async fn read(answers: &[String], fail_at: Option<usize>) -> makinax::Result<ModuleState> {
    read_with(cfg(), answers, fail_at).await
}

async fn read_with(
    cfg: ModuleConfig,
    answers: &[String],
    fail_at: Option<usize>,
) -> makinax::Result<ModuleState> {
    let mock = Asserter::new();
    for (i, answer) in answers.iter().enumerate() {
        if Some(i) == fail_at {
            mock.push_failure_msg("execution reverted");
            break;
        }
        mock.push_success(answer);
    }
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    read_module_state(&provider, &cfg).await
}

#[tokio::test]
async fn every_field_decodes_from_the_real_return_types() {
    let s = read(&answers(), None).await.expect("state reads");
    assert_eq!(s.module, MODULE);
    assert_eq!(s.safe, SAFE);
    assert!(s.safe_matches_config);
    assert_eq!(s.block, 100);
    assert_eq!(s.service_provider, SERVICE);
    assert_eq!(s.registry, REGISTRY);
    assert_eq!(s.weiroll_vm, VM);
    assert_eq!(s.allowed_instr_root, B256::repeat_byte(0xab));
    assert_eq!(s.operating_mode, "FENCED");
    assert!(!s.paused);
    assert!(s.suspended_by_provider);
    assert_eq!(s.accounting_currency, Some(USDC));
    assert_eq!(
        s.loss_caps_bps,
        BTreeMap::from([
            ("position_increase".to_string(), 30),
            ("position_decrease".to_string(), 40),
            ("swap".to_string(), 50),
            ("bridge:1".to_string(), 60),
            ("bridge:2".to_string(), 0),
            ("bridge:3".to_string(), 70),
        ])
    );
    assert_eq!(
        s.cooldowns_s,
        BTreeMap::from([
            ("instruction".to_string(), 600),
            ("swap".to_string(), 300),
            ("bridge".to_string(), 3600),
        ])
    );
    assert_eq!(s.swap_fee_rate, "1000000000000000");
    assert_eq!(s.operators, vec![OPERATOR]);
    assert_eq!(s.guardians, vec![SAFE]);
    assert_eq!(s.swappers.len(), 2);
    assert_eq!(s.swappers[0].name, "kyber");
    assert_eq!(s.swappers[0].swapper_id, 1);
    assert_eq!(s.swappers[0].approval_target, ROUTER);
    assert_eq!(s.swappers[0].execution_target, ROUTER);
    assert_eq!(s.swappers[1].name, "odos");
    assert_eq!(s.swappers[1].execution_target, Address::ZERO);
    let recipients: Vec<(u64, String)> = s
        .recipients
        .iter()
        .map(|r| {
            (
                r["chain_id"].as_u64().unwrap(),
                r["recipient"].as_str().unwrap().to_lowercase(),
            )
        })
        .collect();
    let safe = format!("{SAFE:?}").to_lowercase();
    assert_eq!(recipients, vec![(1, safe.clone()), (999, safe)]);
    // Serializes without loss of the fields a consumer shows.
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["operating_mode"], "FENCED");
    assert_eq!(json["loss_caps_bps"]["bridge:3"], 70);
}

#[tokio::test]
async fn a_zero_accounting_currency_is_absent_not_an_address() {
    let mut a = answers();
    a[9] = ret::<M::accountingCurrencyCall>(&Address::ZERO);
    let s = read(&a, None).await.unwrap();
    assert_eq!(s.accounting_currency, None);
}

#[tokio::test]
async fn an_onchain_safe_that_differs_from_config_is_reported_and_both_are_checked() {
    let other = Address::with_last_byte(0x09);
    let mut a = answers();
    a[1] = ret::<M::safeCall>(&other);
    // Guardian candidates are now {0x09, 0x0a (config safe), 0x0c}: one more
    // isGuardian answer. Recipient candidates double to 8.
    a.insert(21, ret::<M::isGuardianCall>(&true));
    for _ in 0..4 {
        a.push(ret::<M::isWhitelistedRecipientCall>(&false));
    }
    let s = read(&a, None).await.unwrap();
    assert_eq!(s.safe, other);
    assert!(!s.safe_matches_config);
    assert_eq!(s.guardians, vec![other, SAFE]);
}

#[tokio::test]
async fn any_failed_read_is_an_error_naming_the_call_never_a_default() {
    let expected = [
        (0, "block number"),
        (1, "safe()"),
        (6, "operatingMode()"),
        (13, "getMaxBridgeLossBps(1)"),
        (18, "bridgeCooldownDuration()"),
        (20, "isOperator("),
        (21, "isGuardian("),
        (23, "getSwapperTargets(1)"),
        (25, "isWhitelistedRecipient(1, "),
    ];
    for (at, call) in expected {
        match read(&answers(), Some(at)).await {
            Err(Error::Rpc(m)) => assert!(m.starts_with(call), "at {at}: {m}"),
            other => panic!("at {at}: expected an Rpc error naming {call}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_bps_value_too_large_for_u64_is_refused_not_saturated() {
    let mut a = answers();
    a[12] = ret::<M::maxSwapLossBpsCall>(&U256::MAX);
    match read(&a, None).await {
        Err(Error::Rpc(m)) => assert!(m.starts_with("maxSwapLossBps()"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn feed_routes_are_read_for_the_named_tokens_only() {
    use makinax::chain::module::IOracleRegistry as O;
    let feed1 = Address::with_last_byte(0x21);
    let feed2 = Address::with_last_byte(0x22);
    let unregistered = Address::with_last_byte(0x23);
    let mock = Asserter::new();
    for answer in [
        ret::<O::isFeedRouteRegisteredCall>(&true),
        ret::<O::getFeedRouteCall>(&O::getFeedRouteReturn { feed1, feed2 }),
        ret::<O::getFeedStaleThresholdCall>(&u(86_400)),
        ret::<O::getFeedStaleThresholdCall>(&u(3_600)),
        ret::<O::isFeedRouteRegisteredCall>(&false),
    ] {
        mock.push_success(&answer);
    }
    let provider = ProviderBuilder::new().connect_mocked_client(mock);
    let routes = read_feed_routes(&provider, MODULE, &[USDC, unregistered], 100)
        .await
        .unwrap();
    assert_eq!(
        routes,
        vec![FeedRoute {
            token: USDC,
            feed1,
            feed2,
            stale_threshold1_s: 86_400,
            stale_threshold2_s: Some(3_600),
        }]
    );
}

#[tokio::test]
async fn a_chain_name_this_crate_does_not_know_is_resolved_by_the_rpc() {
    let mut cfg = cfg();
    cfg.chain = "a-chain-not-in-known".into();
    let mut a = answers();
    // eth_chainId (Base) comes after the swapper reads, before recipients;
    // Base is then skipped exactly as when the config names it.
    a.insert(25, "0x2105".into());
    let s = read_with(cfg.clone(), &a, None).await.unwrap();
    let chains: Vec<u64> = s
        .recipients
        .iter()
        .map(|r| r["chain_id"].as_u64().unwrap())
        .collect();
    assert_eq!(chains, vec![1, 999]);
    match read_with(cfg, &a, Some(25)).await {
        Err(Error::Rpc(m)) => assert!(m.starts_with("chain id"), "{m}"),
        other => panic!("expected an Rpc error naming the chain id read, got {other:?}"),
    }
}
