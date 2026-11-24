//! Building: a call's calldata, and a readable preview of it.

use rmcp::{handler::server::wrapper::Parameters, model::CallToolResult, tool, tool_router};
use serde_json::json;

use super::{CallArgs, built_json};
use crate::{MakinaxService, service::respond};

#[tool_router(router = build_router, vis = "pub(crate)")]
impl MakinaxService {
    #[tool(
        description = "Build a call to the module and return its calldata: target, data and value. kind is manage_position, harvest, swap, bridge or raw. Nothing is sent or simulated.",
        annotations(read_only_hint = true)
    )]
    async fn build_call(&self, Parameters(a): Parameters<CallArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let built = self
                    .build_for(&a.module, m, &a.call, a.cctp_route.as_ref())
                    .await?;
                Ok(built_json(&built))
            }
            .await,
        )
    }

    #[tool(
        description = "Build a call to the module and describe it without calldata: which actions it runs, with which parameters, and which tokens it may move. Nothing is sent or simulated.",
        annotations(read_only_hint = true)
    )]
    async fn preview_call(&self, Parameters(a): Parameters<CallArgs>) -> CallToolResult {
        respond(
            async {
                let m = self.module(&a.module)?;
                let built = self
                    .build_for(&a.module, m, &a.call, a.cctp_route.as_ref())
                    .await?;
                let summary: serde_json::Value = serde_json::from_str(&built.summary_json)
                    .unwrap_or(serde_json::Value::String(built.summary_json.clone()));
                Ok(json!({
                    "module": a.module,
                    "chain": m.chain,
                    "label": built.label,
                    "to": format!("{:?}", built.to),
                    "value": built.value.to_string(),
                    "calldata_bytes": built.data.len(),
                    "summary": summary,
                    "affected_tokens": built.affected_tokens.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>(),
                    "has_real_accounting": built.has_real_accounting,
                }))
            }
            .await,
        )
    }
}
