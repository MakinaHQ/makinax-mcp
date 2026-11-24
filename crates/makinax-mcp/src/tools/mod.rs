//! The tools, one file per group. Each is a thin wrapper over
//! `makinax::ext`; argument types shared between groups live here.

mod build;
#[cfg(feature = "write")]
mod exec;
mod fetch;
mod owner;
mod proof;
mod read;
pub(crate) mod sim;

use makinax::ext::{BuiltCall, CallSpec, CctpRoute, InstructionLibrary, ModuleConfig};
use schemars::JsonSchema;
use serde::Deserialize;

/// A module named in the config.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ModuleArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
}

/// A call to build against a module.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct CallArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// What to build; `kind` selects the call.
    pub call: CallSpec,
    /// For a CCTP bridge only: the route (both deployments and the
    /// finality) the order is built against.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    pub cctp_route: Option<serde_json::Value>,
}

/// One owner-restricted function call, by signature.
#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct OwnerCallArgs {
    /// The full signature, e.g. `pause()` or `addOperator(address)`; see
    /// list_owner_functions.
    pub function: String,
    /// The arguments, as strings, in order.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Build `a.call` against the module's library (an empty library when the
/// module has no rootfile, so raw calls, swaps and bridges still build).
pub(crate) fn build(
    m: &ModuleConfig,
    lib: &InstructionLibrary,
    call: &CallSpec,
    cctp_route: Option<&serde_json::Value>,
) -> Result<BuiltCall, String> {
    let route: Option<CctpRoute> = cctp_route
        .map(|v| {
            serde_json::from_value(v.clone()).map_err(|e| format!("`cctp_route` is invalid: {e}"))
        })
        .transpose()?;
    makinax::ext::build_call_with_cctp(m, lib, call, route.as_ref()).map_err(|e| e.to_string())
}

impl crate::MakinaxService {
    /// Build a call for `name`, with its library when it has one. A module
    /// without a rootfile still builds swaps, bridges and raw calls (they
    /// run no rootfile action); a call that runs one names the missing key.
    pub(crate) async fn build_for(
        &self,
        name: &str,
        m: &ModuleConfig,
        call: &CallSpec,
        cctp_route: Option<&serde_json::Value>,
    ) -> Result<BuiltCall, String> {
        let runs_an_action = matches!(
            call,
            CallSpec::ManagePosition { .. } | CallSpec::Harvest { .. }
        );
        if runs_an_action || m.rootfile.is_some() || self.lib_errors.contains_key(name) {
            self.with_lib(name, m, |lib| build(m, lib, call, cctp_route))
                .await
        } else {
            build(
                m,
                &InstructionLibrary::empty("no rootfile"),
                call,
                cctp_route,
            )
        }
    }
}

/// The JSON view of a built call.
pub(crate) fn built_json(b: &BuiltCall) -> serde_json::Value {
    serde_json::json!({
        "to": format!("{:?}", b.to),
        "data": b.data.to_string(),
        "value": b.value.to_string(),
        "label": b.label,
        "affected_tokens": b.affected_tokens.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
        "has_real_accounting": b.has_real_accounting,
    })
}

/// Encode `calls` into a Safe Transaction Builder batch for module `m`.
pub(crate) fn owner_batch(
    m: &ModuleConfig,
    chain_id: u64,
    name: &str,
    description: &str,
    calls: &[OwnerCallArgs],
) -> Result<(makinax::ext::TxBuilderBatch, Vec<String>), String> {
    let encoded = calls
        .iter()
        .map(|c| makinax::ext::encode_owner_call(m.module, &c.function, &c.args))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    makinax::ext::build_batch(name, description, chain_id, m.safe, now_ms, &encoded)
        .map_err(|e| e.to_string())
}

impl crate::MakinaxService {
    /// The module's chain id: the chain table's, else the RPC's.
    pub(crate) async fn chain_id_of(&self, name: &str, m: &ModuleConfig) -> Result<u64, String> {
        if let Some(id) = makinax::ext::chain_id_for(&m.chain) {
            return Ok(id);
        }
        use alloy::providers::Provider as _;
        self.provider_for(name, m)?
            .get_chain_id()
            .await
            .map_err(|e| format!("chain '{}': eth_chainId: {e}", m.chain))
    }
}
