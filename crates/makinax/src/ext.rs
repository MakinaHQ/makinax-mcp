//! `makinax::ext` is the only supported integration API. Everything else may
//! change in any release.
//!
//! Everything a caller needs to read a makinaX module, build and prove a call
//! against it, check fetched data, simulate, encode owner calls and (with the
//! `write` feature) send through a caller-supplied pre-send hook is
//! re-exported here. With the `mcp` feature, [`project_router`] lets a caller
//! mount tools written against one service type on its own service.
//!
//! Feature gates follow the items: the quote types need `quote` (or a
//! built-in swap fetcher), simulation, signing and sending need `write`, and
//! [`project_router`] needs `mcp`. A build without `write` has no way to sign
//! or broadcast.

// Configuration and errors.
pub use crate::config::{
    AcrossV4Config, BridgesConfig, CctpV2Config, Endpoints, EventsConfig, FetchConfig, ForkConfig,
    GcpSignerConfig, KeystoreSignerConfig, LzV2Config, MakinaxConfig, ModuleConfig,
    PrivateKeySignerConfig, RootfileSource, SignerConfig, TenderlyConfig,
};
pub use crate::error::{Error, Result};

// Chains: the verified table of networks a module can live on.
pub mod chains {
    //! The chain table: canonical keys, chain ids, public RPCs and explorer
    //! and Safe links for every network this crate knows.
    pub use crate::chains::{
        ChainProfile, KNOWN, by_id, by_key, canonicalize, contract_url, network_by_key,
        public_rpc_for, safe_builder_url, safe_queue_url, service_chain_id, supported_keys,
        tx_service_slug,
    };
}

// Contract bindings, for decoding a module's events, calls and reverts.
pub mod bindings {
    //! `sol!` bindings of the makinaX module and the contracts it talks to:
    //! call, event and error types, with `rpc` instances. Decode a revert
    //! into a [`DecodedError`](super::DecodedError) with
    //! [`decode_revert`](super::decode_revert) or
    //! [`decode_contract_error`](super::decode_contract_error).
    pub use crate::chain::module::{
        IAggregatorV3, IBridgeEncoder, IBridgeEncoderRegistry, IERC20, IERC4626, IMakinaXModule,
        IMultiSend, IOracleRegistry, ISafe,
    };
    pub use crate::chain::taxonomy::{IMakinaXErrors, IWeirollVMErrors};
}

// Providers.
pub use crate::chain::{
    ProviderMode, Providers, RootProvider, chain_id_for, provider_from_endpoints,
};

pub mod chain {
    //! Chain helpers beside the providers: how fresh a head is, whether an
    //! endpoint is local, the chain filter a tool argument names, the
    //! canonical USDC per chain, and the hedged transport's "cannot answer"
    //! error.
    pub use crate::chain::assets::usdc_for;
    pub use crate::chain::hedge::{CannotAnswer, cannot_answer_error};
    pub use crate::chain::provider::http_client;
    pub use crate::chain::{
        RecentHeadError, clock_now_s, endpoint_is_local, head_floor_admits, head_is_recent,
        parse_chain_filter, recent_head, reset_head_floor,
    };
}

pub mod swappers {
    //! The module's swapper registry: scan the registered swapper ids, and
    //! map a quote's router to the adapter that trades through it.
    pub use crate::chain::swappers::{
        KYBER_ROUTER, REG_ABSENT, REG_FOUND, REG_UNKNOWN, Registered, RouterLookup, SCAN_MAX_ID,
        Scan, adapter_for_router, all_unreadable, scan,
    };
}

/// Signer construction beside [`build_wallet`]: a hash signer, the
/// scrubbing variants that also clear a caller's own passphrase variable
/// names from a keychain lookup, and the neutral refusal texts, exported so
/// a caller can match them.
#[cfg(feature = "write")]
pub mod signer {
    pub use crate::chain::signer::{
        KEYSTORE_PASSPHRASE_UNAVAILABLE, PRIVATE_KEY_REFUSED, SignerInfo, build_hash_signer,
        build_hash_signer_scrubbing, build_wallet_scrubbing, describe, is_non_dev_signer,
        keychain_lookup_command, resolve_keystore_passphrase,
    };
}

// Building calls.
pub use crate::bridges::BridgeKind;
pub use crate::bridges::cctp::{CctpCapabilities, CctpDeployment, CctpFinality, CctpRoute};
pub use crate::build::{
    BridgeOrderArgs, BuiltCall, CallSpec, SwapOrderArgs, build_bridge, build_call,
    build_call_with_cctp, call_key,
};
#[cfg(feature = "quote")]
pub use crate::build::{build_swap_from_quote, swap_order_from_quote};
pub use crate::chain::taxonomy::{
    DECLARED_ERROR, DeclaredError, DecodedError, REFUSED_WITHOUT_A_REASON, decode_contract_error,
    decode_failing_frame, decode_revert, decode_revert_declared,
};

pub mod build {
    //! Lower-level encoding beside [`build_call`](super::build_call): one
    //! call from its spec and config, the label of a spec, an empty
    //! accounting instruction, hex parsing, and the neutral refusal text for
    //! a CCTP call without a route.
    pub use crate::build::{
        CCTP_ROUTE_REQUIRED, empty_accounting_for, encode_call, label_for, parse_hex,
    };
}

pub mod bridges {
    //! Bridge tracking, and the per-protocol helpers beside
    //! [`BridgeKind`](super::BridgeKind) and the CCTP route types.
    pub use crate::bridges::{
        BridgeState, PendingBridge, TrackedScan, TrackingCause, TrackingGap, across_spoke_pool,
        in_flight_raw_by_token,
    };

    pub mod across {
        //! Across V3: spoke pools, deposit and fill logs, `extraData`.
        pub use crate::bridges::across::{
            DepositInfo, FillInfo, SPOKE_POOLS, decode_deposit_logs, decode_fill_logs,
            encode_extra_data, fill_filter,
        };
    }

    pub mod cctp {
        //! Circle CCTP V2: contract addresses, the message transmitter's
        //! binding, the claim, the settlement event, message nonces,
        //! `extraData`, and route validation.
        pub use crate::bridges::cctp::{
            CctpSend, IMessageTransmitterV2, MintAndWithdraw, ROUTE_TOKENS_OR_FINALITY_DIFFER,
            claim_calldata, encode_extra_data, message_transmitter, nonce_from_message,
            token_messenger, validate_route,
        };
    }

    pub mod metadata_rpc {
        //! On-chain checks of a bridge route against the endpoints, the
        //! destination and the module's encoder, with the neutral refusal
        //! texts a caller can match.
        pub use crate::bridges::metadata_rpc::{
            ICctpEncoderFacts, ITokenMessengerFacts, ITokenMinterFacts, MODULE_REGISTRY_DIFFERS,
            RPC_CHAIN_DIFFERS, verify_destination, verify_encoding, verify_endpoint, verify_route,
        };
    }
}

// Instructions and proofs.
pub use crate::instructions::{
    Action, ActionKind, ActionParam, InstructionLibrary,
    proofs::{ProofBundle, check_root, proof_for},
};

pub mod instructions {
    //! Previewing one instruction from typed parameters, and reading the
    //! module's on-chain root to compare with a loaded library.
    pub use crate::instructions::builder::{
        MetaFill, ParamMap, ParamValue, PreviewedInstruction, PreviewedRawInstruction,
        build_instruction, build_instruction_with_meta,
    };
    pub use crate::instructions::verify::{fetch_onchain_root, matches};
}

// Owner calls (encoded only, never sent).
pub use crate::owner::{
    batch::{OwnerCall, TxBuilderBatch, TxEntry, build_batch},
    encode::{encode_owner_call, owner_function_names},
};

/// One typed encoder per owner-restricted function of the module. Each
/// returns an [`OwnerCall`] to hand to [`build_batch`]; nothing is sent.
pub mod owner_calls {
    pub use crate::owner::encode::{
        add_guardian, add_operator, add_recipient, clear_feed_route, pause, remove_guardian,
        remove_operator, remove_recipient, set_accounting_currency, set_allowed_instr_root,
        set_bridge_cooldown_duration, set_feed_route, set_feed_stale_threshold,
        set_instr_cooldown_duration, set_max_bridge_loss_bps, set_max_position_decrease_loss_bps,
        set_max_position_increase_loss_bps, set_max_swap_loss_bps, set_operating_mode,
        set_swap_cooldown_duration, set_swapper_targets, sweep_erc20, sweep_erc721, sweep_erc6909,
        sweep_native, unpause,
    };
}

pub mod owner {
    //! Owner batches with native value, and one-call summaries.
    pub use crate::owner::batch::{ValueCall, build_value_batch, call_summary, single_call};
}

pub mod dyn_call {
    //! A call from a human-readable signature and string arguments, and a
    //! read-only check run against a module.
    pub use crate::owner::dyn_call::{CheckOutcome, encode_call, run_check};
}

// Reads.
pub use crate::reads::{
    balances::{TokenBalance, read_balances},
    events::{EventPage, is_range_error, read_module_events},
    module_state::{ModuleState, SwapperTargets, mode_label, read_module_state},
    oracle::{FeedRoute, OraclePrice, read_feed_routes, read_price},
};

pub mod reads {
    //! Balance reads beside [`read_balances`](super::read_balances): the
    //! token universe a library names, and cached reads.
    pub use crate::reads::balances::{
        read_balances_cached, read_balances_cached_at, registered_tokens, root_tokens,
        scan_universe,
    };

    pub mod merkl {
        //! The Merkl Distributor a harvest leaf claims from: its address,
        //! read from the leaf, and its root, claimed and recipient views.
        pub use crate::reads::merkl::{
            ClaimedRecord, IMerklDistributor, distributor_of, read_claim_recipient, read_claimed,
            read_merkle_root,
        };
    }
}

// Fetched data: quotes, and the checks a fetched value passes before it is
// encoded.
#[cfg(feature = "fetch-across")]
pub use crate::fetch::across::{
    ACROSS_API_URL, AcrossFeeQuote, AcrossFeeRequest, fetch_suggested_fees,
};
#[cfg(feature = "fetch-cctp")]
pub use crate::fetch::cctp_attestation::{Attestation, IRIS_BASE_URL, fetch_attestation};
#[cfg(feature = "quote")]
pub use crate::fetch::quote::{
    Quote, QuoteAdapter, QuoteRequest, QuoteSummary, best_quote, default_adapters,
    default_adapters_named, min_out_for,
};
pub use crate::fetch::validate::{
    BridgeExpectation, SwapExpectation, check_field, validate_bridge_order,
};
#[cfg(feature = "quote")]
pub use crate::fetch::validate::{validate_swap_quote, validate_swap_quote_with_spenders};
pub use crate::fetch::{across_enabled, cctp_attestation_enabled};

pub mod fetch {
    //! The fetchers' lower-level pieces: every quote at once and how a quote
    //! error is classified, the built-in vendor adapters, attestation polling
    //! that accepts only a matching message, and the mismatch error a check
    //! raises.
    #[cfg(feature = "fetch-cctp")]
    pub use crate::fetch::cctp_attestation::{
        EXPECTED_ATTESTATION_S, fetch_attestation_matching, interpret_matching,
    };
    #[cfg(feature = "fetch-kyber")]
    pub use crate::fetch::kyber::Kyber;
    #[cfg(feature = "fetch-odos")]
    pub use crate::fetch::odos::Odos;
    #[cfg(feature = "quote")]
    pub use crate::fetch::quote::{classify_quote_error, quote_all};
    pub use crate::fetch::validate::mismatch;

    pub mod merkl {
        //! Merkl rewards for a `MerklClaimData` harvest: parse the API's
        //! answer, and encode the claim as the leaf's meta-slot words.
        #[cfg(feature = "fetch-merkl")]
        pub use crate::fetch::merkl::fetch_rewards;
        pub use crate::fetch::merkl::{
            BreakdownData, ClaimData, MERKL_API_URL, MERKL_CLAIM_META_TYPE, MerklResponse,
            MerklRewards, RewardsData, TokenData, parse_rewards, rewards_url,
        };
    }
}

// Simulation.
#[cfg(feature = "write")]
pub use crate::owner::sim::{ApplyFailure, SafeForkSession};
#[cfg(feature = "tenderly")]
pub use crate::sim::tenderly::TenderlyClient;
#[cfg(feature = "write")]
pub use crate::sim::{
    fork::{ExecTarget, ForkManager, ForkReadiness, ForkStats, SpawnOptions},
    gas::GasBudget,
    receipts::{SimReceipt, SimReceiptStore},
    simulate::{
        BalanceDiff, CallEcho, ExpectedChange, SimBackend, SimulationResult, TenderlyRef, simulate,
    },
};

/// Simulation tooling beside the flat simulation items, one module per
/// concern (`write`).
#[cfg(feature = "write")]
pub mod sim {
    pub mod fork {
        //! The anvil fork's process: its arguments and environment, where
        //! the binary is found, readiness texts and timeouts, and reaping
        //! forks left behind by a crashed process.
        pub use crate::sim::fork::{
            OUR_ANVIL_MARKER, READINESS_CHILD_HEADROOM_MS, READINESS_TIMEOUT_MS,
            anvil_env_overrides, anvil_fork_args, bundled_base_anvil, fork_source, readiness_error,
            readiness_failed, resolve_anvil_path, sweep_orphaned_forks,
        };
    }

    pub mod gas {
        //! The gas limit a send uses, from a simulated estimate and a live
        //! one.
        pub use crate::sim::gas::{MARGIN_BPS, SendLimit, budget, send_limit};
    }

    pub mod simulate {
        //! A simulation result's provenance, fidelity and remedies, and
        //! (with `tenderly`) balance diffs from a Tenderly response.
        #[cfg(feature = "tenderly")]
        pub use crate::sim::simulate::diffs_from_asset_changes;
        pub use crate::sim::simulate::{
            DEV_CONTAMINATED_REMEDY, erc4626_probe, fidelity_note, fork_provenance,
            out_of_gas_remediation, tenderly_error,
        };
    }

    /// Parsing a Tenderly simulation response (`tenderly`).
    #[cfg(feature = "tenderly")]
    pub mod tenderly {
        pub use crate::sim::tenderly::{
            AssetChange, ParsedSimulation, SimulationOutcome, parse_simulate_response,
        };
    }

    pub mod timings {
        //! Phase-level wall-clock for a multi-phase call, and the last phase
        //! a stalled call completed.
        pub use crate::sim::timings::{StallMarker, Timings};
    }

    pub mod trace {
        //! `debug_traceTransaction` and `debug_traceCall` call traces: fetch,
        //! flatten, find the frame that ran out of gas or reverted, and follow
        //! a revert to the frame it started in.
        pub use crate::sim::trace::{
            CallTrace, FailingFrame, Frame, failing_frame, fetch, fetch_call, flatten,
        };
    }

    pub mod weiroll_diag {
        //! Formatting token amounts in a failed weiroll instruction's
        //! diagnosis.
        pub use crate::sim::weiroll_diag::units;
    }
}

// Files.
pub use crate::util::perms::write_private;

pub mod perms {
    //! Owner-only permissions beside [`write_private`](super::write_private):
    //! harden a directory to 0700, and the permission bits a private file
    //! must not have.
    #[cfg(unix)]
    pub use crate::util::perms::FORBIDDEN_BITS;
    pub use crate::util::perms::harden_dir;
}

pub mod redact {
    //! Removing credentials from an RPC URL before it is shown: [`url`] for
    //! a URL or a one-line message, [`text`] for a message whose own
    //! whitespace must be kept.
    pub use crate::util::redact::{MARK, text, url};
}

pub mod secrets {
    //! Keeping passphrase variables out of a child process: the names to
    //! clear, clearing them from a command, and the overrides that blank
    //! them.
    pub use crate::util::secrets::{
        configured_passphrase_env, empty_overrides, names_with_configured, scrub,
    };
}

// Signing and sending, always behind a caller-supplied hook.
#[cfg(feature = "write")]
pub use crate::chain::signer::build_wallet;
#[cfg(feature = "write")]
pub use crate::exec::{
    AllowAll, Executor, HookVerdict, PreSendHook, PreparedTx, SendOptions, Sent,
};

#[cfg(feature = "mcp")]
use rmcp::{
    handler::server::{
        router::tool::{ToolRoute, ToolRouter},
        tool::ToolCallContext,
    },
    model::CallToolRequestParams,
};

/// Re-home a tool router onto a caller's service type. `project` borrows the
/// makinax-side service out of the caller's.
///
/// Every route keeps its tool attributes (name, description, schemas,
/// annotations); a call on `S` is forwarded, with its arguments, task and
/// request context, to the same handler on `project(&s)`. Routes the input
/// router has disabled are not carried over, and its
/// `transparent_when_not_found` setting is copied.
#[cfg(feature = "mcp")]
pub fn project_router<S, M>(router: ToolRouter<M>, project: fn(&S) -> &M) -> ToolRouter<S>
where
    S: Send + Sync + 'static,
    M: Send + Sync + 'static,
{
    let mut out = ToolRouter::<S>::new();
    out.transparent_when_not_found = router.transparent_when_not_found;
    for route in router {
        let call = route.call.clone();
        out.add_route(ToolRoute::new_dyn(
            route.attr.clone(),
            move |ctx: ToolCallContext<'_, S>| {
                let mut params = CallToolRequestParams::new(ctx.name);
                if let Some(arguments) = ctx.arguments {
                    params = params.with_arguments(arguments);
                }
                if let Some(task) = ctx.task {
                    params = params.with_task(task);
                }
                let inner = ToolCallContext::new(project(ctx.service), params, ctx.request_context);
                (call)(inner)
            },
        ));
    }
    out
}
