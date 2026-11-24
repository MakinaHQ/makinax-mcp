//! Drive a service through a real MCP client over an in-memory pipe.
#![allow(dead_code)]

use makinax_mcp::MakinaxService;
use rmcp::{ServiceExt, model::CallToolRequestParams};

/// One tool call's outcome: whether it was an error, and its text.
pub struct Outcome {
    pub is_error: bool,
    pub text: String,
}

/// A running server and a client connected to it.
pub struct Session {
    client: rmcp::service::RunningService<rmcp::RoleClient, ()>,
    server: tokio::task::JoinHandle<()>,
}

impl Session {
    pub async fn start(svc: MakinaxService) -> Self {
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            let running = svc.serve(server_side).await.expect("server starts");
            let _ = running.waiting().await;
        });
        let client = ().serve(client_side).await.expect("client connects");
        Self { client, server }
    }

    pub async fn call(&self, name: &str, args: serde_json::Value) -> Outcome {
        let mut params = CallToolRequestParams::new(name.to_string());
        if let Some(obj) = args.as_object() {
            params = params.with_arguments(obj.clone());
        }
        let r = self
            .client
            .call_tool(params)
            .await
            .unwrap_or_else(|e| panic!("{name}: the call itself failed: {e}"));
        let text = r
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        Outcome {
            is_error: r.is_error == Some(true),
            text,
        }
    }

    pub async fn tool_names(&self) -> Vec<String> {
        self.client
            .list_all_tools()
            .await
            .expect("tools/list")
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    pub async fn stop(self) {
        self.client.cancel().await.expect("client stops");
        self.server.await.expect("server task ends");
    }
}

/// One call through a fresh session.
pub async fn call(svc: &MakinaxService, name: &str, args: serde_json::Value) -> Outcome {
    let s = Session::start(svc.clone()).await;
    let out = s.call(name, args).await;
    s.stop().await;
    out
}
