//! The words config-lookup errors use. This server's own wording is pinned
//! byte for byte; a caller-supplied wording replaces every `module` /
//! `[modules.*]` in those errors and nothing else.

mod common;

use common::call;
use makinax::ext::{MakinaxConfig, ModuleConfig, RootfileSource};
use makinax_mcp::{ConfigWording, MakinaxService};
use serde_json::json;

fn module_on_base(rootfile: Option<RootfileSource>) -> ModuleConfig {
    ModuleConfig {
        chain: "base".into(),
        safe: "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap(),
        module: "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap(),
        rootfile,
        operator_address: None,
        guardian_address: None,
        swappers: Default::default(),
    }
}

/// `main` has no rootfile and no RPC; `bad` has a rootfile that cannot load.
fn cfg() -> MakinaxConfig {
    let mut cfg = MakinaxConfig::default();
    cfg.modules.insert("main".into(), module_on_base(None));
    cfg.modules.insert(
        "bad".into(),
        module_on_base(Some(RootfileSource::Local(
            "/nonexistent/rootfile.toml".into(),
        ))),
    );
    cfg
}

/// A caller whose config names its entries `[safes.<name>]`.
const SAFES: ConfigWording = ConfigWording {
    noun: "safe",
    section: "safes",
    unusable_config: "the tools have no usable config",
};

async fn texts(svc: &MakinaxService, empty: &MakinaxService) -> Vec<String> {
    let mut out = Vec::new();
    for (s, tool, args) in [
        (empty, "get_module_state", json!({"module": "nope"})),
        (svc, "get_module_state", json!({"module": "nope"})),
        (svc, "get_module_state", json!({"module": "main"})),
        (
            svc,
            "get_proof",
            json!({"module": "main", "action": "a.b.c"}),
        ),
        (
            svc,
            "get_proof",
            json!({"module": "bad", "action": "a.b.c"}),
        ),
    ] {
        let r = call(s, tool, args).await;
        assert!(r.is_error, "{}", r.text);
        out.push(r.text);
    }
    out
}

#[tokio::test]
async fn the_default_wording_is_this_servers_own() {
    let svc = MakinaxService::new(cfg()).await.unwrap();
    let empty = MakinaxService::new(MakinaxConfig::default()).await.unwrap();
    let t = texts(&svc, &empty).await;
    assert_eq!(
        t[0],
        "module 'nope' is not configured: no [modules.*] entry in the config"
    );
    assert_eq!(
        t[1],
        "module 'nope' is not configured (configured: bad, main)"
    );
    assert_eq!(
        t[2],
        "module 'main' is on chain 'base', and rpc_urls.base is not configured"
    );
    assert_eq!(
        t[3],
        "module 'main' has no rootfile: set modules.main.rootfile (\"local:<path>\")"
    );
    assert!(
        t[4].starts_with("module 'bad': rootfile local:/nonexistent/rootfile.toml did not load: "),
        "{}",
        t[4]
    );

    let down = MakinaxService::unavailable("no config file at /x/config.toml")
        .await
        .unwrap();
    let r = call(&down, "get_module_state", json!({"module": "main"})).await;
    assert_eq!(
        r.text,
        "the server has no usable config: no config file at /x/config.toml"
    );
    assert_eq!(ConfigWording::default(), ConfigWording::MODULES);
}

#[tokio::test]
async fn a_caller_wording_replaces_every_module_word() {
    let svc = MakinaxService::new(cfg())
        .await
        .unwrap()
        .with_config_wording(SAFES);
    let empty = MakinaxService::new(MakinaxConfig::default())
        .await
        .unwrap()
        .with_config_wording(SAFES);
    let t = texts(&svc, &empty).await;
    assert_eq!(
        t[0],
        "safe 'nope' is not configured: no [safes.*] entry in the config"
    );
    assert_eq!(
        t[1],
        "safe 'nope' is not configured (configured: bad, main)"
    );
    assert_eq!(
        t[2],
        "safe 'main' is on chain 'base', and rpc_urls.base is not configured"
    );
    assert_eq!(
        t[3],
        "safe 'main' has no rootfile: set safes.main.rootfile (\"local:<path>\")"
    );
    assert!(
        t[4].starts_with("safe 'bad': rootfile local:/nonexistent/rootfile.toml did not load: "),
        "{}",
        t[4]
    );
    for text in &t {
        assert!(!text.contains("module"), "{text}");
    }

    let down = MakinaxService::unavailable("no config file at /x/config.toml")
        .await
        .unwrap()
        .with_config_wording(SAFES);
    let r = call(&down, "get_module_state", json!({"module": "main"})).await;
    assert_eq!(
        r.text,
        "the tools have no usable config: no config file at /x/config.toml"
    );
}
