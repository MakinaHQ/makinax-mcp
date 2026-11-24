//! Warm anvil fork lifecycle for execute-time facts sims.
//!
//! The warm anvil fork is a facts-sim backend that must work with nothing
//! but an RPC URL ([tenderly] is an optional accelerator). Design:
//!
//! - one fork process per chain, spawned lazily with tuned flags
//!   (`[fork]` config: upstream timeout, retries, backoff, rate limit —
//!   the stock spawn's 10s ready-wait + no flags times out on real
//!   gateways),
//! - kept alive for the process lifetime and REUSED; before each facts-sim
//!   the fork is health-pinged (respawn on crash) and re-anchored to a fresh
//!   block with `anvil_reset` (incremental refetch ≪ cold genesis burst),
//! - `[dev_rpc_urls.<chain>]` still attaches to an external endpoint instead
//!   (never spawned, never reset — externally managed),
//! - every failure is a typed `Error::ForkBackend` naming the chain, the
//!   phase (binary-resolution | spawn | health-ping | reset) and the fork
//!   source — never a bare timeout.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant},
};

use alloy::node_bindings::{Anvil, AnvilInstance};
use serde::{Deserialize, Serialize};

use crate::{
    config::{ForkConfig, MakinaxConfig},
    error::{Error, Result},
};

// What a simulation is FOR: a dev target (a fork or attached dev endpoint
// that may carry dev state) or the live chain. A mainnet-targeted request is
// never answered from a dev target (see `ForkManager::fresh_endpoint`).
//
// A plain comment, not a doc comment, on purpose: with the `schemars`
// feature a doc comment becomes the schema's `description`, and a consumer
// that exposes this enum in a tool schema must see the same schema it always
// has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum ExecTarget {
    Dev,
    Mainnet,
}

impl ExecTarget {
    pub fn label(&self) -> &'static str {
        match self {
            ExecTarget::Dev => "dev",
            ExecTarget::Mainnet => "mainnet",
        }
    }
}

/// Process-level choices a consumer makes about the anvil children this
/// manager spawns — things no config file can express, because they belong
/// to the consumer's own build and conventions rather than to a deployment.
#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    /// Env var names that may carry a secret (e.g. a consumer's canonical
    /// keystore-passphrase names) and must never reach a spawned anvil. The
    /// configured signer's own `passphrase_env` is always added on top (see
    /// [`anvil_env_overrides`]).
    pub scrub_env: Vec<String>,
    /// Candidate locations of a bundled Base-capable anvil, RELATIVE to the
    /// directory of the running executable (symlinks resolved), tried in
    /// order when `fork.base_anvil_path` is unset — so a consumer that ships
    /// its own Base anvil beside its binary finds it without configuration.
    pub bundled_base_anvil: Vec<PathBuf>,
    /// Reap orphaned forks when the manager is constructed (default
    /// `false`).
    ///
    /// **SIDE EFFECT, MACHINE-WIDE:** when `true`, constructing the manager
    /// runs [`sweep_orphaned_forks`], which lists every process on the host
    /// and sends SIGTERM to each anvil whose argv carries
    /// [`OUR_ANVIL_MARKER`] and whose parent is gone (`ppid == 1`), whoever
    /// started it. That is right for an application that owns its host's
    /// forks (it covers the exits no destructor sees: a killed test run, a
    /// SIGKILL), and wrong as a silent default of a library constructor.
    pub sweep_orphans_on_start: bool,
}

#[derive(Default)]
pub struct ForkManager {
    spawned: Mutex<HashMap<String, ForkHandle>>,
    stats: Mutex<HashMap<String, ForkStats>>,
    readiness_cache: Mutex<HashMap<String, ForkReadiness>>,
    options: SpawnOptions,
}

/// Result of an active fork-readiness probe: a send must never be the
/// discovery mechanism for infra rot — a health check reports this.
#[derive(Debug, Clone, Serialize)]
pub struct ForkReadiness {
    /// "pass" | "fail" | "off"
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_unix: Option<i64>,
    /// True when served from the probe cache (TTL below) instead of a live probe.
    pub cached: bool,
}

/// What a failed readiness probe SAYS, as a pure function so it can be tested.
///
/// `READINESS_TIMEOUT_MS` is fixed and caps `fork.timeout_ms` in the probe, so
/// naming that setting as the remedy sends a reader to a knob that cannot move
/// this outcome.
pub fn readiness_error(endpoint: &str, cause: &str) -> String {
    let probe_ms = READINESS_TIMEOUT_MS.saturating_sub(READINESS_CHILD_HEADROOM_MS);
    let write_ms = ForkConfig::default_timeout_ms();
    let write_retries = ForkConfig::default_retries();
    format!(
        "the fork readiness probe did not finish within its own fixed {READINESS_TIMEOUT_MS}ms \
         budget: {cause}. That budget is INDEPENDENT of `[fork] timeout_ms` and caps it, so \
         raising that setting does not change this result. THIS IS NOT A PREDICTION THAT A WRITE \
         WILL FAIL: the probe allows {probe_ms}ms with no retries, while a simulation gets \
         `[fork] timeout_ms` ({write_ms}ms by default) and `retries` ({write_retries}), so a fork \
         that misses this budget can still simulate. It is not a verdict on {endpoint} either — \
         that endpoint may be healthy and merely slower than a health check is allowed to wait. \
         If it is simply slow, `[fork] probe = \"off\"` is the setting for that."
    )
}

/// The probe failed for a reason of ITS OWN, inside its budget — so the budget
/// is not the cause and saying it is sends the reader to a knob that would
/// change nothing. Testing measured this as the common case: twice in three
/// runs on a healthy machine against a healthy public endpoint, and once on a
/// failure that came back in ONE millisecond while the text claimed 1500ms had
/// been exhausted.
pub fn readiness_failed(endpoint: &str, cause: &str, took_ms: u64) -> String {
    let write_ms = ForkConfig::default_timeout_ms();
    let write_retries = ForkConfig::default_retries();
    let probe_ms = READINESS_TIMEOUT_MS.saturating_sub(READINESS_CHILD_HEADROOM_MS);
    format!(
        "the fork readiness probe failed after {took_ms}ms, INSIDE its {READINESS_TIMEOUT_MS}ms \
         budget, so the budget is not the cause: {cause}. IGNORE ANY ADVICE ABOVE TO RAISE \
         `[fork] timeout_ms`: this probe caps it at {probe_ms}ms with no retries whatever that \
         setting says, so the cap it hit is the probe's, not yours. THIS IS NOT A PREDICTION THAT \
         A WRITE WILL FAIL: a simulation gets the full `[fork] timeout_ms` ({write_ms}ms by \
         default) and `retries` ({write_retries}), so a fork that fails this probe can still \
         simulate. It is not a verdict on {endpoint} either — read it as not measured rather than \
         as broken."
    )
}

/// The probe caps the CHILD's own deadline this far below its outer budget, so
/// work left on the blocking pool ends shortly after the outer timeout.
pub const READINESS_CHILD_HEADROOM_MS: u64 = 250;

/// Probe results are cached per chain so a health check stays fast.
pub(crate) const READINESS_TTL_S: i64 = 300;
/// A FAILURE is cached for far less. Five minutes of "fast" on a passing probe
/// is a saving; five minutes on a failing one means the person who fixes their
/// endpoint and presses Check again is told the same thing for five minutes,
/// and it is the wrong thing often enough (see `readiness_failed`) that
/// repeating it is the worse trade. A failing probe costs at most one budget.
pub(crate) const READINESS_FAIL_TTL_S: i64 = 15;

// A failing probe must not be remembered for as long as a passing one: the
// person who fixes their endpoint and checks again deserves a fresh answer,
// and the answer is wrong often enough to matter. Both sides are constants, so
// this belongs to the BUILD rather than to a test run -- it was an
// `assert!` in a test, which could never vary and never fail.
const _: () = assert!(READINESS_FAIL_TTL_S < READINESS_TTL_S);
/// A health check is a consumer's first diagnostic and must remain responsive
/// even when a just-written fork RPC is unreachable.  This is deliberately
/// independent of the normal simulation budget (`fork.timeout_ms`).
pub const READINESS_TIMEOUT_MS: u64 = 1_500;

struct ForkHandle {
    url: String,
    /// The upstream this handle forked from. Kept for diagnosis when a warm
    /// fork misbehaves; read by nothing today, so it is explicitly allowed
    /// rather than silently dead.
    #[allow(dead_code)]
    source: String,
    instance: Option<AnvilInstance>, // killed on drop (respawn / process exit)
}

/// Observability for a health check: how the fork backend has been behaving.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ForkStats {
    pub last_spawn_ms: Option<u64>,
    pub last_reset_ms: Option<u64>,
    pub respawns: u32,
    /// Dev-targeted executions broadcast to this fork. Once > 0 the fork is a
    /// STATEFUL dev environment and is no longer re-anchored (an anvil_reset
    /// would wipe the executed state facts sims must see).
    pub dev_writes: u32,
    /// Simulations that fell back OFF the fork after an infrastructure failure
    /// (rpc_error, "failed to get storage", 429, spawn/timeout). The fork is
    /// the flakiest dependency; a health check surfaces this so operators can
    /// see how often the fork is flaking and fallback is kicking in.
    pub fallbacks: u32,
}

/// The argv marker that identifies an anvil this crate spawned.
///
/// `--fork-retry-backoff` is emitted only by [`anvil_fork_args`]. Nobody types
/// it by hand, so an anvil carrying it is ours — which is what makes reaping
/// safe: a developer's own `anvil --fork-url …` never matches.
pub const OUR_ANVIL_MARKER: &str = "--fork-retry-backoff";

/// Whether a running process is an ORPHANED fork this crate leaked.
///
/// All three conditions, and each is doing work:
/// - it is an anvil (not, say, a shell whose command line mentions the flag —
///   a grep for the marker alone matches the very command doing the grepping);
/// - it carries our marker, so it is ours rather than the developer's;
/// - its parent is gone (`ppid == 1`), so nothing is going to reap it.
///
/// A fork with a LIVE parent is somebody's working fork and is never touched.
pub(crate) fn is_orphaned_fork_of_ours(command: &str, ppid: i32) -> bool {
    command.contains("anvil") && command.contains(OUR_ANVIL_MARKER) && ppid == 1
}

/// Reap anvil forks this crate leaked in earlier runs.
///
/// `shutdown()` handles every path that ENDS deliberately. This handles the
/// one that does not: a process killed outright — a cancelled `cargo test`, a
/// SIGKILL — runs no destructors and no shutdown, and its anvil children
/// reparent to init and stay forever. Measured on a developer
/// machine: **138 orphans**, oldest eight days, forked from localhost RPCs
/// that had long since died. They are not merely untidy; that much CPU and
/// memory contention shows up as timeouts and flaky readiness probes, which
/// get diagnosed as bugs somewhere else entirely.
///
/// Called on startup because that is the moment we know no run of ours is in
/// flight that we did not start.
///
/// Returns how many were reaped, and never touches anything that fails
/// [`is_orphaned_fork_of_ours`].
#[cfg(unix)]
pub fn sweep_orphaned_forks() -> usize {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-eo", "pid,ppid,command"])
        .output()
    else {
        return 0;
    };
    let listing = String::from_utf8_lossy(&out.stdout);
    let mut reaped = 0usize;
    for line in listing.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let (Some(pid), Some(ppid)) = (cols.next(), cols.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<i32>(), ppid.parse::<i32>()) else {
            continue;
        };
        // Never signal ourselves, whatever the command line says.
        if pid == std::process::id() as i32 {
            continue;
        }
        if !is_orphaned_fork_of_ours(line, ppid) {
            continue;
        }
        // SIGTERM only. A fork that ignores it is left alone rather than
        // escalated: this runs unattended at startup, and the cost of being
        // wrong about one process is worse than the cost of leaving it.
        if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
            reaped += 1;
        }
    }
    if reaped > 0 {
        tracing::info!(reaped, "reaped orphaned anvil forks left by an earlier run");
    }
    reaped
}

#[cfg(not(unix))]
pub fn sweep_orphaned_forks() -> usize {
    0
}

/// The exact extra CLI args a spawned anvil fork gets — extracted so a test
/// can PROVE the `[fork]` knobs named in error remediations are real (a
/// fictional knob in an error message is worse than none).
///
/// `--no-storage-caching` is always passed. Without it anvil persists fork
/// storage under `~/.foundry/cache/rpc/<chain>/<block>/`, keyed by chain and
/// block but not by upstream, and a later fork of the same block reloads it.
/// A fork from one upstream must never reuse storage cached from another, so
/// every fork reads its state from its own source.
pub fn anvil_fork_args(fork: &ForkConfig, hardfork: &str) -> Vec<String> {
    let mut args = vec![
        "--no-storage-caching".to_string(),
        "--hardfork".to_string(),
        hardfork.to_string(),
        "--timeout".to_string(),
        fork.timeout_ms.to_string(),
        "--retries".to_string(),
        fork.retries.to_string(),
        "--fork-retry-backoff".to_string(),
        fork.retry_backoff_ms.to_string(),
    ];
    if let Some(cups) = fork.compute_units_per_second {
        args.push("--compute-units-per-second".to_string());
        args.push(cups.to_string());
    }
    args
}

/// THE constructor for `Error::ForkBackend` — so redacting here covers both
/// the error's Display (which renders `fork_source`) and any structured
/// payload a caller builds from the error's fields verbatim. Fork sources are
/// Tenderly/Alchemy gateway URLs with the key in the path.
///
/// KNOWN REMAINING EXPOSURE: anvil takes `--fork-url` on argv, so the keyed
/// URL is visible to `ps` for the fork's lifetime. alloy's `Anvil` builder
/// offers no env/stdin path for it; fixing that means spawning anvil by hand.
/// Local-process only, and not something this layer can close today.
fn fork_err(chain: &str, phase: &str, source: &str, detail: impl std::fmt::Display) -> Error {
    Error::ForkBackend {
        chain: chain.to_string(),
        phase: phase.to_string(),
        fork_source: crate::util::redact::url(source),
        detail: crate::util::redact::url(&detail.to_string()),
    }
}

/// Resolve the anvil binary: explicit config, then $PATH, then
/// ~/.foundry/bin/anvil.
pub fn resolve_anvil_path(fork: &ForkConfig) -> Result<PathBuf> {
    if let Some(p) = &fork.anvil_path {
        if p.exists() {
            return Ok(p.clone());
        }
        return Err(fork_err(
            "*",
            "binary-resolution",
            "-",
            format!("fork.anvil_path {} does not exist", p.display()),
        ));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("anvil");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".foundry/bin/anvil");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(fork_err(
        "*",
        "binary-resolution",
        "-",
        "anvil not found on $PATH or at ~/.foundry/bin/anvil — install foundry (https://getfoundry.sh) or set fork.anvil_path",
    ))
}

/// Base's native B20 tokens require the Base EVM, even though eth_getCode
/// returns the one-byte 0xef marker. Stock Anvil treats it as an opcode.
/// Never silently simulate Base against Ethereum-only execution rules.
fn chain_backend(
    chain: &str,
    fork: &ForkConfig,
    options: &SpawnOptions,
) -> Result<(PathBuf, Vec<String>)> {
    let mut args = anvil_fork_args(fork, &fork.hardfork);
    if crate::chains::network_by_key(chain).map(|c| c.chain_id) != Some(8453) {
        return Ok((resolve_anvil_path(fork)?, args));
    }
    let explicit = fork.base_anvil_path.as_ref();
    let path = if let Some(path) = explicit {
        path.clone()
    } else {
        bundled_base_anvil(std::env::current_exe().ok().as_deref(), &options.bundled_base_anvil).or_else(|| std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join("base-anvil"))
            .chain(dirs::home_dir().map(|home| home.join(".foundry/bin/base-anvil")))
            .find(|path| path.is_file()))
            // Preserve explicitly configured custom binaries; --base below
            // makes a stock binary fail clearly rather than return a false revert.
            .or_else(|| fork.anvil_path.clone())
            .ok_or_else(|| fork_err(chain, "binary-resolution", "-",
                "Base simulation requires Base Anvil with native B20 support. Install the matching base/base-anvil release alongside stock Foundry, or set fork.base_anvil_path to its raw anvil binary. See https://github.com/base/base-anvil/tree/base-anvil-fork/docs/base.md"))?
    };
    if !path.is_file() {
        return Err(fork_err(
            chain,
            "binary-resolution",
            "-",
            format!("Base Anvil binary {} does not exist", path.display()),
        ));
    }
    if explicit.is_some()
        || fork.anvil_path.as_ref() == Some(&path)
        || path.file_name().and_then(|name| name.to_str()) != Some("base-anvil")
    {
        args.push("--base".into());
    }
    Ok((path, args))
}

/// The first of `candidates` (relative to the running executable's own
/// directory) that is a file. Package managers and CLI aliases may reach the
/// executable through a symlink, so it is resolved to the installed
/// directory first — never the caller's working directory.
pub fn bundled_base_anvil(
    executable: Option<&std::path::Path>,
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    let executable = executable?;
    let executable = executable.canonicalize().ok()?;
    let directory = executable.parent()?;
    candidates
        .iter()
        .map(|candidate| directory.join(candidate))
        .find(|candidate| candidate.is_file())
}

/// The environment a spawned anvil gets ON TOP of this process's own:
/// every secret-bearing name in `options.scrub_env`, plus the configured
/// signer's `passphrase_env`, shadowed with an empty value.
///
/// anvil inherits this process's environment, and `passphrase_env` is a
/// supported way to hand a signer its keystore passphrase — so without this
/// the secret reaches the fork child and (if it is orphaned) outlives this
/// process. alloy's builder can only ADD variables, so each name is shadowed
/// empty: the child sees the name, never the value.
pub fn anvil_env_overrides(options: &SpawnOptions, cfg: &MakinaxConfig) -> Vec<(String, String)> {
    let canonical: Vec<&str> = options.scrub_env.iter().map(String::as_str).collect();
    crate::util::secrets::empty_overrides(
        &canonical,
        crate::util::secrets::configured_passphrase_env(&cfg.signer),
    )
}

/// The upstream a chain's fork reads state from:
/// `[fork.source_rpc_urls.<chain>]`, falling back to `[rpc_urls.<chain>]`.
pub fn fork_source(chain: &str, cfg: &MakinaxConfig) -> Result<String> {
    // anvil forks from ONE upstream URL: prefer an explicit fork source, else
    // the primary rpc_urls endpoint. (Read-path failover across the rpc_urls
    // list is handled by the provider layer; a fork infra failure is for the
    // caller's sim fallback to catch.)
    cfg.fork
        .source_rpc_urls
        .get(chain)
        .cloned()
        .or_else(|| cfg.rpc_urls.get(chain).and_then(|e| e.primary()).map(String::from))
        .ok_or_else(|| {
            Error::Config(format!(
                "no fork.source_rpc_urls.{chain} or rpc_urls.{chain} to fork from, and no dev_rpc_urls.{chain} to attach to"
            ))
        })
}

impl ForkManager {
    /// Construct the manager with default [`SpawnOptions`]: no extra env
    /// scrub, no bundled Base anvil, and NO orphan sweep.
    pub fn new() -> Self {
        Self::with_options(SpawnOptions::default())
    }

    /// Construct the manager with a consumer's own [`SpawnOptions`] for
    /// every anvil it spawns.
    ///
    /// With `options.sweep_orphans_on_start`, this also reaps anything an
    /// earlier run leaked (see that field for the machine-wide side effect).
    /// Construction is the moment to do it: no fork of ours is in flight that
    /// this process did not start. `shutdown()` covers every ending we
    /// control; the sweep covers the ones we do not — a killed test run, a
    /// SIGKILL — where no destructor runs and the children reparent to init.
    pub fn with_options(options: SpawnOptions) -> Self {
        let sweep = options.sweep_orphans_on_start;
        let this = Self {
            options,
            ..Self::default()
        };
        if !sweep {
            return this;
        }
        let reaped = sweep_orphaned_forks();
        if reaped > 0 {
            tracing::info!(
                reaped,
                "an earlier run left anvil forks behind; reaped them before starting"
            );
        }
        this
    }

    /// The spawn options every anvil this manager spawns runs with.
    pub fn options(&self) -> &SpawnOptions {
        &self.options
    }

    /// Dev/fork endpoint for a chain: `[dev_rpc_urls.<chain>]` if configured
    /// (attach), else the warm spawned anvil fork (spawned lazily, cached).
    pub async fn endpoint(&self, chain: &str, cfg: &MakinaxConfig) -> Result<String> {
        if let Some(url) = cfg.dev_rpc_urls.get(chain) {
            return Ok(url.clone());
        }
        if let Some(h) = self.spawned.lock().unwrap().get(chain) {
            return Ok(h.url.clone());
        }

        // `Anvil::try_spawn` performs synchronous stdout reads while waiting
        // for readiness.  Running it in an async future blocks a Tokio worker,
        // so even an outer `tokio::time::timeout` cannot fire.  Keep the whole
        // blocking startup on the blocking pool and mutate manager state only
        // after it has joined.
        let (handle, spawn_ms) = self.spawn_off_runtime(chain, cfg).await?;
        let url = handle.url.clone();
        let mut spawned = self.spawned.lock().unwrap();
        if let Some(existing) = spawned.get(chain) {
            // A concurrent caller won the cold-start race. Dropping our handle
            // terminates its duplicate child; return the canonical warm fork.
            return Ok(existing.url.clone());
        }
        spawned.insert(chain.to_string(), handle);
        self.stats
            .lock()
            .unwrap()
            .entry(chain.to_string())
            .or_default()
            .last_spawn_ms = Some(spawn_ms);
        Ok(url)
    }

    /// Endpoint for a NEW facts-sim: health-pings the warm fork (respawning a
    /// crashed one) and re-anchors it to a fresh upstream block via
    /// `anvil_reset`. Attached `dev_rpc_urls` endpoints are returned as-is —
    /// they are externally managed.
    pub async fn fresh_endpoint(
        &self,
        chain: &str,
        cfg: &MakinaxConfig,
        target: ExecTarget,
    ) -> Result<String> {
        // STRUCTURAL: a mainnet-targeted request can never receive a dev
        // endpoint, whatever the caller. A per-caller check rots the moment
        // someone adds another caller.
        //
        // Both conditions make this fork a dev target rather than a mirror of
        // the live chain: an attached `dev_rpc_urls` endpoint is externally
        // managed and arbitrary, and a warm fork that has taken dev executions
        // stops re-anchoring (below), so it is frozen with dev state applied.
        if matches!(target, ExecTarget::Mainnet) {
            if cfg.dev_rpc_urls.contains_key(chain) {
                return Err(Error::MainnetOnDevEndpoint {
                    chain: chain.to_string(),
                });
            }
            let dev_writes = self.dev_writes(chain);
            if dev_writes > 0 {
                return Err(Error::MainnetOnDevFork {
                    chain: chain.to_string(),
                    dev_writes,
                });
            }
        }
        if let Some(url) = cfg.dev_rpc_urls.get(chain) {
            return Ok(url.clone());
        }
        let url = self.endpoint(chain, cfg).await?;
        let source = fork_source(chain, cfg)?;

        // A fork that has received dev executions is a stateful dev target:
        // facts sims must observe that accumulated state, not a fresh anchor.
        let is_dev_env = self
            .stats
            .lock()
            .unwrap()
            .get(chain)
            .map(|s| s.dev_writes > 0)
            .unwrap_or(false);

        let timeout_ms = cfg.fork.timeout_ms;
        if !self.ping(&url).await {
            tracing::warn!(chain, "warm fork unresponsive; respawning");
            // Drop the dead instance first (kills the child), then spawn.  The
            // startup remains off-runtime on the respawn path too.
            self.spawned.lock().unwrap().remove(chain);
            let (handle, spawn_ms) = self.spawn_off_runtime(chain, cfg).await?;
            let new_url = handle.url.clone();
            self.spawned
                .lock()
                .unwrap()
                .insert(chain.to_string(), handle);
            self.stats
                .lock()
                .unwrap()
                .entry(chain.to_string())
                .or_default()
                .respawns += 1;
            self.stats
                .lock()
                .unwrap()
                .entry(chain.to_string())
                .or_default()
                .last_spawn_ms = Some(spawn_ms);
            self.reset(chain, &new_url, &source, timeout_ms).await?;
            return Ok(new_url);
        }

        if !is_dev_env {
            self.reset(chain, &url, &source, timeout_ms).await?;
        } else {
            // `reset` is what clears the head floor,
            // and this branch skips it. A dev fork restarted lower on a chain
            // key the process has already seen a higher block on would then be
            // refused for the rest of the process -- the one legitimate step
            // backwards, unclearable. Handing out a fork endpoint at all is
            // enough to mean the topology may have moved.
            crate::chain::reset_head_floor(chain);
        }
        Ok(url)
    }

    /// Dev executions recorded against this chain's warm fork. Non-zero means
    /// the fork no longer re-anchors (see `fresh_endpoint`), so it is not a
    /// faithful mirror of the live chain — a mainnet-targeted simulation
    /// refuses on it rather than evaluating against dev state.
    pub fn dev_writes(&self, chain: &str) -> u32 {
        self.stats
            .lock()
            .unwrap()
            .get(chain)
            .map(|s| s.dev_writes)
            .unwrap_or(0)
    }

    /// Record a dev-targeted execution broadcast to this chain's fork — from
    /// now on the fork keeps its state (no more anvil_reset re-anchoring).
    pub fn mark_dev_write(&self, chain: &str) {
        self.stats
            .lock()
            .unwrap()
            .entry(chain.to_string())
            .or_default()
            .dev_writes += 1;
    }

    /// Record a simulation that fell back off the fork after an infra failure
    /// (the caller's sim fallback). Surfaced by a health check as the fork's
    /// flakiness signal.
    pub fn record_fallback(&self, chain: &str) {
        self.stats
            .lock()
            .unwrap()
            .entry(chain.to_string())
            .or_default()
            .fallbacks += 1;
    }

    /// Kill the warm fork's PROCESS but keep the stale handle cached —
    /// simulates a crash for lifecycle tests. Hidden: not part of the API.
    #[doc(hidden)]
    pub fn simulate_crash(&self, chain: &str) {
        if let Some(h) = self.spawned.lock().unwrap().get_mut(chain) {
            h.instance = None; // drop kills the child; URL stays cached
        }
    }

    /// The OS pids of every anvil child THIS manager currently holds. A leak
    /// test asserts on exactly these — a global `ps`
    /// scan cannot tell this manager's anvils from a sibling test's in the
    /// same process, and a leak detector that cannot identify its own
    /// subject eventually certifies the bug it was written for.
    pub fn child_pids(&self) -> Vec<u32> {
        self.spawned
            .lock()
            .unwrap()
            .values()
            .filter_map(|h| h.instance.as_ref().map(|i| i.child().id()))
            .collect()
    }

    /// Kill every warm fork this manager spawned, synchronously.
    ///
    /// **`std::process::exit` does not run destructors**, so a process that
    /// exits that way never runs `ForkHandle`'s `Drop`, and each spawned anvil
    /// outlives it — one orphan per chain per restart (239 were once purged
    /// from a live host after a day of restarts). Likewise, a manager moved
    /// into a detached task is only `abort()`ed on an orderly return, and
    /// abort merely schedules cancellation.
    ///
    /// Deliberately not a `Drop` impl: on the paths that matter the drop is
    /// exactly what does not happen. Every caller that can end the process
    /// has to reap here, on purpose. It was never "one child per successful
    /// sim" — the warm fork is one cached process per chain by design, and
    /// repeated sims reuse it; the leak was at the exit, not in the loop.
    ///
    /// A SIGKILL still orphans them; nothing in this process can prevent that
    /// ([`sweep_orphaned_forks`] reaps those on the next start).
    pub fn shutdown(&self) {
        let killed: Vec<String> = {
            let mut spawned = self.spawned.lock().unwrap();
            let names: Vec<String> = spawned.keys().cloned().collect();
            spawned.clear(); // dropping each handle kills its child
            names
        };
        if !killed.is_empty() {
            tracing::info!(chains = ?killed, "killed warm anvil forks on shutdown");
        }
    }

    pub fn stats(&self, chain: &str) -> Option<ForkStats> {
        self.stats.lock().unwrap().get(chain).cloned()
    }

    /// Active fork-readiness probe: spawn-or-reset the chain's warm fork and
    /// report pass/fail + latency. Cached for READINESS_TTL_S so repeated
    /// health checks stay fast; `[fork] probe = "off"` skips it
    /// entirely (constrained setups). First probe on a cold fork pays the
    /// spawn (~2s on a healthy source, bounded by `[fork] timeout_ms` on a
    /// hostile one) — the same cost the first write would have paid blind.
    pub async fn readiness(&self, chain: &str, cfg: &MakinaxConfig) -> ForkReadiness {
        if cfg.fork.probe == "off" {
            return ForkReadiness {
                status: "off".into(),
                latency_ms: None,
                error: None,
                checked_unix: None,
                cached: false,
            };
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Some(c) = self.readiness_cache.lock().unwrap().get(chain)
            && c.checked_unix
                .map(|t| {
                    let ttl = if c.status == "pass" {
                        READINESS_TTL_S
                    } else {
                        READINESS_FAIL_TTL_S
                    };
                    now - t < ttl
                })
                .unwrap_or(false)
        {
            let mut c = c.clone();
            c.cached = true;
            return c;
        }
        let started = Instant::now();
        let endpoint = cfg
            .dev_rpc_urls
            .get(chain)
            .cloned()
            .or_else(|| fork_source(chain, cfg).ok())
            .map(|url| crate::util::redact::url(&url))
            .unwrap_or_else(|| format!("chain `{chain}`"));
        // A health probe has a latency contract distinct from a real
        // simulation. Cap the child process' own deadline so work left on the
        // blocking pool also terminates shortly after an outer timeout.
        let mut probe_cfg = cfg.clone();
        probe_cfg.fork.timeout_ms = probe_cfg
            .fork
            .timeout_ms
            .min(READINESS_TIMEOUT_MS.saturating_sub(READINESS_CHILD_HEADROOM_MS));
        probe_cfg.fork.retries = 0;
        // Liveness probe only — it simulates nothing, so it must not be
        // refused on a machine that legitimately runs a dev endpoint.
        let res = tokio::time::timeout(
            Duration::from_millis(READINESS_TIMEOUT_MS),
            self.fresh_endpoint(chain, &probe_cfg, ExecTarget::Dev),
        )
        .await;
        let took_ms = started.elapsed().as_millis() as u64;
        // TWO DIFFERENT FAILURES, and they had one sentence between them. The
        // budget running out is `Err` from the timeout; everything else came
        // back on its own and the budget had nothing to do with it.
        let error = match &res {
            Err(_) => Some(readiness_error(
                &endpoint,
                &format!("probe exceeded {READINESS_TIMEOUT_MS}ms"),
            )),
            Ok(Err(e)) => Some(readiness_failed(&endpoint, &e.to_string(), took_ms)),
            Ok(Ok(_)) => None,
        };
        let r = ForkReadiness {
            status: if error.is_none() { "pass" } else { "fail" }.into(),
            latency_ms: Some(took_ms),
            error,
            checked_unix: Some(now),
            cached: false,
        };
        self.readiness_cache
            .lock()
            .unwrap()
            .insert(chain.to_string(), r.clone());
        r
    }

    async fn spawn_off_runtime(
        &self,
        chain: &str,
        cfg: &MakinaxConfig,
    ) -> Result<(ForkHandle, u64)> {
        let chain = chain.to_string();
        let task_chain = chain.clone();
        let cfg = cfg.clone();
        let options = self.options.clone();
        tokio::task::spawn_blocking(move || Self::spawn_handle(&task_chain, &cfg, &options))
            .await
            .map_err(|e| fork_err(&chain, "spawn-task", "-", e))?
    }

    fn spawn_handle(
        chain: &str,
        cfg: &MakinaxConfig,
        options: &SpawnOptions,
    ) -> Result<(ForkHandle, u64)> {
        let source = fork_source(chain, cfg)?;
        let fork = &cfg.fork;
        let (anvil_path, args) = chain_backend(chain, fork, options)
            .map_err(|e| fork_err(chain, "binary-resolution", &source, e))?;

        tracing::info!(
            chain,
            // Keyed gateway URLs (Tenderly/Alchemy/…) must never be logged in
            // full on a warm-fork spawn.
            source = %crate::util::redact::url(&source),
            timeout_ms = fork.timeout_ms,
            retries = fork.retries,
            "spawning warm anvil fork"
        );
        let started = Instant::now();
        // Secret-bearing env names are shadowed empty in the child (see
        // `anvil_env_overrides`): the child sees the name, never the value.
        let instance = anvil_env_overrides(options, cfg)
            .into_iter()
            .fold(
                Anvil::at(anvil_path)
                    .fork(&source)
                    .timeout(fork.timeout_ms) // ready-wait (stock default is only 10s)
                    .args(args),
                |a, (k, v)| a.env(k, v),
            )
            .try_spawn()
            .map_err(|e| fork_err(chain, "spawn", &source, e))?;
        let spawn_ms = started.elapsed().as_millis() as u64;
        tracing::info!(chain, spawn_ms, "anvil fork ready");

        Ok((
            ForkHandle {
                url: instance.endpoint(),
                source,
                instance: Some(instance),
            },
            spawn_ms,
        ))
    }

    /// True if the fork answers eth_blockNumber promptly.
    async fn ping(&self, url: &str) -> bool {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return false,
        };
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "eth_blockNumber", "params": []});
        matches!(client.post(url).json(&body).send().await, Ok(r) if r.status().is_success())
    }

    /// Re-anchor the warm fork to a fresh upstream block. anvil_reset
    /// triggers an upstream refetch of the new head state — same generous
    /// budget as fork requests.
    async fn reset(&self, chain: &str, url: &str, source: &str, timeout_ms: u64) -> Result<()> {
        // Re-anchoring points this chain key at a different topology
        // whose height can legitimately be LOWER than one already seen on the
        // live chain. That is the one legitimate step backwards, and the head
        // floor has to be told about it or every later read of this chain is
        // refused for describing the past.
        crate::chain::reset_head_floor(chain);
        let started = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .build()
            .map_err(|e| fork_err(chain, "reset", source, e))?;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "anvil_reset",
            "params": [{"forking": {"jsonRpcUrl": source}}],
        });
        let resp = client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| fork_err(chain, "reset", source, e))?;
        let text = resp
            .text()
            .await
            .map_err(|e| fork_err(chain, "reset", source, e))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| fork_err(chain, "reset", source, format!("{e} (body: {text})")))?;
        if v.get("error").is_some() {
            return Err(fork_err(chain, "reset", source, text));
        }
        let reset_ms = started.elapsed().as_millis() as u64;
        self.stats
            .lock()
            .unwrap()
            .entry(chain.to_string())
            .or_default()
            .last_reset_ms = Some(reset_ms);
        tracing::debug!(chain, reset_ms, "anvil fork re-anchored");
        Ok(())
    }
}

#[cfg(test)]
mod readiness_cause {
    use super::*;

    /// Every failure once carried the same sentence — "did not finish
    /// within its own fixed 1500ms budget" — including the ones that came back
    /// in a millisecond. A row that names the wrong cause is worse than a row
    /// that names none, because it sends the reader to a setting that cannot
    /// move the outcome.
    #[test]
    fn a_failure_inside_the_budget_does_not_blame_the_budget() {
        let timed_out = readiness_error(
            "rpc.example",
            &format!("probe exceeded {READINESS_TIMEOUT_MS}ms"),
        );
        assert!(timed_out.contains("did not finish within"), "{timed_out}");

        let quick = readiness_failed("rpc.example", "anvil exited immediately: no such binary", 1);
        assert!(
            !quick.contains("did not finish within"),
            "a failure that returned in 1ms must not claim the budget ran out: {quick}"
        );
        assert!(quick.contains("anvil exited immediately"), "{quick}");
        assert!(
            quick.contains("1ms"),
            "the measured time is the evidence: {quick}"
        );
        assert!(
            quick.contains("not a verdict on rpc.example"),
            "advisory, and it says so: {quick}"
        );
    }
}

#[cfg(test)]
mod tests {
    /// A consumer's bundled Base anvil is found beside the REAL executable
    /// (symlinks resolved), in the consumer's own candidate order.
    #[test]
    fn bundled_base_is_resolved_beside_the_real_executable() {
        let candidates = [
            PathBuf::from("vendor-base-anvil"),
            PathBuf::from("../share/vendor/base-anvil/vendor-base-anvil"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = root.join("consumer");
        std::fs::write(&exe, "exe").unwrap();
        assert!(bundled_base_anvil(Some(&exe), &candidates).is_none());
        assert!(
            bundled_base_anvil(Some(&exe), &[]).is_none(),
            "no candidates: nothing bundled"
        );
        let base = root.join("vendor-base-anvil");
        std::fs::write(&base, "Base binary").unwrap();
        assert_eq!(
            bundled_base_anvil(Some(&exe), &candidates),
            Some(base.clone())
        );
        #[cfg(unix)]
        {
            let alias_dir = tempfile::tempdir().unwrap();
            let alias = alias_dir.path().join("alias");
            std::os::unix::fs::symlink(&exe, &alias).unwrap();
            assert_eq!(bundled_base_anvil(Some(&alias), &candidates), Some(base));
        }
        let nested = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(nested.path().join("bin")).unwrap();
        let exe = nested.path().join("bin/consumer");
        std::fs::write(&exe, "exe").unwrap();
        let payload = nested.path().join("share/vendor/base-anvil");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("vendor-base-anvil"), "Base binary").unwrap();
        assert_eq!(
            bundled_base_anvil(Some(&exe), &candidates)
                .unwrap()
                .canonicalize()
                .unwrap(),
            payload.join("vendor-base-anvil").canonicalize().unwrap()
        );
    }

    /// The configured signer's own `passphrase_env` is always shadowed, on
    /// top of the consumer's list; nothing else is touched.
    #[test]
    fn the_spawn_env_shadows_every_secret_name_empty() {
        let cfg = MakinaxConfig {
            signer: crate::config::SignerConfig::Keystore(crate::config::KeystoreSignerConfig {
                path: "/tmp/k.json".into(),
                passphrase_file: None,
                passphrase_env: Some("MY_CONFIGURED_PASS".into()),
                keychain_service: None,
            }),
            ..Default::default()
        };
        let options = SpawnOptions {
            scrub_env: vec!["EXAMPLE_PASS_A".into(), "EXAMPLE_PASS_B".into()],
            ..Default::default()
        };
        assert_eq!(
            anvil_env_overrides(&options, &cfg),
            vec![
                ("EXAMPLE_PASS_A".to_string(), String::new()),
                ("EXAMPLE_PASS_B".to_string(), String::new()),
                ("MY_CONFIGURED_PASS".to_string(), String::new()),
            ]
        );
        assert!(
            anvil_env_overrides(&SpawnOptions::default(), &MakinaxConfig::default()).is_empty(),
            "no names, no signer passphrase env: nothing to shadow"
        );
    }

    #[test]
    fn base_backend_is_chain_specific_and_enables_native_execution() {
        let dir = tempfile::tempdir().unwrap();
        let stock = dir.path().join("stock-anvil");
        let base = dir.path().join("anvil");
        std::fs::write(&stock, "").unwrap();
        std::fs::write(&base, "").unwrap();
        let mut fork = ForkConfig {
            anvil_path: Some(stock.clone()),
            base_anvil_path: Some(base.clone()),
            ..Default::default()
        };
        let options = SpawnOptions::default();
        let (path, args) = chain_backend("base", &fork, &options).unwrap();
        assert_eq!(path, base);
        assert_eq!(
            args.iter().filter(|arg| arg.as_str() == "--base").count(),
            1
        );
        for chain in ["mainnet", "hyperevm"] {
            let (path, args) = chain_backend(chain, &fork, &options).unwrap();
            assert_eq!(path, stock);
            assert!(!args.iter().any(|arg| arg == "--base"));
        }
        let wrapper = dir.path().join("base-anvil");
        std::fs::write(&wrapper, "").unwrap();
        fork.base_anvil_path = Some(wrapper);
        // Explicit paths name raw binaries, including a renamed raw binary.
        assert!(
            chain_backend("base", &fork, &options)
                .unwrap()
                .1
                .contains(&"--base".into())
        );
        fork.base_anvil_path = Some(dir.path().join("missing"));
        assert!(
            chain_backend("base", &fork, &options)
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
    }
    use super::*;

    /// A mainnet-targeted request never receives a dev target, and the
    /// refusal is structured (chain, and the dev-write count for a dirty
    /// fork) so a consumer can render it in its own words.
    #[tokio::test]
    async fn a_mainnet_request_never_gets_a_dev_target() {
        let mut cfg = MakinaxConfig::default();
        cfg.dev_rpc_urls
            .insert("base".into(), "http://127.0.0.1:1/dev".into());
        let m = ForkManager::default();
        match m.fresh_endpoint("base", &cfg, ExecTarget::Mainnet).await {
            Err(Error::MainnetOnDevEndpoint { chain }) => assert_eq!(chain, "base"),
            other => panic!("expected MainnetOnDevEndpoint, got {other:?}"),
        }
        assert_eq!(
            m.fresh_endpoint("base", &cfg, ExecTarget::Dev)
                .await
                .unwrap(),
            "http://127.0.0.1:1/dev",
            "a dev request attaches to the configured dev endpoint"
        );
        m.mark_dev_write("base");
        match m
            .fresh_endpoint("base", &MakinaxConfig::default(), ExecTarget::Mainnet)
            .await
        {
            Err(Error::MainnetOnDevFork { chain, dev_writes }) => {
                assert_eq!((chain.as_str(), dev_writes), ("base", 1))
            }
            other => panic!("expected MainnetOnDevFork, got {other:?}"),
        }
    }

    /// Health-conformance RED/GREEN: a readiness probe against an unreachable
    /// fork source must NOT block the single-threaded runtime.  Before the fix,
    /// `try_spawn` runs synchronously on the runtime thread, so a concurrent
    /// `tokio::time::interval` tick can never fire — the runtime is wedged.
    ///
    /// After the fix the spawn moves to `spawn_blocking`, the readiness probe
    /// gets its own ≤2 s budget, and the ticker advances while the probe is
    /// pending.
    #[test]
    fn health_readiness_does_not_block_current_thread_runtime() {
        // 192.0.2.1 — RFC 5737 TEST-NET-1, guaranteed unreachable / silent.
        let unreachable_rpc = "http://192.0.2.1:1";

        let cfg: MakinaxConfig = toml::from_str(&format!(
            r#"
[rpc_urls]
base = "{unreachable_rpc}"

[fork]
probe = "auto"
timeout_ms = 3000
retries = 0

[modules.demo]
chain = "base"
safe = "0x5aFE00000000000000000000000000000000A001"
module = "0x5AfE00000000000000000000000000000000b001"
rootfile = "local:/tmp/x.toml"
"#
        ))
        .unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mgr = ForkManager::new();
            let ticks = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
            let ticks2 = ticks.clone();

            // A ticker that should fire every 100ms — but on the old code the
            // blocking try_spawn holds the only thread, so it never fires.
            let ticker = tokio::spawn(async move {
                let mut iv = tokio::time::interval(std::time::Duration::from_millis(100));
                iv.tick().await; // first tick is instant
                loop {
                    iv.tick().await;
                    ticks2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });

            let probe_started = Instant::now();
            let readiness = mgr.readiness("base", &cfg).await;
            let probe_took = probe_started.elapsed();
            ticker.abort();

            let fired = ticks.load(std::sync::atomic::Ordering::Relaxed);

            // The readiness probe must complete (status = "fail"), and the
            // ticker must have advanced at least once — proving the runtime
            // was NOT wedged.
            assert_eq!(
                readiness.status, "fail",
                "unreachable fork must report fail, got: {readiness:?}"
            );
            assert!(
                readiness.error.is_some(),
                "fail status must carry an error message"
            );
            // The property is that the runtime made progress DURING the probe.
            // That is only observable while the probe is actually pending: on a
            // host with no foundry (every CI runner) `resolve_anvil_path` fails
            // in about a millisecond, so a 100ms ticker cannot have fired and
            // its silence says nothing. Requiring a tick unconditionally made
            // the test fail on exactly the machines where the behaviour was
            // perfect — asserting "the probe was slow" instead of "the runtime
            // stayed responsive".
            if probe_took >= std::time::Duration::from_millis(250) {
                assert!(
                    fired >= 1,
                    "the probe was pending for {probe_took:?} and the ticker never fired — the \
                     runtime was blocked by it ({fired} ticks)"
                );
            } else {
                // Nothing to prove, but say so rather than passing silently:
                // a run that never exercises the property should not read as
                // one that verified it.
                eprintln!(
                    "note: probe returned in {probe_took:?} (likely no anvil binary present), so \
                     runtime-responsiveness was NOT exercised here — the status/error assertions \
                     above still hold"
                );
            }
        });
    }

    /// Every `[fork]` knob the error remediations name must
    /// demonstrably reach the anvil command line — no fictional knobs.
    #[test]
    fn fork_knobs_propagate_to_anvil_args() {
        let fork = ForkConfig {
            timeout_ms: 45_000,
            retries: 9,
            retry_backoff_ms: 2_500,
            compute_units_per_second: Some(750),
            ..Default::default()
        };
        let args = anvil_fork_args(&fork, "osaka");
        for (flag, value) in [
            ("--hardfork", "osaka"),
            ("--timeout", "45000"),
            ("--retries", "9"),
            ("--fork-retry-backoff", "2500"),
            ("--compute-units-per-second", "750"),
        ] {
            let i = args
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("{flag} missing from anvil args: {args:?}"));
            assert_eq!(args[i + 1], value, "{flag} carries the configured value");
        }
        // cups is optional — unset must not emit the flag at all.
        let no_cups = ForkConfig::default();
        assert!(
            !anvil_fork_args(&no_cups, "osaka").contains(&"--compute-units-per-second".to_string())
        );
    }

    /// Every fork reads storage from its own upstream: anvil's on-disk RPC
    /// cache is keyed by chain and block only, so a fork from one upstream
    /// must never reload storage cached from another. The flag is passed
    /// whatever the `[fork]` knobs say.
    #[test]
    fn storage_caching_is_always_disabled() {
        let knobs = ForkConfig {
            timeout_ms: 45_000,
            retries: 9,
            retry_backoff_ms: 2_500,
            compute_units_per_second: Some(750),
            ..Default::default()
        };
        for (fork, hardfork) in [
            (ForkConfig::default(), "osaka"),
            (ForkConfig::default(), "cancun"),
            (knobs, "osaka"),
        ] {
            let args = anvil_fork_args(&fork, hardfork);
            assert_eq!(
                args.iter().filter(|a| *a == "--no-storage-caching").count(),
                1,
                "--no-storage-caching must be passed exactly once: {args:?}"
            );
        }
    }
}

#[cfg(test)]
mod orphan_sweep {
    use super::*;

    const OURS: &str = "/Users/x/.foundry/bin/anvil -p 0 -f https://base-rpc.publicnode.com \
                        --hardfork osaka --timeout 1250 --retries 0 --fork-retry-backoff 1000";

    #[test]
    fn an_orphaned_fork_of_ours_is_reapable() {
        assert!(is_orphaned_fork_of_ours(OURS, 1));
    }

    /// The condition that keeps this safe to run unattended. A fork whose
    /// parent is alive belongs to somebody's running work.
    #[test]
    fn a_fork_with_a_live_parent_is_never_reaped() {
        assert!(!is_orphaned_fork_of_ours(OURS, 4242));
    }

    /// A developer's own anvil carries no marker of ours, whatever else it
    /// looks like. This is the whole reason the marker exists.
    #[test]
    fn a_developers_own_anvil_is_never_ours() {
        for theirs in [
            "/Users/x/.foundry/bin/anvil --fork-url https://base.llamarpc.com --port 8545",
            "/Users/x/.foundry/bin/anvil -p 0 -f http://localhost:8545 --hardfork osaka",
            "anvil",
        ] {
            assert!(
                !is_orphaned_fork_of_ours(theirs, 1),
                "would have reaped a developer's own anvil: {theirs}"
            );
        }
    }

    /// The trap this classifier was written around. Grepping for the marker
    /// alone matches the very shell command doing the grep — which is how a
    /// sweep could kill its own tooling. Requiring "anvil" in the command is
    /// what excludes it.
    #[test]
    fn a_shell_command_that_merely_mentions_the_marker_is_not_a_fork() {
        let grepping =
            "/bin/zsh -c ps -eo pid,ppid,command | awk '/--fork-retry-backoff/ && $2==1'";
        assert!(
            !is_orphaned_fork_of_ours(grepping, 1),
            "a sweep would have signalled the command that was looking for forks"
        );
    }

    /// The marker is the one emitted by `anvil_fork_args`, not a copy of it
    /// that could drift.
    #[test]
    fn the_marker_is_the_flag_the_spawner_actually_passes() {
        let args = anvil_fork_args(&ForkConfig::default(), "osaka");
        assert!(
            args.iter().any(|a| a == OUR_ANVIL_MARKER),
            "the spawner no longer passes {OUR_ANVIL_MARKER}, so the sweep would reap nothing \
             and silently stop working: {args:?}"
        );
    }
}
