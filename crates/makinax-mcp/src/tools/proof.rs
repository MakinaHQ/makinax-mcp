//! Proofs: the merkle proof for an action, and whether the loaded root is
//! the one the module enforces.

use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use super::ModuleArgs;
use crate::{
    MakinaxService,
    service::{respond, to_json},
};

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct ProofArgs {
    /// The module's name in the config (`[modules.<name>]`).
    pub module: String,
    /// The action's qualified name, `protocol.action.token`; see
    /// list_actions.
    pub action: String,
}

#[tool_router(router = proof_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "The merkle proof that an action is allowed under the module's loaded rootfile: leaf hash, sibling hashes and root. An action the rootfile does not contain is refused.",
        annotations(read_only_hint = true)
    )]
    async fn get_proof(&self, Parameters(a): Parameters<ProofArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                self.with_lib(&a.module, m, |lib| {
                    makinax::ext::proof_for(lib, &a.action)
                        .map_err(|e| e.to_string())
                        .and_then(|p| to_json(&p))
                })
                .await
            }
            .await,
        )
    }

    #[tool(
        description = "Compare the loaded rootfile's root with the module's on-chain allowed root. A difference is an error carrying both roots.",
        annotations(read_only_hint = true)
    )]
    async fn check_root(&self, Parameters(a): Parameters<ModuleArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let p = self.provider_for(&a.module, m)?;
                let lib = self.lib_for(&a.module, m).await?;
                let root = makinax::ext::check_root(&lib, p, m.module)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({ "matches": true, "root": root.to_string() }))
            }
            .await,
        )
    }
}
