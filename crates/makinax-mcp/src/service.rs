//! The MCP service: the parsed config, one provider per configured chain,
//! each module's instruction library, and (read-write build) the fork
//! manager and the signer.
//!
//! Starting never fails on a module-level gap. A module whose chain has no
//! RPC, a rootfile that does not load, or a signer that cannot be built is
//! recorded, and the tool that needs it returns an error naming the missing
//! key. Only an unusable RPC URL fails [`MakinaxService::new`]; the binary
//! then serves [`MakinaxService::unavailable`], whose every config-reading
//! tool returns that reason (`list_owner_functions`, a fixed list, still
//! answers).

use std::{collections::BTreeMap, sync::Arc};

use makinax::ext::{InstructionLibrary, MakinaxConfig, ModuleConfig, Providers, RootProvider};
use rmcp::{
    ServerHandler,
    handler::server::router::tool::ToolRouter,
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool_handler,
};
use tokio::sync::{RwLock, RwLockReadGuard};

#[cfg(feature = "write")]
use makinax::ext::{ForkManager, SimReceiptStore};

/// How long a simulation receipt is reported valid for, in blocks. Receipts
/// are informational here: nothing in this server requires or checks one.
#[cfg(feature = "write")]
const RECEIPT_TTL_BLOCKS: u64 = 20;

/// Why a `private_key` signer is refused.
#[cfg(feature = "write")]
pub(crate) const PRIVATE_KEY_REFUSED: &str =
    "this server accepts only an encrypted keystore or a KMS signer; raw private keys are refused";

/// The signer, as far as starting could get with it.
#[cfg(feature = "write")]
pub(crate) enum SignerState {
    /// `[signer]` is absent or `type = "unsigned"`.
    None,
    /// The configured signer could not be built; the reason.
    Failed(String),
    Ready {
        wallet: alloy::network::EthereumWallet,
        address: alloy::primitives::Address,
    },
}

/// The words the config-lookup errors use for a configured entry.
///
/// A caller that serves these tools from its own config (whose entries are
/// named differently) supplies its own words, so an error names the key the
/// reader can actually edit. [`ConfigWording::MODULES`] is this server's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigWording {
    /// One entry, in prose: `module 'x' is not configured`.
    pub noun: &'static str,
    /// The config table that holds the entries: `[modules.*]`,
    /// `modules.x.rootfile`.
    pub section: &'static str,
    /// The error prefix when the service could not start with its config:
    /// `the server has no usable config: <why>`.
    pub unusable_config: &'static str,
}

impl ConfigWording {
    /// This server's own config: `[modules.<name>]`.
    pub const MODULES: Self = Self {
        noun: "module",
        section: "modules",
        unusable_config: "the server has no usable config",
    };
}

impl Default for ConfigWording {
    fn default() -> Self {
        Self::MODULES
    }
}

#[derive(Clone)]
pub struct MakinaxService {
    pub(crate) cfg: Arc<MakinaxConfig>,
    pub(crate) providers: Arc<Providers>,
    pub(crate) libs: Arc<RwLock<BTreeMap<String, InstructionLibrary>>>,
    /// Modules whose rootfile is configured but did not load, with why
    /// (`rootfile <src> did not load: <error>`).
    pub(crate) lib_errors: Arc<BTreeMap<String, String>>,
    /// The words config-lookup errors use.
    pub(crate) wording: ConfigWording,
    /// Where the config came from, named in "not configured" errors.
    pub(crate) config_source: Arc<str>,
    /// Set when the server could not start with its config: every tool
    /// returns it.
    pub(crate) unavailable: Option<Arc<str>>,
    #[cfg(feature = "write")]
    pub(crate) forks: Arc<ForkManager>,
    #[cfg(feature = "write")]
    pub(crate) receipts: Arc<SimReceiptStore>,
    #[cfg(feature = "write")]
    pub(crate) signer: Arc<SignerState>,
    tool_router: ToolRouter<Self>,
}

impl MakinaxService {
    /// Start with `cfg`. Fails only when an RPC URL cannot be used at all.
    pub async fn new(cfg: MakinaxConfig) -> makinax::ext::Result<Self> {
        Self::with_source(cfg, "the config").await
    }

    /// [`Self::new`], naming where the config came from in errors.
    pub async fn with_source(
        cfg: MakinaxConfig,
        source: impl Into<String>,
    ) -> makinax::ext::Result<Self> {
        let providers = Providers::from_config(&cfg).await?;
        let mut libs = BTreeMap::new();
        let mut lib_errors = BTreeMap::new();
        for (name, m) in &cfg.modules {
            let Some(src) = &m.rootfile else { continue };
            match InstructionLibrary::load(src).await {
                Ok(lib) => {
                    libs.insert(name.clone(), lib);
                }
                Err(e) => {
                    tracing::warn!(module = %name, error = %e, "rootfile did not load");
                    lib_errors.insert(
                        name.clone(),
                        error_text(format!("rootfile {src} did not load: {e}")),
                    );
                }
            }
        }
        #[cfg(feature = "write")]
        let signer = Self::build_signer(&cfg).await;
        Ok(Self {
            cfg: Arc::new(cfg),
            providers: Arc::new(providers),
            libs: Arc::new(RwLock::new(libs)),
            lib_errors: Arc::new(lib_errors),
            config_source: source.into().into(),
            unavailable: None,
            wording: ConfigWording::MODULES,
            #[cfg(feature = "write")]
            forks: Arc::new(ForkManager::new()),
            #[cfg(feature = "write")]
            receipts: Arc::new(SimReceiptStore::new(RECEIPT_TTL_BLOCKS)),
            #[cfg(feature = "write")]
            signer: Arc::new(signer),
            tool_router: Self::router(),
        })
    }

    /// A service that could not start with its config. Every tool that
    /// reads the config returns `problem`, so a client can read why;
    /// `list_owner_functions`, a fixed list, still answers.
    pub async fn unavailable(problem: impl Into<String>) -> makinax::ext::Result<Self> {
        let cfg = MakinaxConfig::default();
        let providers = Providers::from_config(&cfg).await?;
        Ok(Self {
            cfg: Arc::new(cfg),
            providers: Arc::new(providers),
            libs: Arc::new(RwLock::new(BTreeMap::new())),
            lib_errors: Arc::new(BTreeMap::new()),
            config_source: "no config".into(),
            unavailable: Some(problem.into().into()),
            wording: ConfigWording::MODULES,
            #[cfg(feature = "write")]
            forks: Arc::new(ForkManager::new()),
            #[cfg(feature = "write")]
            receipts: Arc::new(SimReceiptStore::new(RECEIPT_TTL_BLOCKS)),
            #[cfg(feature = "write")]
            signer: Arc::new(SignerState::None),
            tool_router: Self::router(),
        })
    }

    #[cfg(feature = "write")]
    async fn build_signer(cfg: &MakinaxConfig) -> SignerState {
        use alloy::network::{Ethereum, NetworkWallet};
        // Refused here, in this server's own words: makinax's refusal names
        // an opt-in this server does not offer.
        if matches!(cfg.signer, makinax::ext::SignerConfig::PrivateKey(_)) {
            tracing::warn!("a raw private key signer is configured; it is refused");
            return SignerState::Failed(PRIVATE_KEY_REFUSED.to_string());
        }
        match makinax::ext::build_wallet(&cfg.signer, false).await {
            Ok(None) => SignerState::None,
            Ok(Some(wallet)) => {
                let address = NetworkWallet::<Ethereum>::default_signer_address(&wallet);
                SignerState::Ready { wallet, address }
            }
            Err(e) => {
                tracing::warn!(error = %e, "the configured signer could not be built");
                SignerState::Failed(e.to_string())
            }
        }
    }

    /// Every tool this build serves.
    pub fn router() -> ToolRouter<Self> {
        let router = Self::read_router()
            + Self::proof_router()
            + Self::build_router()
            + Self::owner_router()
            + Self::fetch_router()
            + Self::sim_router();
        #[cfg(feature = "write")]
        let router = router + Self::exec_router();
        #[cfg(not(feature = "write"))]
        let router = {
            let mut router = router;
            for (name, text) in crate::tools::sim::READ_ONLY_DESCRIPTIONS {
                if let Some(route) = router.map.get_mut(name) {
                    route.attr.description = Some(text.into());
                }
            }
            router
        };
        router
    }

    /// Name config entries in errors with `wording` instead of this
    /// server's own `[modules.*]`.
    pub fn with_config_wording(mut self, wording: ConfigWording) -> Self {
        self.wording = wording;
        self
    }

    /// Stop any fork this service started.
    pub fn shutdown(&self) {
        #[cfg(feature = "write")]
        self.forks.shutdown();
    }

    // ---- lookups every tool shares; each error names the missing key ----

    pub(crate) fn available(&self) -> Result<(), String> {
        match &self.unavailable {
            Some(problem) => Err(format!("{}: {problem}", self.wording.unusable_config)),
            None => Ok(()),
        }
    }

    pub(crate) fn module(&self, name: &str) -> Result<&ModuleConfig, String> {
        self.available()?;
        self.cfg.modules.get(name).ok_or_else(|| {
            let ConfigWording { noun, section, .. } = self.wording;
            let known: Vec<&str> = self.cfg.modules.keys().map(String::as_str).collect();
            if known.is_empty() {
                format!(
                    "{noun} '{name}' is not configured: no [{section}.*] entry in {}",
                    self.config_source
                )
            } else {
                format!(
                    "{noun} '{name}' is not configured (configured: {})",
                    known.join(", ")
                )
            }
        })
    }

    pub(crate) fn provider_for(
        &self,
        name: &str,
        m: &ModuleConfig,
    ) -> Result<&RootProvider, String> {
        self.providers.get(&m.chain).map_err(|_| {
            format!(
                "{noun} '{name}' is on chain '{chain}', and rpc_urls.{chain} is not configured",
                noun = self.wording.noun,
                chain = m.chain
            )
        })
    }

    /// The module's instruction library, or the error naming why there is
    /// none (no rootfile configured, or it did not load).
    pub(crate) async fn lib_for(
        &self,
        name: &str,
        m: &ModuleConfig,
    ) -> Result<RwLockReadGuard<'_, InstructionLibrary>, String> {
        let ConfigWording { noun, section, .. } = self.wording;
        if let Some(e) = self.lib_errors.get(name) {
            return Err(format!("{noun} '{name}': {e}"));
        }
        RwLockReadGuard::try_map(self.libs.read().await, |libs| libs.get(name)).map_err(|_| {
            if m.rootfile.is_none() {
                format!(
                    "{noun} '{name}' has no rootfile: set {section}.{name}.rootfile (\"local:<path>\")"
                )
            } else {
                format!("{noun} '{name}': rootfile not loaded")
            }
        })
    }

    /// Run `f` against the module's instruction library.
    pub(crate) async fn with_lib<T>(
        &self,
        name: &str,
        m: &ModuleConfig,
        f: impl FnOnce(&InstructionLibrary) -> Result<T, String>,
    ) -> Result<T, String> {
        f(&*self.lib_for(name, m).await?)
    }
}

// `router = self.tool_router`: the field holds every group's routes, not
// only the macro default.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for MakinaxService {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::new("makinax-mcp", crate::BUILD_IDENTITY);
        info.instructions = Some(format!(
            "Read, build, prove and simulate calls to a makinaX module ({} build). \
             Modules are named in the config; list_modules shows them.",
            crate::BUILD_VARIANT
        ));
        info
    }
}

/// A tool's answer: JSON on success, the error text (never a panic) on
/// failure.
///
/// Every tool error leaves through here, so this is where an error text is
/// masked ([`error_text`]) whatever built it. A successful result is not
/// rewritten: a field of it that reports a failure is masked where it is
/// set.
pub(crate) fn respond(r: Result<serde_json::Value, String>) -> CallToolResult {
    match r {
        Ok(v) => CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
        )]),
        Err(e) => CallToolResult::error(vec![Content::text(error_text(e))]),
    }
}

/// An error's text as a client may see it: credentials in any endpoint URL
/// it quotes are masked, and the rest is unchanged. A transport error
/// quotes the URL it was talking to, and an RPC URL routinely carries its
/// key.
pub(crate) fn error_text(e: impl std::fmt::Display) -> String {
    makinax::ext::redact::text(&e.to_string())
}

/// Serialize a result value, mapping a failure to an error string.
pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(v).map_err(|e| format!("could not render the result: {e}"))
}

pub(crate) fn parse_address(field: &str, s: &str) -> Result<alloy::primitives::Address, String> {
    s.trim()
        .parse()
        .map_err(|e| format!("`{field}` is not an address ({s}): {e}"))
}

pub(crate) fn parse_u256(field: &str, s: &str) -> Result<alloy::primitives::U256, String> {
    s.trim()
        .parse()
        .map_err(|e| format!("`{field}` is not a non-negative integer ({s}): {e}"))
}
