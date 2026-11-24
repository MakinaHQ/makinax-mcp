//! The makinax walk: one module, end to end, on an anvil fork of Base,
//! through `makinax::ext` alone.
//!
//! Setup (test scaffolding, not makinax): fork Base at a pinned block, create
//! a 1-of-1 Safe with the canonical Safe factory, create a module for it
//! through the Makina module factory (`createModuleFree`) with the fixture
//! rootfile's root, then, as the Safe, enable the module and add an operator.
//!
//! The walk:
//! 1. `read_module_state`, and `check_root` agrees with the fixture rootfile;
//! 2. `build_call` for the fixture's management action, carrying its proof;
//! 3. `simulate` on the Fork backend: success;
//! 4. `Executor::new(AllowAll)` then `send`: the receipt succeeds;
//! 5. `encode_owner_call("setAllowedInstrRoot(bytes32)")` then `build_batch`,
//!    applied as the Safe with `SafeForkSession`: the module reports the new
//!    root;
//! 6. a CCTP `Bridge` spec is built and its calldata decoded back;
//! 7. `read_module_events` over the walk's blocks returns the events of steps
//!    4 and 5.
//!
//! Needs anvil (foundry) and a Base RPC that serves state at the pinned
//! block:
//! - `MAKINAX_FORK_RPC`: the fork source. Default: `https://mainnet.base.org`
//!   (public; serves historical state).
//! - `MAKINAX_FORK_BLOCK`: the block to fork at, a decimal block number.
//!   Default: [`FORK_BLOCK`]. A value that does not parse is a failure.
//!
//! The fork is stock anvil (foundry), not Base Anvil: the walk touches WETH,
//! a Safe, the module and the core registry, and no native B20 token, so
//! Ethereum execution rules answer it faithfully. The simulation in step 3
//! attaches to this fork (`dev_rpc_urls`) rather than spawning its own.
//!
//! When the fork source cannot be reached, the walk says so and skips on a
//! developer machine. Under CI (`CI` set) it fails instead, naming the last
//! error: a walk that did not run must not read green. A missing anvil is
//! always a failure. CI runs it in `findings-suite`:
//!   cargo test -p makinax --test fork_walk -- --ignored
#![cfg(feature = "write")]

use std::{sync::Arc, time::Duration};

use alloy::{
    network::EthereumWallet,
    node_bindings::{Anvil, AnvilInstance},
    primitives::{Address, B256, Bytes, U256, address, keccak256},
    providers::{Provider, ProviderBuilder},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{SolCall, SolEvent, SolValue},
};
use makinax::ext::{
    AllowAll, BridgeOrderArgs, CallSpec, CctpCapabilities, CctpDeployment, CctpFinality, CctpRoute,
    Error, EventsConfig, ExecTarget, Executor, ForkConfig, ForkManager, InstructionLibrary,
    MakinaxConfig, ModuleConfig, Providers, RootfileSource, SafeForkSession, SendOptions,
    SimBackend, SimReceiptStore,
    bindings::{IBridgeEncoderRegistry, IMakinaXModule, ISafe},
    build_batch, build_call, build_call_with_cctp, chain_id_for, check_root, encode_owner_call,
    owner_calls, proof_for, provider_from_endpoints, read_module_events, read_module_state,
    simulate,
};

/// The default fork source: Base's public endpoint, which serves historical
/// state (the pinned block is not the head).
const DEFAULT_FORK_RPC: &str = "https://mainnet.base.org";
/// The pinned Base block (2026-09-25). The module factory has free
/// deployment on at this block.
const FORK_BLOCK: u64 = 51_790_000;
const BASE: &str = "base";

/// The Makina module factory on Base (`ModuleFactory`, a proxy).
const MODULE_FACTORY: Address = address!("6d4131Ca2c0417D29760D2e860dE62D432C3F5eB");
/// Canonical Safe 1.4.1 deployments (same address on every chain).
const SAFE_PROXY_FACTORY: Address = address!("4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67");
const SAFE_L2_SINGLETON: Address = address!("29fcB43b46531BcA003ddC8FCB67FFE91900C762");

/// Circle CCTP v2 facts, the same on Base and Ethereum mainnet.
const TOKEN_MESSENGER_V2: Address = address!("28b5a0e9C621a5BaDaA536219b3a228C8168cf5d");
const MESSAGE_TRANSMITTER_V2: Address = address!("81D40F21F12A8F0E3252Bccb954D722d4c464B64");
const USDC_BASE: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
const USDC_MAINNET: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");

/// The fixture's one management action.
const ACTION: &str = "weth.approve.Zero allowance";

sol! {
    #[sol(rpc)]
    interface ISafeProxyFactory {
        function createProxyWithNonce(address singleton, bytes initializer, uint256 saltNonce)
            external returns (address proxy);
    }

    function setup(
        address[] owners,
        uint256 threshold,
        address to,
        bytes data,
        address fallbackHandler,
        address paymentToken,
        uint256 payment,
        address paymentReceiver
    );

    /// `IMakinaXModule.MakinaXModuleInitParams`, field order as the contract.
    struct ModuleInitParams {
        address safe;
        uint8 initialOperatingMode;
        bytes32 initialAllowedInstrRoot;
        uint256 initialMaxPositionIncreaseLossBps;
        uint256 initialMaxPositionDecreaseLossBps;
        uint256 initialInstrCooldownDuration;
        uint256 initialMaxSwapLossBps;
        uint256 initialSwapCooldownDuration;
    }

    #[sol(rpc)]
    interface IModuleFactory {
        function freeDeployment() external view returns (bool);
        function createModuleFree(ModuleInitParams params, bytes32 salt, bytes32 referralKey)
            external returns (address);
    }
}

fn fixture(name: &str) -> RootfileSource {
    RootfileSource::Local(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
}

/// The fork source, or `None` (with the reason printed) when it cannot be
/// reached. Under CI an unreachable source is a failure, not a skip.
async fn reachable_fork_source() -> Option<String> {
    let url = std::env::var("MAKINAX_FORK_RPC")
        .ok()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| DEFAULT_FORK_RPC.to_string());
    let provider = provider_from_endpoints(std::slice::from_ref(&url), "fork source")
        .unwrap_or_else(|e| panic!("MAKINAX_FORK_RPC `{url}` is not usable: {e}"));
    let mut last = String::new();
    for attempt in 0..4u64 {
        match tokio::time::timeout(Duration::from_secs(20), provider.get_chain_id()).await {
            Ok(Ok(8453)) => return Some(url),
            Ok(Ok(other)) => panic!("MAKINAX_FORK_RPC serves chain {other}, not Base (8453)"),
            Ok(Err(e)) => last = e.to_string(),
            Err(_) => last = "no answer within 20s".into(),
        }
        tokio::time::sleep(Duration::from_secs(2 << attempt)).await;
    }
    let block = fork_block();
    if std::env::var_os("CI").is_some() {
        panic!(
            "the fork source {url} is unreachable ({last}); under CI the walk fails rather than \
             skipping. Set MAKINAX_FORK_RPC to a Base RPC that serves state at block {block}."
        );
    }
    eprintln!(
        "SKIPPED: the fork source {url} is unreachable ({last}). Set MAKINAX_FORK_RPC to a Base \
         RPC that serves state at block {block}, then run again."
    );
    None
}

/// `MAKINAX_FORK_BLOCK`, or [`FORK_BLOCK`] when unset or empty. A value that
/// is not a block number fails the walk rather than being ignored.
fn fork_block() -> u64 {
    match std::env::var("MAKINAX_FORK_BLOCK") {
        Ok(b) if !b.trim().is_empty() => b
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("MAKINAX_FORK_BLOCK `{b}` is not a block number: {e}")),
        _ => FORK_BLOCK,
    }
}

/// Anvil forking Base, retried with backoff: a public source may refuse the
/// burst of state reads a fork starts with.
fn spawn_fork(source: &str, block: u64) -> AnvilInstance {
    let mut last = String::new();
    for attempt in 0..4u64 {
        match Anvil::new()
            .fork(source)
            .fork_block_number(block)
            .args([
                "--no-storage-caching".to_string(),
                "--hardfork".to_string(),
                ForkConfig::default_hardfork(),
                "--retries".into(),
                "10".into(),
                "--fork-retry-backoff".into(),
                "2000".into(),
            ])
            .timeout(120_000)
            .try_spawn()
        {
            Ok(anvil) => return anvil,
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(Duration::from_secs(5 << attempt));
    }
    panic!("anvil could not fork {source} at block {block}: {last}");
}

#[tokio::test]
#[ignore = "needs anvil and a Base RPC; CI runs it with --ignored in findings-suite"]
async fn a_module_walk_on_a_base_fork() {
    // Read first, so a bad value fails before any network wait.
    let block = fork_block();
    let Some(source) = reachable_fork_source().await else {
        return;
    };
    let anvil = spawn_fork(&source, block);
    let url = anvil.endpoint();
    println!("anvil fork of Base at block {block}: {url}");

    // A test-only reader over the fork (scaffolding and cross-checks).
    let reader = ProviderBuilder::new()
        .connect_http(anvil.endpoint_url())
        .erased();
    let deployer = anvil.addresses()[0];
    let operator_key = PrivateKeySigner::from(anvil.keys()[1].clone());
    let operator = operator_key.address();
    let deployer_signer = PrivateKeySigner::from(anvil.keys()[0].clone());
    let deployer_rpc = ProviderBuilder::new()
        .wallet(EthereumWallet::from(deployer_signer))
        .connect_http(anvil.endpoint_url())
        .erased();

    let lib = InstructionLibrary::load(&fixture("fork_walk_rootfile.toml"))
        .await
        .expect("the walk fixture loads");
    let other_lib = InstructionLibrary::load(&fixture("rootfile.toml"))
        .await
        .expect("the proofs fixture loads");
    assert_ne!(lib.root(), other_lib.root());

    // ── Setup: a Safe, a module for it, the module enabled, an operator. ──
    let initializer = setupCall {
        owners: vec![deployer],
        threshold: U256::from(1),
        to: Address::ZERO,
        data: Bytes::new(),
        fallbackHandler: Address::ZERO,
        paymentToken: Address::ZERO,
        payment: U256::ZERO,
        paymentReceiver: Address::ZERO,
    }
    .abi_encode();
    let safe_factory = ISafeProxyFactory::new(SAFE_PROXY_FACTORY, &deployer_rpc);
    let create_safe =
        safe_factory.createProxyWithNonce(SAFE_L2_SINGLETON, initializer.into(), U256::from(1));
    let safe = create_safe.call().await.expect("predict the Safe");
    let r = create_safe
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(r.status(), "Safe creation reverted");

    let factory = IModuleFactory::new(MODULE_FACTORY, &deployer_rpc);
    assert!(
        factory.freeDeployment().call().await.unwrap(),
        "free deployment is off at block {block}; pin a block where it is on"
    );
    let create_module = factory.createModuleFree(
        ModuleInitParams {
            safe,
            initialOperatingMode: 0, // OPEN
            initialAllowedInstrRoot: lib.root(),
            initialMaxPositionIncreaseLossBps: U256::ZERO,
            initialMaxPositionDecreaseLossBps: U256::ZERO,
            initialInstrCooldownDuration: U256::ZERO,
            initialMaxSwapLossBps: U256::ZERO,
            initialSwapCooldownDuration: U256::ZERO,
        },
        keccak256(b"makinax fork walk"),
        B256::ZERO,
    );
    let module = create_module.call().await.expect("predict the module");
    let r = create_module
        .send()
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(r.status(), "module creation reverted");
    println!("Safe {safe:?}, module {module:?}");

    // Everything from here on is makinax.
    let session = SafeForkSession::attach(url.clone(), safe)
        .await
        .expect("attach to the fork as the Safe");
    let enable = ISafe::enableModuleCall { module }.abi_encode();
    session
        .apply_single(&format!("{safe:?}"), &alloy::hex::encode_prefixed(enable))
        .await
        .expect("enable the module on the Safe");
    let (setup_batch, _) = build_batch(
        "setup",
        "add the walk's operator",
        8453,
        safe,
        0,
        &[owner_calls::add_operator(module, operator)],
    )
    .unwrap();
    session
        .apply(&setup_batch, 0)
        .await
        .expect("add the operator as the Safe");
    let walk_from = reader.get_block_number().await.unwrap() + 1;

    let cfg = ModuleConfig {
        chain: BASE.into(),
        safe,
        module,
        rootfile: Some(fixture("fork_walk_rootfile.toml")),
        operator_address: Some(operator),
        guardian_address: None,
        swappers: Default::default(),
    };
    let mut mcfg = MakinaxConfig::default();
    mcfg.rpc_urls.insert(BASE.into(), url.as_str().into());
    // The simulation attaches to this fork rather than spawning its own, and
    // an infrastructure failure must fail the walk, not fall back.
    mcfg.dev_rpc_urls.insert(BASE.into(), url.clone());
    mcfg.fork.sim_fallback = false;
    mcfg.modules.insert("walk".into(), cfg.clone());
    let providers = Providers::from_config(&mcfg).await.unwrap();
    let provider = providers.get(BASE).unwrap();

    // ── 1. State, and the root agrees with the fixture. ──
    let state = read_module_state(provider, &cfg)
        .await
        .expect("module state");
    assert_eq!(state.module, module);
    assert_eq!(state.safe, safe);
    assert!(state.safe_matches_config);
    assert_eq!(state.allowed_instr_root, lib.root());
    assert_eq!(state.operating_mode, "OPEN");
    assert!(!state.paused && !state.suspended_by_provider);
    assert_eq!(state.operators, vec![operator]);
    assert!(state.guardians.contains(&safe), "the Safe is a guardian");
    let agreed = check_root(&lib, provider, module)
        .await
        .expect("roots agree");
    assert_eq!(agreed, lib.root());
    println!("1. state read; root {agreed} agrees with the fixture");

    // ── 2. Build the management action with its proof. ──
    let bundle = proof_for(&lib, ACTION).unwrap();
    let built = build_call(
        &cfg,
        &lib,
        &CallSpec::ManagePosition {
            mgmt_action: ACTION.into(),
            mgmt_params: Default::default(),
            acct_action: None,
            acct_params: Default::default(),
        },
    )
    .expect("build the management call");
    assert_eq!(built.to, module);
    assert!(built.is_manage_position);
    assert!(
        !built.has_real_accounting,
        "the fixture pairs no accounting"
    );
    let decoded = IMakinaXModule::managePositionCall::abi_decode(&built.data).unwrap();
    assert_eq!(decoded.mgmt.merkleProof, bundle.proof);
    assert_eq!(decoded.mgmt.positionId, U256::from(1));
    assert!(decoded.acct.commands.is_empty());
    println!("2. built {} ({} bytes)", built.label, built.data.len());

    // ── 3. Simulate on the fork. ──
    let forks = ForkManager::new();
    let receipts = SimReceiptStore::new(100);
    let sim = simulate(
        "walk",
        &cfg,
        &mcfg,
        &built,
        operator,
        SimBackend::Fork,
        &providers,
        &forks,
        &receipts,
        None,
        ExecTarget::Dev,
    )
    .await
    .expect("simulate");
    assert!(sim.success, "simulation failed: {:?}", sim.error);
    assert_eq!(sim.backend, "fork");
    assert!(sim.sim_backend_fallback_from.is_none());
    assert_eq!(sim.operating_mode, "OPEN");
    println!("3. simulated on the fork: gas {:?}", sim.gas_estimate);

    // ── 4. Send through an Executor with an explicit AllowAll hook. ──
    let executor = Executor::new(
        Arc::new(AllowAll),
        EthereumWallet::from(operator_key),
        url.clone(),
    );
    let sent = executor
        .send(BASE, operator, &built, SendOptions::default())
        .await
        .expect("send the management call");
    let receipt = executor
        .wait_receipt(sent.tx_hash, Duration::from_secs(60))
        .await
        .expect("receipt");
    assert!(receipt.status(), "the management call reverted");
    let managed = receipt
        .inner
        .logs()
        .iter()
        .find_map(|l| IMakinaXModule::PositionManaged::decode_log(&l.inner).ok())
        .expect("the receipt carries PositionManaged");
    assert_eq!(managed.positionId, U256::from(1));
    println!("4. sent {:?}: success", sent.tx_hash);

    // ── 5. An owner batch, applied as the Safe. ──
    let new_root = other_lib.root();
    let set_root = encode_owner_call(
        module,
        "setAllowedInstrRoot(bytes32)",
        &[new_root.to_string()],
    )
    .unwrap();
    let chain_id = chain_id_for(BASE).unwrap();
    let (batch, summary) = build_batch(
        "rotate root",
        "adopt the second fixture's root",
        chain_id,
        safe,
        0,
        &[set_root],
    )
    .unwrap();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.chain_id(), "8453");
    println!("5. batch: {summary:?}");
    session.apply(&batch, 0).await.expect("apply the batch");
    let state = read_module_state(provider, &cfg).await.unwrap();
    assert_eq!(state.allowed_instr_root, new_root);
    assert_eq!(
        check_root(&other_lib, provider, module).await.unwrap(),
        new_root
    );
    match check_root(&lib, provider, module).await {
        Err(Error::RootMismatch { loaded, onchain }) => {
            assert_eq!(loaded, lib.root().to_string());
            assert_eq!(onchain, new_root.to_string());
        }
        other => panic!("expected RootMismatch, got {other:?}"),
    }
    println!("5. the module reports the new root {new_root}");

    // ── 6. A CCTP bridge call, built and decoded back. ──
    let encoder = IBridgeEncoderRegistry::new(state.registry, provider)
        .getBridgeEncoder(3)
        .call()
        .await
        .expect("the registry names a CCTP encoder");
    assert_ne!(encoder, Address::ZERO);
    let deployment = |domain, usdc| CctpDeployment {
        domain,
        usdc,
        token_messenger: TOKEN_MESSENGER_V2,
        message_transmitter: MESSAGE_TRANSMITTER_V2,
        // Building reads only the source side; the destination registry
        // and encoder are carried, not used.
        registry: state.registry,
        encoder,
        bridge_id: 3,
        capabilities: CctpCapabilities {
            standard: true,
            fast: true,
        },
    };
    let route = CctpRoute {
        from_chain: 8453,
        to_chain: 1,
        source: deployment(6, USDC_BASE),
        destination: deployment(0, USDC_MAINNET),
        finality: CctpFinality::Standard,
        message_sender: Some(module),
    };
    let recipient = Address::repeat_byte(0x22);
    let order = BridgeOrderArgs {
        bridge_id: 3,
        dest_chain: "mainnet".into(),
        recipient: Some(format!("{recipient:?}")),
        input_token: format!("{USDC_BASE:?}"),
        output_token: format!("{USDC_MAINNET:?}"),
        input_amount: "5000000".into(),
        min_output_amount: "4990000".into(),
        fill_deadline_offset_s: None,
        cctp_finality: None,
    };
    let spec = CallSpec::Bridge { order };
    // Without a route, a CCTP order is refused rather than guessed.
    assert!(build_call(&cfg, &lib, &spec).is_err());
    let bridge = build_call_with_cctp(&cfg, &lib, &spec, Some(&route)).expect("build the bridge");
    assert_eq!(bridge.to, module);
    assert_eq!(bridge.label, "bridge");
    let call = IMakinaXModule::sendOutBridgeTransferCall::abi_decode(&bridge.data).unwrap();
    assert_eq!(call.order.bridgeId, 3);
    assert_eq!(call.order.destinationChainId, U256::from(1));
    assert_eq!(call.order.recipient, recipient);
    assert_eq!(call.order.inputToken, USDC_BASE);
    assert_eq!(call.order.inputAmount, U256::from(5_000_000));
    assert_eq!(call.order.minOutputAmount, U256::from(4_990_000));
    let threshold = U256::abi_decode(&call.order.extraData).unwrap();
    assert_eq!(
        threshold,
        U256::from(CctpFinality::Standard.min_finality_threshold())
    );
    println!("6. CCTP bridge built and decoded (encoder {encoder:?})");

    // ── 7. The walk's events, read back. ──
    let head = reader.get_block_number().await.unwrap();
    let page = read_module_events(provider, module, walk_from, head, &EventsConfig::default())
        .await
        .expect("read the events");
    assert!(page.next_from_block.is_none(), "{:?}", page.note);
    let positions: Vec<_> = page
        .logs
        .iter()
        .filter_map(|l| IMakinaXModule::PositionManaged::decode_log(&l.inner).ok())
        .collect();
    let roots: Vec<_> = page
        .logs
        .iter()
        .filter_map(|l| IMakinaXModule::AllowedInstrRootChanged::decode_log(&l.inner).ok())
        .collect();
    assert_eq!(positions.len(), 1, "step 4's PositionManaged");
    assert_eq!(positions[0].positionId, U256::from(1));
    assert_eq!(
        page.logs
            .iter()
            .find(|l| l.topic0() == Some(&IMakinaXModule::PositionManaged::SIGNATURE_HASH))
            .and_then(|l| l.transaction_hash),
        Some(sent.tx_hash)
    );
    assert_eq!(roots.len(), 1, "step 5's AllowedInstrRootChanged");
    assert_eq!(roots[0].oldRoot, lib.root());
    assert_eq!(roots[0].newRoot, new_root);
    println!(
        "7. {} event(s) over blocks {walk_from}..={head}",
        page.logs.len()
    );

    session.teardown().await;
}
