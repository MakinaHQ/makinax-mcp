//! `makinax::ext` is the integration surface.
//!
//! - It names what a caller needs, in every build (checked at compile time).
//! - `ext::project_router` re-homes a tool router written for one service
//!   type onto another that contains it, and the projected tool is reachable
//!   end to end through a real MCP client over an in-memory pipe.

/// The read, build, prove and encode surface is reachable through `ext`
/// alone, in every build, and a proof is refused for an unknown action.
#[test]
fn ext_names_the_core_surface() {
    use makinax::ext::{
        BuiltCall, CallSpec, Error, EventPage, InstructionLibrary, MakinaxConfig, ModuleConfig,
        ModuleState, OwnerCall, ProofBundle, Result, TxBuilderBatch, build_batch, build_call,
        encode_owner_call, is_range_error, owner_calls, owner_function_names, proof_for,
    };
    fn _types(
        _: Option<(BuiltCall, CallSpec, EventPage, ModuleConfig, ModuleState)>,
        _: Option<(OwnerCall, ProofBundle, TxBuilderBatch, MakinaxConfig)>,
    ) -> Result<()> {
        Ok(())
    }
    let _ = (build_call, build_batch);
    let module = alloy::primitives::Address::repeat_byte(0x11);
    assert!(owner_function_names().contains(&"pause()"));
    let by_name = encode_owner_call(module, "pause()", &[]).unwrap();
    let typed = owner_calls::pause(module);
    assert_eq!(
        (by_name.to, by_name.sig, by_name.args),
        (typed.to, typed.sig, typed.args)
    );
    assert!(is_range_error("query returned more than 10000 results"));
    let empty = InstructionLibrary::empty("none");
    assert!(matches!(
        proof_for(&empty, "nope"),
        Err(Error::UnknownAction(name)) if name == "nope"
    ));
}

/// Every item a standalone module user needs resolves through `ext`, in
/// every build. A missing re-export fails compilation, not a test run.
#[test]
#[allow(unused_imports)]
fn ext_pins_the_surface_in_every_build() {
    use makinax::ext::bindings::{
        IAggregatorV3, IBridgeEncoder, IBridgeEncoderRegistry, IERC20, IERC4626, IMakinaXErrors,
        IMakinaXModule, IMultiSend, IOracleRegistry, ISafe, IWeirollVMErrors,
    };
    use makinax::ext::chains::{
        ChainProfile, KNOWN, by_id, by_key, canonicalize, contract_url, network_by_key,
        public_rpc_for, safe_builder_url, safe_queue_url, service_chain_id, supported_keys,
        tx_service_slug,
    };
    use makinax::ext::{
        Action, ActionKind, ActionParam, BridgeKind, DecodedError, FeedRoute, OraclePrice,
        TokenBalance, TxEntry, check_root, decode_contract_error, decode_revert, mode_label,
        read_balances, read_feed_routes, read_module_events, read_module_state, read_price,
        write_private,
    };

    use alloy::sol_types::SolError;

    let _: fn(&makinax::ext::TxBuilderBatch) -> &[TxEntry] = |b| b.transactions();
    let _: fn(&std::path::Path, &[u8]) -> std::io::Result<()> = write_private;
    assert_eq!(mode_label(2), "WALLED");
    assert_eq!(BridgeKind::describe_id(3), "bridgeId 3 (Circle CCTP v2)");

    // The decoders produce a DecodedError from raw revert data.
    let d: DecodedError = decode_revert(&IMakinaXErrors::AccountingMandatory {}.abi_encode());
    assert!(d.known, "{d:?}");
    assert_eq!(d.code, "accounting_mandatory", "{d:?}");
    let base = by_key("base").expect("base is a known chain");
    assert_eq!(by_id(base.chain_id).map(|c| c.key), Some(base.key));
}

/// The per-concern modules beside the flat items, in every build.
#[test]
#[allow(unused_imports)]
fn ext_pins_the_module_toolkits() {
    use makinax::ext::bridges::across::{
        DepositInfo, FillInfo, SPOKE_POOLS, decode_deposit_logs, decode_fill_logs,
        encode_extra_data, fill_filter,
    };
    use makinax::ext::bridges::cctp::{
        CctpSend, IMessageTransmitterV2, MintAndWithdraw, ROUTE_TOKENS_OR_FINALITY_DIFFER,
        claim_calldata, encode_extra_data as cctp_extra_data, message_transmitter,
        nonce_from_message, token_messenger, validate_route,
    };
    use makinax::ext::bridges::metadata_rpc::{
        ICctpEncoderFacts, ITokenMessengerFacts, ITokenMinterFacts, MODULE_REGISTRY_DIFFERS,
        RPC_CHAIN_DIFFERS, verify_destination, verify_encoding, verify_endpoint, verify_route,
    };
    use makinax::ext::bridges::{
        BridgeState, PendingBridge, TrackedScan, TrackingCause, TrackingGap, across_spoke_pool,
        in_flight_raw_by_token,
    };
    use makinax::ext::build::{
        CCTP_ROUTE_REQUIRED, empty_accounting_for, encode_call, label_for, parse_hex,
    };
    use makinax::ext::chain::{
        CannotAnswer, RecentHeadError, cannot_answer_error, clock_now_s, endpoint_is_local,
        head_floor_admits, head_is_recent, http_client, parse_chain_filter, recent_head,
        reset_head_floor, usdc_for,
    };
    use makinax::ext::dyn_call::{CheckOutcome, run_check};
    use makinax::ext::fetch::mismatch;
    use makinax::ext::instructions::{
        ParamMap, ParamValue, PreviewedInstruction, PreviewedRawInstruction, build_instruction,
        fetch_onchain_root, matches,
    };
    use makinax::ext::owner::{ValueCall, build_value_batch, call_summary, single_call};
    #[cfg(unix)]
    use makinax::ext::perms::FORBIDDEN_BITS;
    use makinax::ext::perms::harden_dir;
    use makinax::ext::reads::{
        read_balances_cached, read_balances_cached_at, registered_tokens, root_tokens,
        scan_universe,
    };
    use makinax::ext::redact::{MARK, text, url};
    use makinax::ext::secrets::{
        configured_passphrase_env, empty_overrides, names_with_configured, scrub,
    };
    use makinax::ext::swappers::{
        KYBER_ROUTER, REG_ABSENT, REG_FOUND, REG_UNKNOWN, Registered, RouterLookup, SCAN_MAX_ID,
        Scan, adapter_for_router, all_unreadable, scan,
    };

    let _: fn(&str, &[String]) -> makinax::ext::Result<Vec<u8>> =
        makinax::ext::dyn_call::encode_call;
    assert!(endpoint_is_local("http://127.0.0.1:8545"));
    #[cfg(unix)]
    assert_eq!(FORBIDDEN_BITS, 0o077);
    assert!(!url("https://user:hunter2@rpc.example/x").contains("hunter2"));
    assert_eq!(
        text("failed:\n https://user:hunter2@rpc.example/x"),
        format!("failed:\n https://user:{MARK}@rpc.example/x")
    );
    assert!(matches(
        alloy::primitives::B256::repeat_byte(1),
        alloy::primitives::B256::repeat_byte(1)
    ));
}

/// The error variants, config types and generated items a caller matches on
/// or builds, named through `ext`, each with the behaviour a caller relies on.
#[test]
fn ext_exposes_what_a_caller_matches_on() {
    use makinax::ext::bindings::IMakinaXModule;
    use makinax::ext::{
        ActionKind, BridgesConfig, CctpV2Config, Error, GcpSignerConfig, LzV2Config, ProviderMode,
        SignerConfig,
    };

    assert_eq!(
        Error::Io(std::io::Error::other("disk full")).to_string(),
        "disk full"
    );
    assert_eq!(
        Error::Merkle("bad leaf".into()).to_string(),
        "merkle: bad leaf"
    );
    assert_eq!(
        Error::UnknownModule("m1".into()).to_string(),
        "unknown module: m1"
    );
    let e = Error::UnknownParam {
        action: "a.b".into(),
        param: "amount".into(),
    };
    assert!(e.to_string().contains("no runtime param `amount`"), "{e}");
    match (Error::MetaTypeSlot {
        action: "a.b".into(),
        slot: "swap".into(),
    }) {
        Error::MetaTypeSlot { action, slot } => {
            assert_eq!((action.as_str(), slot.as_str()), ("a.b", "swap"))
        }
        other => panic!("{other:?}"),
    }
    let e = Error::SimFailed {
        code: "reverted".into(),
        explanation: "boom".into(),
    };
    assert_eq!(e.to_string(), "simulation failed: boom");
    assert_eq!(
        Error::QuoteNotFound("q-1".into()).to_string(),
        "quote not found: q-1"
    );
    assert_eq!(
        Error::QuoteStale { age_s: 9, ttl_s: 5 }.to_string(),
        "quote stale: 9s old, ttl 5s"
    );
    assert_eq!(
        Error::FetchRateLimited {
            service: "merkl".into(),
            retry_after_s: Some(3),
        }
        .to_string(),
        "merkl: rate limited"
    );
    assert!(matches!(Error::from(eyre::eyre!("x")), Error::Internal(_)));

    assert_ne!(ActionKind::Accounting, ActionKind::Management);
    assert_ne!(ProviderMode::Mainnet, ProviderMode::Dev);
    assert!(
        IMakinaXModule::IMakinaXModuleCalls::SIGNATURES.contains(&"allowedInstrRoot()"),
        "the module's call signatures are listed"
    );

    let bridges = BridgesConfig {
        cctp_v2: Some(CctpV2Config {
            domains: [("base".to_string(), 6)].into(),
        }),
        lz_v2: Some(LzV2Config {
            endpoints: [("base".to_string(), 30184)].into(),
        }),
        across_v4: None,
    };
    assert_eq!(bridges.cctp_v2.unwrap().domains["base"], 6);
    assert_eq!(bridges.lz_v2.unwrap().endpoints["base"], 30184);
    let gcp = SignerConfig::Gcp(GcpSignerConfig {
        project_id: "p".into(),
        location: "global".into(),
        keyring: "k".into(),
        key_name: "n".into(),
        key_version: 1,
    });
    assert!(matches!(
        gcp,
        SignerConfig::Gcp(GcpSignerConfig { key_version: 1, .. })
    ));
}

/// The per-concern modules that need `write`: signing and simulation.
#[cfg(feature = "write")]
#[test]
#[allow(unused_imports)]
fn ext_pins_the_write_toolkits() {
    use makinax::ext::signer::{
        KEYSTORE_PASSPHRASE_UNAVAILABLE, PRIVATE_KEY_REFUSED, SignerInfo, build_hash_signer,
        build_hash_signer_scrubbing, build_wallet_scrubbing, describe, is_non_dev_signer,
        keychain_lookup_command, resolve_keystore_passphrase,
    };
    use makinax::ext::sim::fork::{
        OUR_ANVIL_MARKER, READINESS_CHILD_HEADROOM_MS, READINESS_TIMEOUT_MS, anvil_env_overrides,
        anvil_fork_args, bundled_base_anvil, fork_source, readiness_error, readiness_failed,
        resolve_anvil_path, sweep_orphaned_forks,
    };
    use makinax::ext::sim::gas::{MARGIN_BPS, SendLimit, budget, send_limit};
    #[cfg(feature = "tenderly")]
    use makinax::ext::sim::simulate::diffs_from_asset_changes;
    use makinax::ext::sim::simulate::{
        DEV_CONTAMINATED_REMEDY, erc4626_probe, fidelity_note, fork_provenance,
        out_of_gas_remediation, tenderly_error,
    };
    #[cfg(feature = "tenderly")]
    use makinax::ext::sim::tenderly::{
        AssetChange, ParsedSimulation, SimulationOutcome, parse_simulate_response,
    };
    use makinax::ext::sim::timings::{StallMarker, Timings};
    use makinax::ext::sim::trace::{CallTrace, Frame, fetch, flatten};
    use makinax::ext::sim::weiroll_diag::units;
    assert_eq!(units(alloy::primitives::U256::from(1_500_000u64), 6), "1.5");
    let spawned = makinax::ext::ForkManager::with_options(makinax::ext::SpawnOptions::default());
    assert!(
        spawned.child_pids().is_empty(),
        "nothing is spawned until asked"
    );
    assert_eq!(units(alloy::primitives::U256::from(7u64), 0), "7");
}

/// The fetchers' lower-level pieces, each with its own feature.
#[cfg(feature = "quote")]
#[test]
#[allow(unused_imports)]
fn ext_pins_the_fetch_toolkit() {
    #[cfg(feature = "fetch-kyber")]
    use makinax::ext::fetch::Kyber;
    #[cfg(feature = "fetch-odos")]
    use makinax::ext::fetch::Odos;
    #[cfg(feature = "fetch-cctp")]
    use makinax::ext::fetch::{
        EXPECTED_ATTESTATION_S, fetch_attestation_matching, interpret_matching,
    };
    use makinax::ext::fetch::{classify_quote_error, quote_all};
    let _: fn(&str) -> &'static str = classify_quote_error;
}

/// The simulation, signing and sending surface, only with `write`.
#[cfg(feature = "write")]
#[test]
#[allow(unused_imports)]
fn ext_pins_the_write_surface() {
    use makinax::ext::{
        AllowAll, ApplyFailure, BalanceDiff, CallEcho, ExecTarget, Executor, ExpectedChange,
        ForkManager, ForkReadiness, ForkStats, GasBudget, HookVerdict, PreSendHook, PreparedTx,
        SafeForkSession, SendOptions, Sent, SimBackend, SimReceipt, SimReceiptStore,
        SimulationResult, SpawnOptions, TenderlyRef, build_wallet, simulate,
    };
    // There is no hook-less constructor: an Executor always takes one.
    let _: fn(std::sync::Arc<dyn PreSendHook>, alloy::network::EthereumWallet, String) -> Executor =
        Executor::new;
    let hook: std::sync::Arc<dyn PreSendHook> = std::sync::Arc::new(AllowAll);
    drop(hook);
}

/// The Tenderly client `simulate` takes, only with `tenderly`.
#[cfg(feature = "tenderly")]
#[test]
fn ext_pins_the_tenderly_client() {
    use makinax::ext::{Result, TenderlyClient, TenderlyConfig};
    let _: fn(TenderlyConfig) -> Result<TenderlyClient> = TenderlyClient::new;
}

/// The attestation fetcher and the API it is pointed at, only with
/// `fetch-cctp`.
#[cfg(feature = "fetch-cctp")]
#[test]
fn ext_names_the_attestation_api() {
    use makinax::ext::{Attestation, IRIS_BASE_URL, fetch_attestation};
    let _ = fetch_attestation;
    let _: Option<Attestation> = None;
    assert!(IRIS_BASE_URL.starts_with("https://"), "{IRIS_BASE_URL}");
}

/// The quote surface, only with `quote` (or a built-in swap fetcher).
#[cfg(feature = "quote")]
#[test]
#[allow(unused_imports)]
fn ext_pins_the_quote_surface() {
    use makinax::ext::{
        Quote, QuoteAdapter, QuoteRequest, QuoteSummary, best_quote, build_swap_from_quote,
        default_adapters, default_adapters_named, min_out_for, swap_order_from_quote,
        validate_swap_quote, validate_swap_quote_with_spenders,
    };
    let _ = min_out_for(alloy::primitives::U256::from(10_000u64), 100);
}

#[cfg(feature = "mcp")]
mod mcp {
    use makinax::ext::project_router;
    use rmcp::{
        ServerHandler, ServiceExt,
        handler::server::{router::tool::ToolRouter, wrapper::Parameters},
        model::{CallToolRequestParams, ServerCapabilities, ServerInfo},
        schemars, tool, tool_handler, tool_router,
    };
    use serde::Deserialize;

    /// The service a library writes its tools against.
    struct Inner {
        prefix: String,
    }

    #[derive(Debug, Deserialize, schemars::JsonSchema)]
    struct EchoArgs {
        text: String,
    }

    #[tool_router]
    impl Inner {
        #[tool(description = "Echo the text back, with the service's prefix")]
        fn echo(&self, Parameters(args): Parameters<EchoArgs>) -> String {
            format!("{}{}", self.prefix, args.text)
        }
    }

    /// A caller's own service, holding the library's service inside it.
    struct Outer {
        inner: Inner,
        tool_router: ToolRouter<Outer>,
    }

    impl Outer {
        fn new(prefix: &str) -> Self {
            Self {
                inner: Inner {
                    prefix: prefix.to_string(),
                },
                tool_router: project_router(Inner::tool_router(), |o: &Outer| &o.inner),
            }
        }
    }

    #[tool_handler(router = self.tool_router)]
    impl ServerHandler for Outer {
        fn get_info(&self) -> ServerInfo {
            let mut info = ServerInfo::default();
            info.capabilities = ServerCapabilities::builder().enable_tools().build();
            info
        }
    }

    #[test]
    fn the_projected_router_lists_the_inner_tools() {
        let router = project_router(Inner::tool_router(), |o: &Outer| &o.inner);
        let names: Vec<String> = router
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(names, vec!["echo".to_string()]);
        let schema = serde_json::to_string(&router.list_all()[0].input_schema).unwrap();
        assert!(schema.contains("\"text\""), "input schema kept: {schema}");
    }

    #[test]
    fn the_projected_router_keeps_transparent_when_not_found() {
        for flag in [false, true] {
            let mut inner = Inner::tool_router();
            inner.transparent_when_not_found = flag;
            let router = project_router(inner, |o: &Outer| &o.inner);
            assert_eq!(router.transparent_when_not_found, flag);
        }
    }

    #[tokio::test]
    async fn a_client_calls_the_projected_tool_over_an_in_memory_pipe() {
        let (client_side, server_side) = tokio::io::duplex(1 << 16);
        let server = tokio::spawn(async move {
            let running = Outer::new("echo: ").serve(server_side).await.unwrap();
            let _ = running.waiting().await;
        });

        let client = ().serve(client_side).await.unwrap();

        let tools = client.list_all_tools().await.unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, vec!["echo"]);

        let args = serde_json::json!({ "text": "hello" });
        let result = client
            .call_tool(
                CallToolRequestParams::new("echo")
                    .with_arguments(args.as_object().unwrap().clone()),
            )
            .await
            .unwrap();
        assert_ne!(result.is_error, Some(true), "tool errored: {result:?}");
        let text = result.content[0].as_text().unwrap().text.clone();
        assert_eq!(text, "echo: hello", "the call reached the caller's Inner");

        // A tool the inner router does not have is refused, not routed.
        let missing = client.call_tool(CallToolRequestParams::new("nope")).await;
        assert!(
            missing.is_err(),
            "unknown tool must be refused: {missing:?}"
        );

        client.cancel().await.unwrap();
        server.await.unwrap();
    }
}
