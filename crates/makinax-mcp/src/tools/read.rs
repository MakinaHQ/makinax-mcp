//! Reads: configured modules, a module's live state, balances, oracle
//! prices, events, and the actions its rootfile allows.

use alloy::providers::Provider as _;
use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::ModuleArgs;
use crate::{
    MakinaxService,
    service::{parse_address, respond, to_json},
};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct BalancesArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// Token addresses to read. Default: every token the module's rootfile
    /// names.
    #[serde(default)]
    pub tokens: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct PriceArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// The token to price.
    pub base: String,
    /// The token to price it in. Default: the module's reference currency.
    #[serde(default)]
    pub quote: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct EventsArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// First block to read.
    pub from_block: u64,
    /// Last block to read. Default: the latest block.
    #[serde(default)]
    pub to_block: Option<u64>,
}

#[tool_router(router = read_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "List the modules in the config: name, chain, Safe, module address, rootfile, and how many actions the rootfile allows. Makes no RPC call.",
        annotations(read_only_hint = true)
    )]
    async fn list_modules(&self) -> CallToolResult {
        respond(self.list_modules_impl().await)
    }

    #[tool(
        description = "Read a module's live state at the latest block: its Safe, operating mode, pause and suspension, instruction root, operators and guardians among the configured candidates, loss caps, cooldowns, swapper targets, bridge recipients and accounting currency.",
        annotations(read_only_hint = true)
    )]
    async fn get_module_state(&self, Parameters(a): Parameters<ModuleArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let p = self.provider_for(&a.module, m)?;
                let state = makinax::ext::read_module_state(p, m)
                    .await
                    .map_err(|e| e.to_string())?;
                to_json(&state)
            }
            .await,
        )
    }

    #[tool(
        description = "Read the Safe's balance of each token: the tokens given, or every token the module's rootfile names. Amounts are raw integer units.",
        annotations(read_only_hint = true)
    )]
    async fn get_balances(&self, Parameters(a): Parameters<BalancesArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let tokens = match &a.tokens {
                    Some(list) => list
                        .iter()
                        .map(|t| parse_address("tokens", t))
                        .collect::<Result<Vec<_>, _>>()?,
                    None => self
                        .with_lib(&a.module, m, |lib| {
                            let mut set = std::collections::BTreeSet::new();
                            for action in lib.iter() {
                                set.extend(action.instruction.affected_tokens.iter().copied());
                            }
                            set.remove(&alloy::primitives::Address::ZERO);
                            Ok(set.into_iter().collect::<Vec<_>>())
                        })
                        .await
                        .map_err(|e| format!("{e}; or name the `tokens` to read"))?,
                };
                let p = self.provider_for(&a.module, m)?;
                let balances = makinax::ext::read_balances(p, m.safe, &tokens)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "safe": format!("{:?}", m.safe), "balances": to_json(&balances)? }))
            }
            .await,
        )
    }

    #[tool(
        description = "Read the module's oracle price for a token, in the module's reference currency or in a second token. Reports whether the token has a feed route.",
        annotations(read_only_hint = true)
    )]
    async fn get_oracle_price(&self, Parameters(a): Parameters<PriceArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let base = parse_address("base", &a.base)?;
                let quote = a
                    .quote
                    .as_deref()
                    .map(|q| parse_address("quote", q))
                    .transpose()?;
                let p = self.provider_for(&a.module, m)?;
                let price = makinax::ext::read_price(p, m.module, base, quote)
                    .await
                    .map_err(|e| e.to_string())?;
                to_json(&price)
            }
            .await,
        )
    }

    #[tool(
        description = "Read the module's events from from_block to to_block (default: the latest block), in chunks. When the range could not be read in full, the result carries next_from_block: call again from there to continue. What was read is kept.",
        annotations(read_only_hint = true)
    )]
    async fn get_module_events(&self, Parameters(a): Parameters<EventsArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let p = self.provider_for(&a.module, m)?;
                let to_block = match a.to_block {
                    Some(b) => b,
                    None => p
                        .get_block_number()
                        .await
                        .map_err(|e| format!("latest block: {e}"))?,
                };
                let page = makinax::ext::read_module_events(
                    p,
                    m.module,
                    a.from_block,
                    to_block,
                    &self.cfg.events,
                )
                .await
                .map_err(|e| e.to_string())?;
                to_json(&page)
            }
            .await,
        )
    }

    #[tool(
        description = "List the actions the module's rootfile allows: qualified name, kind, and the runtime parameters each takes.",
        annotations(read_only_hint = true)
    )]
    async fn list_actions(&self, Parameters(a): Parameters<ModuleArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                self.with_lib(&a.module, m, |lib| {
                    let actions: Vec<_> = lib
                        .iter()
                        .map(|x| {
                            json!({
                                "action": x.qualified_name,
                                "kind": x.kind,
                                "params": x.params,
                            })
                        })
                        .collect();
                    Ok(json!({
                        "rootfile": lib.source_label(),
                        "root": lib.root().to_string(),
                        "actions": actions,
                    }))
                })
                .await
            }
            .await,
        )
    }
}

impl MakinaxService {
    async fn list_modules_impl(&self) -> Result<serde_json::Value, String> {
        self.available()?;
        let libs = self.libs.read().await;
        let modules: Vec<_> = self
            .cfg
            .modules
            .iter()
            .map(|(name, m)| {
                let (actions, rootfile_error) = match (libs.get(name), self.lib_errors.get(name)) {
                    (Some(lib), _) => (Some(lib.count()), None),
                    (None, Some(e)) => (None, Some(e.clone())),
                    (None, None) => (None, None),
                };
                json!({
                    "name": name,
                    "chain": m.chain,
                    "safe": format!("{:?}", m.safe),
                    "module": format!("{:?}", m.module),
                    "rootfile": m.rootfile.as_ref().map(|r| r.to_string()),
                    "actions": actions,
                    "rootfile_error": rootfile_error,
                    "rpc_configured": self.providers.endpoints(&m.chain).is_some(),
                })
            })
            .collect();
        Ok(serde_json::Value::Array(modules))
    }
}
