//! Type-safe bindings for the on-chain surfaces we touch.
//!
//! `IMakinaXModule` covers the operator- and owner-callable functions on
//! `MakinaXModule.sol`. Field order on the `Instruction` struct matches
//! `IWeirollComponent.sol:38-49` exactly. The owner-only admin section
//! (below) is verified against the real contract source, makina-x @
//! 6168e175 (`MakinaXGovernable.sol`, `MakinaXModule.sol` and the
//! `OracleRegistry`/`SwapComponent`/`BridgeComponent` components it
//! inherits) — every function there is cited against its actual access
//! modifier, not inferred from product call sites.

// ISafe::execTransaction mirrors the on-chain function's exact parameter
// list (10 args) — reducing it would no longer match the ABI it decodes.
#![allow(clippy::too_many_arguments)]

use alloy::sol;

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function decimals() external view returns (uint8);
        function symbol() external view returns (string memory);
        function transfer(address to, uint256 amount) external returns (bool);
        event Transfer(address indexed from, address indexed to, uint256 value);
    }
}

sol! {
    #[sol(rpc)]
    interface IERC4626 {
        function maxDeposit(address receiver) external view returns (uint256);
        function maxWithdraw(address owner) external view returns (uint256);
        function maxRedeem(address owner) external view returns (uint256);
        function asset() external view returns (address);
    }

    #[sol(rpc)]
    interface ISafe {
        function getOwners() external view returns (address[] memory);
        function getThreshold() external view returns (uint256);
        function isModuleEnabled(address module) external view returns (bool);
        // Module linked-list walk (resolves `prevModule` for
        // disableModule). SENTINEL_MODULES = address(0x1).
        function getModulesPaginated(address start, uint256 pageSize) external view returns (address[] memory array, address next);
        // Safe self-calls an owner batch may encode (never sent by this
        // crate — the OWNER executes them via the Safe Tx Builder).
        function enableModule(address module) external;
        function disableModule(address prevModule, address module) external;
        // EIP-1271 (contract signers, e.g. a nested Safe as owner)
        function isValidSignature(bytes32 hash, bytes memory signature) external view returns (bytes4);
        // Historical-calldata decoding: past owner transactions carry values
        // the chain does not expose via getters (e.g. setFeedRoute staleness
        // thresholds).
        function execTransaction(address to, uint256 value, bytes calldata data, uint8 operation, uint256 safeTxGas, uint256 baseGas, uint256 gasPrice, address gasToken, address refundReceiver, bytes memory signatures) external payable returns (bool success);
    }

    #[sol(rpc)]
    interface IMultiSend {
        function multiSend(bytes memory transactions) external payable;
    }

    // Chainlink-style price feed: the aggregators a declared `setFeedRoute`
    // call points at. Read DIRECTLY over our own RPC for
    // ADVISORY display/context — `updatedAt` is what makes heartbeat
    // staleness legible. Valuation never comes from here: that is the
    // caliber's on-chain oracle route (IOracleRegistry below).
    #[sol(rpc)]
    interface IAggregatorV3 {
        function decimals() external view returns (uint8);
        function description() external view returns (string memory);
        function latestRoundData() external view returns (
            uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound
        );
    }

    #[sol(rpc)]
    interface IOracleRegistry {
        function getPrice(address baseToken, address quoteToken) external view returns (uint256);
        function getReferencePrice(address token) external view returns (uint256);
        function isFeedRouteRegistered(address token) external view returns (bool);
        function getFeedRoute(address token) external view returns (address feed1, address feed2);
        // Verified live (selector 0xcf704f83, mainnet module
        // 0x5AFe…B003, from = the Safe): USDC's aggregator 83400, WETH's
        // 4200, weETH leg1 1, weETH leg2 4200.
        //
        // It was absent from this binding, and reading OUR ABI rather than the
        // contract produced the conclusion that tolerances were unreadable
        // on-chain — which nearly bought calldata archaeology and a class of
        // "unverified tolerance" reporting that never needed to exist.
        //
        // KEYED BY FEED ADDRESS, not by (token, leg). weETH's second leg and
        // WETH's first leg are the same aggregator and report the same 4200.
        // One aggregator therefore has ONE tolerance for the whole module:
        // two independent configurations declaring different tolerances for
        // it cannot both be satisfied, and whichever registers last
        // silently wins.
        function getFeedStaleThreshold(address feed) external view returns (uint256);
        // Safe-only config (useful for registering feed routes against a
        // fork in tests).
        function setFeedRoute(address token, address feed1, uint256 stalenessThreshold1, address feed2, uint256 stalenessThreshold2) external;
    }
}

sol! {
    #[sol(rpc)]
    interface IBridgeEncoder {
        // ACROSS encoders: a route table keyed (input, chain, output).
        function isRouteRegistered(address inputToken, uint256 destinationChainId, address outputToken) external view returns (bool);
        // CCTP encoders: there is NO route table. The encoder maps a chain id
        // to the Circle domain it will send to, and that mapping is the whole
        // registration. Measured live on the encoder both the Base
        // and mainnet registries return for bridgeId 3:
        //   getCctpDomain(999)  -> 19 on both (HyperEVM, registered);
        //   getCctpDomain(1)    -> 0  (mainnet's REAL Circle domain, not an
        //                             absence — never test this for zero);
        //   getCctpDomain(8453) -> reverts CctpDomainNotRegistered on Base,
        //                             its own chain; answers 6 on mainnet.
        // `isRouteRegistered` on that same encoder reverts BARE, no reason
        // string: the function does not exist there. So a bare revert from
        // one selector means "wrong question", and a named
        // CctpDomainNotRegistered means "right question, answer is no". The
        // contract is a proxy with an unverified implementation; eth_call is
        // the only probe, and the bytecode does not contain either selector.
        function getCctpDomain(uint256 destinationChainId) external view returns (uint32);
    }

    /// The core registry (`module.registry()`) that holds bridge encoders.
    ///
    /// Verified on Base: module 0x5afe…b002 → registry
    /// 0xd91567aF9d2792Cb5760a73a7Fc526c7C634876a, whose
    /// `getBridgeEncoder(2)` and `(3)` both return configured encoders. This
    /// getter was previously declared on `IMakinaXModule`, where it does
    /// not exist — the resulting revert was read as "unset" rather than as
    /// "no such function", so a caller reported a working registry as an
    /// unconfigured module.
    ///
    /// A revert HERE has no documented meaning, so the probe reports it
    /// `unverified` rather than inventing one. That is the lesson of the
    /// previous mistake, not a gap.
    #[sol(rpc)]
    interface IBridgeEncoderRegistry {
        function getBridgeEncoder(uint16 bridgeId) external view returns (address);
    }
}

sol! {
    #[sol(rpc)]
    interface IMakinaXModule {
        // ─── Reads ─────────────────────────────────────────────────────
        // Identity (`MakinaXGovernable.sol`, `WeirollComponent.sol`, makina-x
        // @ 6168e175): the Safe the module serves (set at initialize), the
        // MakinaX service account (`onlyProvider` setter, not the Safe's),
        // and the Weiroll VM (immutable, fixed at deploy).
        function safe() external view returns (address);
        function provider() external view returns (address);
        function weirollVm() external view returns (address);
        function allowedInstrRoot() external view returns (bytes32);
        function accountingCurrency() external view returns (address);
        function maxPositionIncreaseLossBps() external view returns (uint256);
        function maxPositionDecreaseLossBps() external view returns (uint256);
        function maxSwapLossBps() external view returns (uint256);
        // `IMakinaXGovernable.operatingMode()` @ 6168e175 returns the
        // `OperatingMode` enum, ABI-encoded as uint8: 0=OPEN, 1=FENCED,
        // 2=WALLED.
        function operatingMode() external view returns (uint8);
        // NOT in makina-x @ 6168e175. Kept only because a consumer's own
        // getter-classification test pins it, for a fallback it runs against
        // older deployments; nothing in this crate calls it.
        function lockdownMode() external view returns (bool);
        function paused() external view returns (bool);
        function suspendedByProvider() external view returns (bool);
        function isOperator(address) external view returns (bool);
        // Cooldowns (seconds) and the swap fee rate (1e18 = 100%): all in
        // makina-x @ 6168e175 — `IWeirollComponent.instrCooldownDuration`,
        // `ISwapComponent.swapCooldownDuration`/`swapFeeRate`,
        // `IBridgeComponent.bridgeCooldownDuration`. This crate reads them
        // strictly (`reads::module_state`); a caller reading deployments
        // that predate them must treat them as best-effort.
        function instrCooldownDuration() external view returns (uint256);
        function swapCooldownDuration() external view returns (uint256);
        function bridgeCooldownDuration() external view returns (uint256);
        function swapFeeRate() external view returns (uint256);
        function getSwapperTargets(uint16 swapperId) external view returns (address approvalTarget, address executionTarget);

        // ─── Bridge surface ────────────────────────────────────────────────
        // These two ARE on the module (present in the verified ABI of
        // 0x5afe…b002 on Base, alongside addRecipient and
        // sendOutBridgeTransfer).
        //
        // `getBridgeEncoder` is NOT, and used to be declared here. It lives on
        // the ENCODER REGISTRY, reached via `registry()` — see
        // `IBridgeEncoderRegistry` below. Calling it on the module reverts
        // because the function does not exist, which an earlier note in this
        // file misread as "reverts instead of returning address(0) for an
        // unset bridgeId" and wrote up as intended degradation. That
        // misdiagnosis then propagated into the probe as "a bare revert means
        // UNSET", which would have reported live, configured encoders as
        // absent. Base's registry answers ids 2 and 3 with real encoders.
        function getMaxBridgeLossBps(uint16 bridgeId) external view returns (uint256);
        function isWhitelistedRecipient(uint256 destinationChainId, address recipient) external view returns (bool);
        /// The core registry this module resolves shared components from —
        /// bridge encoders among them.
        function registry() external view returns (address);

        // ─── Operator entrypoints ──────────────────────────────────────
        // Field order on Instruction matches IWeirollComponent.sol:38-49.
        // InstructionType enum: 0=MANAGEMENT, 1=ACCOUNTING, 2=HARVEST, 3=FLASHLOAN_MANAGEMENT.
        struct Instruction {
            uint256 positionId;
            bool isDebt;
            uint256 groupId;
            uint8 instructionType;
            address[] affectedTokens;
            address[] positionTokens;
            bytes32[] commands;
            bytes[] state;
            uint128 stateBitmap;
            bytes32[] merkleProof;
        }

        // FIELD ORDER IS ABI: matches ISwapComponent.sol EXACTLY (`data` is
        // the SECOND field). An earlier binding had `data` last — every
        // module.swap encoding was garbage; caught by the first real swap
        // execution in a fork end-to-end test.
        struct SwapOrder {
            uint16 swapperId;
            bytes data;
            address inputToken;
            address outputToken;
            uint256 inputAmount;
            uint256 minOutputAmount;
        }

        // Matches IBridgeComponent.sol exactly (bridging unused until 1b+).
        struct BridgeOrder {
            uint16 bridgeId;
            uint256 destinationChainId;
            address recipient;
            address inputToken;
            uint256 inputAmount;
            uint256 minOutputAmount;
            bytes extraData;
        }

        function accountForPosition(Instruction calldata instruction) external returns (uint256 value);
        // groupIds is unused by MakinaXModule; exists for Makina Core interface compat.
        function accountForPositionBatch(Instruction[] calldata instructions, uint256[] calldata groupIds)
            external returns (uint256[] memory values);
        function managePosition(Instruction calldata mgmt, Instruction calldata acct)
            external returns (uint256 value, int256 change);
        function harvest(Instruction calldata instruction, SwapOrder[] calldata swapOrders) external;
        function swap(SwapOrder calldata order) external;
        function sendOutBridgeTransfer(BridgeOrder calldata order) external;

        // ─── Events the ledger cares about ─────────────────────────────
        event PositionManaged(
            bool indexed withValuation,
            bool indexed withAccountingChecks,
            uint256 indexed positionId,
            uint256 value
        );

        event AllowedInstrRootChanged(bytes32 indexed oldRoot, bytes32 indexed newRoot);
        event Swap(uint16 indexed swapperId, address indexed inputToken, address indexed outputToken, uint256 inputAmount, uint256 outputAmount);
        event Paused(address indexed guardian);
        event Unpaused(address indexed guardian);
        event Suspended();
        event Unsuspended();
        event OperatorAdded(address indexed newOperator);
        event OperatorRemoved(address indexed operator);
        // Not observed live yet (addGuardian/removeGuardian emit unknown);
        // scanned best-effort — config candidates + isGuardian() remain the
        // authoritative guardian source.
        event GuardianAdded(address indexed guardian);
        event GuardianRemoved(address indexed guardian);
        event SwapperTargetsSet(uint16 indexed swapperId, address approvalTarget, address executionTarget);
        // OracleRegistry component (inherited by the module — emitted from the
        // module address). The feed-route mapping is private with per-token
        // getters only, so these events are the ONLY way to enumerate
        // feed-registered tokens.
        event FeedRouteRegistered(address indexed token, address indexed feed1, address indexed feed2);
        event FeedRouteCleared(address indexed token);

        // ─── Owner-only admin ──────────────────────────────────────────
        // OWNER_SURFACE_START
        // Every function in this section is restricted to the Safe in the
        // real contract, verified against makina-x @ 6168e175
        // (`onlySafe` in `MakinaXGovernable.sol` / `MakinaXModule.sol`),
        // except `pause`/`unpause` (see the note beside them). This is the
        // section `owner::encode::owner_function_names()` is derived from;
        // a test in that module cross-checks against it in BOTH directions
        // (every non-view function declared here must be named there, and
        // vice versa) — so this section's membership IS the encoder's
        // contract, not just its inspiration. Bounded by explicit start/end
        // marker comments (see below and at the close of this section) so
        // the check cannot silently grow to swallow unrelated later
        // sections, or shrink to nothing if this section is ever split up.
        //
        // `setFeedRoute` is a NAMED EXCEPTION: it is Safe-only too (see its
        // own "Safe-only config" comment under `IOracleRegistry` above) but
        // stays declared there, next to its sibling read `IOracleRegistry`
        // functions, rather than being duplicated here.
        //
        // Installs/rotates the instruction whitelist. The module emits
        // `AllowedInstrRootChanged` when this runs (declared above, among
        // the events), but the interface used to declare only that event,
        // never the function itself — every consumer that builds a
        // Safe-signed batch to install or rotate this root calls it by
        // this exact signature, so completing the declaration matches real
        // usage rather than guessing at one. Directly confirmed against
        // `MakinaXModule.sol`'s `onlySafe setAllowedInstrRoot`.
        function setAllowedInstrRoot(bytes32 newRoot) external;

        // Governance (`MakinaXGovernable.sol`). `setProvider` and
        // `suspend`/`unsuspend` are `onlyProvider` (the MakinaX service
        // account, not the Safe) and are deliberately NOT declared as owner
        // functions.
        function addOperator(address newOperator) external;
        function removeOperator(address operator) external;
        function addGuardian(address newGuardian) external;
        function removeGuardian(address guardian) external;
        function isGuardian(address account) external view returns (bool);
        function setOperatingMode(uint8 newMode) external;
        // Both `pause` and `unpause` are `onlyGuardian` in the real
        // contract — NOT split guardian/Safe as an earlier version of this
        // comment claimed (that claim was never checked against the
        // contract). But the Safe is added as a guardian at module
        // initialization (`__MakinaXGovernable_init` calls `_addGuardian`
        // on the Safe), and `removeGuardian` refuses `guardian == safe`
        // (`Errors.ProtectedGuardian`) — the Safe can never stop being a
        // guardian, so it can always call both. Included in the owner
        // surface for that reason, not because either selector is
        // Safe-exclusive on its own.
        function pause() external;
        function unpause() external;

        // Oracle (`OracleRegistry` component; `setFeedRoute` is already
        // declared above under `IOracleRegistry`'s own interface — the
        // module inherits it, and it is `onlySafe` there too).
        function clearFeedRoute(address token) external;
        function setFeedStaleThreshold(address feed, uint256 newThreshold) external;

        // Weiroll / position risk (`MakinaXModule.sol`).
        function setAccountingCurrency(address newAccountingCurrency) external;
        function setMaxPositionIncreaseLossBps(uint256 newMaxPositionIncreaseLossBps) external;
        function setMaxPositionDecreaseLossBps(uint256 newMaxPositionDecreaseLossBps) external;
        function setInstrCooldownDuration(uint256 newInstrCooldownDuration) external;

        // Swap (`MakinaXModule.sol`). `setSwapFeeRate` is `onlyProvider`
        // in the real contract and is deliberately NOT declared as an
        // owner function.
        function setMaxSwapLossBps(uint256 newMaxSwapLossBps) external;
        function setSwapCooldownDuration(uint256 newSwapCooldownDuration) external;
        function setSwapperTargets(uint16 swapperId, address approvalTarget, address executionTarget) external;

        // Bridge (`MakinaXModule.sol` / `BridgeComponent.sol`; calldata a
        // consumer emits as owner instructions for the Safe to sign — this
        // crate NEVER calls these, the Safe does, in the Makina Safe app).
        function addRecipient(uint256 destinationChainId, address recipient) external;
        function removeRecipient(uint256 destinationChainId, address recipient) external;
        function setMaxBridgeLossBps(uint16 bridgeId, uint256 maxLossBps) external;
        function setBridgeCooldownDuration(uint256 newBridgeCooldownDuration) external;

        // Sweep (`MakinaXModule.sol`) — asset recovery, not module
        // configuration, but still a Safe-only external function on the
        // real contract (makina-x @ 6168e175), so it belongs in the
        // complete owner surface derived here from that contract's access
        // modifiers.
        function sweepERC20(address token) external;
        function sweepERC721(address token, uint256 tokenId) external;
        function sweepERC6909(address token, uint256 id) external;
        function sweepNative() external;
        // OWNER_SURFACE_END
    }
}
