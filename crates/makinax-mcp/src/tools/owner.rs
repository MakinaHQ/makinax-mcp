//! Owner calls: encoded for the Safe's owners to sign, never sent (a
//! guardian's `pause()`/`unpause()` go through execute_call instead).

use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::{OwnerCallArgs, owner_batch};
use crate::{
    MakinaxService,
    service::{respond, to_json},
};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct EncodeOwnerArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// The full signature, e.g. `pause()` or `addOperator(address)`; see
    /// list_owner_functions.
    pub function: String,
    /// The arguments, as strings, in order.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct OwnerBatchArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// The calls, in the order they run.
    pub calls: Vec<OwnerCallArgs>,
    /// The batch's name. Default: "makinaX module owner calls".
    #[serde(default)]
    pub name: Option<String>,
    /// The batch's description. Default: the call summaries.
    #[serde(default)]
    pub description: Option<String>,
}

#[tool_router(router = owner_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "List the module's owner-restricted functions (callable only by its Safe; `pause()` and `unpause()` also by any guardian), as full signatures.",
        annotations(read_only_hint = true)
    )]
    async fn list_owner_functions(&self) -> CallToolResult {
        respond(Ok(json!(makinax::ext::owner_function_names())))
    }

    #[tool(
        description = "Encode one owner-restricted call to the module: target and calldata for the Safe to execute. A function that is not owner-restricted is refused. Nothing is sent.",
        annotations(read_only_hint = true)
    )]
    async fn encode_owner_call(
        &self,
        Parameters(a): Parameters<EncodeOwnerArgs>,
    ) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                // The chain id only goes into the batch envelope, which is
                // not returned here: the calldata does not depend on it.
                let (batch, summary) = owner_batch(
                    m,
                    0,
                    "owner call",
                    "",
                    &[OwnerCallArgs {
                        function: a.function.clone(),
                        args: a.args.clone(),
                    }],
                )?;
                let tx = batch
                    .transactions()
                    .first()
                    .ok_or_else(|| "the call did not encode".to_string())?;
                Ok(json!({
                    "to": tx.to,
                    "value": tx.value,
                    "data": tx.data,
                    "function": a.function,
                    "args": a.args,
                    "summary": summary.first(),
                }))
            }
            .await,
        )
    }

    #[tool(
        description = "Encode an ordered list of owner-restricted calls to the module as a Safe Transaction Builder batch (JSON the Safe web app imports), with a one-line summary per call. Nothing is sent.",
        annotations(read_only_hint = true)
    )]
    async fn build_owner_batch(&self, Parameters(a): Parameters<OwnerBatchArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                if a.calls.is_empty() {
                    return Err("`calls` is empty".to_string());
                }
                let chain_id = self.chain_id_of(&a.module, m).await?;
                let name = a
                    .name
                    .clone()
                    .unwrap_or_else(|| "makinaX module owner calls".to_string());
                // Encode once for the summaries, then again with them as the
                // description when none is given.
                let (_, summary) = owner_batch(m, chain_id, &name, "", &a.calls)?;
                let description = a.description.clone().unwrap_or_else(|| summary.join("; "));
                let (batch, summary) = owner_batch(m, chain_id, &name, &description, &a.calls)?;
                Ok(json!({
                    "batch": to_json(&batch)?,
                    "summary": summary,
                    "safe_builder_url": makinax::ext::chains::safe_builder_url(&m.chain, m.safe),
                }))
            }
            .await,
        )
    }
}
