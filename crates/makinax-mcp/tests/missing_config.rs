//! Review focus 1: a missing or partial config. Every tool answers with an
//! error naming what is missing; nothing panics, and the server stays up.

mod common;

use common::{Session, call};
use makinax::ext::{MakinaxConfig, ModuleConfig};
use makinax_mcp::MakinaxService;
use serde_json::json;

fn module_on_base() -> ModuleConfig {
    ModuleConfig {
        chain: "base".into(),
        safe: "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap(),
        module: "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap(),
        rootfile: None,
        operator_address: None,
        guardian_address: None,
        swappers: Default::default(),
    }
}

fn cfg_with_module_but_no_rpc() -> MakinaxConfig {
    let mut cfg = MakinaxConfig::default();
    cfg.modules.insert("main".into(), module_on_base());
    cfg
}

/// Without a rootfile, a call that runs a rootfile action names the missing
/// key; a raw call, which runs none, still builds (no RPC needed).
#[tokio::test]
async fn a_call_that_needs_the_rootfile_names_it_and_a_raw_call_does_not() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap();
    let s = Session::start(svc).await;
    let r = s
        .call(
            "build_call",
            json!({"module": "main", "call": {"kind": "manage_position", "mgmt_action": "a.b.c"}}),
        )
        .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("modules.main.rootfile"), "{}", r.text);

    let r = s
        .call(
            "build_call",
            json!({"module": "main", "call": {"kind": "raw", "to": "0x3333333333333333333333333333333333333333", "data": "0x1234"}}),
        )
        .await;
    assert!(!r.is_error, "{}", r.text);
    let v: serde_json::Value = serde_json::from_str(&r.text).unwrap();
    assert_eq!(v["data"], "0x1234", "{v}");
    s.stop().await;
}

#[tokio::test]
async fn a_module_on_a_chain_without_rpc_gets_a_named_error() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap(); // starting is fine
    let r = call(&svc, "get_module_state", json!({"module": "main"})).await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("rpc_urls.base"), "{}", r.text);
}

#[tokio::test]
async fn an_unknown_module_is_named() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap();
    let r = call(&svc, "get_module_state", json!({"module": "nope"})).await;
    assert!(r.is_error, "{}", r.text);
    assert!(
        r.text.contains("module 'nope' is not configured"),
        "{}",
        r.text
    );
    assert!(
        r.text.contains("main"),
        "names the configured modules: {}",
        r.text
    );
}

#[cfg(feature = "write")]
#[tokio::test]
async fn execute_without_a_signer_is_a_clear_error() {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.rpc_urls
        .insert("base".into(), "http://127.0.0.1:1".into());
    let svc = MakinaxService::new(cfg).await.unwrap();
    let r = call(
        &svc,
        "execute_call",
        json!({"module": "main", "call": {"kind": "raw", "to": "0x2222222222222222222222222222222222222222", "data": "0x"}}),
    )
    .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("signer"), "{}", r.text);
}

#[cfg(feature = "write")]
#[tokio::test]
async fn a_signer_that_cannot_be_built_is_named_at_execute_not_at_start() {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.rpc_urls
        .insert("base".into(), "http://127.0.0.1:1".into());
    cfg.signer = makinax::ext::SignerConfig::Keystore(makinax::ext::KeystoreSignerConfig {
        path: "/nonexistent/keystore.json".into(),
        passphrase_file: None,
        passphrase_env: None,
        keychain_service: None,
    });
    let svc = MakinaxService::new(cfg).await.unwrap();
    let r = call(
        &svc,
        "execute_call",
        json!({"module": "main", "call": {"kind": "raw", "to": "0x2222222222222222222222222222222222222222", "data": "0x"}}),
    )
    .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("signer"), "{}", r.text);
}

#[tokio::test]
async fn an_empty_config_still_serves_list_modules() {
    let svc = MakinaxService::new(MakinaxConfig::default()).await.unwrap();
    let r = call(&svc, "list_modules", json!({})).await;
    assert!(!r.is_error, "{}", r.text);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&r.text).unwrap(),
        json!([])
    );
}

#[tokio::test]
async fn a_module_without_a_rootfile_names_the_key_for_every_rootfile_tool() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap();
    let s = Session::start(svc).await;
    for (tool, args) in [
        ("list_actions", json!({"module": "main"})),
        ("get_proof", json!({"module": "main", "action": "a.b.c"})),
    ] {
        let r = s.call(tool, args).await;
        assert!(r.is_error, "{tool}: {}", r.text);
        assert!(
            r.text.contains("modules.main.rootfile"),
            "{tool}: {}",
            r.text
        );
    }
    s.stop().await;
}

#[tokio::test]
async fn a_rootfile_that_does_not_load_is_named_and_does_not_stop_the_start() {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.modules.get_mut("main").unwrap().rootfile = Some(makinax::ext::RootfileSource::Local(
        "/nonexistent/rootfile.toml".into(),
    ));
    let svc = MakinaxService::new(cfg).await.unwrap();
    let r = call(&svc, "list_actions", json!({"module": "main"})).await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("/nonexistent/rootfile.toml"), "{}", r.text);
}

/// Every tool that needs a module answers an unknown one with the same
/// named error, and the server keeps answering after each.
#[tokio::test]
async fn the_server_stays_up_through_every_tool_failing() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap();
    let s = Session::start(svc).await;
    let call_spec =
        json!({"kind": "raw", "to": "0x2222222222222222222222222222222222222222", "data": "0x"});
    let mut cases = vec![
        ("get_module_state", json!({"module": "nope"})),
        ("get_balances", json!({"module": "nope"})),
        (
            "get_oracle_price",
            json!({"module": "nope", "base": "0x2222222222222222222222222222222222222222"}),
        ),
        (
            "get_module_events",
            json!({"module": "nope", "from_block": 1}),
        ),
        ("list_actions", json!({"module": "nope"})),
        ("get_proof", json!({"module": "nope", "action": "x"})),
        ("check_root", json!({"module": "nope"})),
        ("build_call", json!({"module": "nope", "call": call_spec})),
        ("preview_call", json!({"module": "nope", "call": call_spec})),
        (
            "encode_owner_call",
            json!({"module": "nope", "function": "pause()"}),
        ),
        (
            "build_owner_batch",
            json!({"module": "nope", "calls": [{"function": "pause()"}]}),
        ),
        (
            "get_swap_quote",
            json!({"module": "nope", "input_token": "0x2222222222222222222222222222222222222222", "output_token": "0x2222222222222222222222222222222222222222", "input_amount": "1", "slippage_bps": 50}),
        ),
        (
            "get_bridge_quote",
            json!({"module": "nope", "dest_chain": "mainnet", "input_token": "0x2222222222222222222222222222222222222222", "output_token": "0x2222222222222222222222222222222222222222", "amount": "1"}),
        ),
        (
            "simulate_call",
            json!({"module": "nope", "call": call_spec}),
        ),
        (
            "simulate_owner_batch",
            json!({"module": "nope", "calls": [{"function": "pause()"}]}),
        ),
    ];
    if cfg!(feature = "write") {
        cases.push(("execute_call", json!({"module": "nope", "call": call_spec})));
    }
    for (tool, args) in cases {
        let r = s.call(tool, args).await;
        assert!(r.is_error, "{tool}: {}", r.text);
        assert!(
            r.text.contains("module 'nope' is not configured"),
            "{tool}: {}",
            r.text
        );
    }
    // The chain-level tool with no module names the config key it needs.
    let r = s
        .call(
            "get_cctp_attestation",
            json!({"source_chain": "base", "tx_hash": format!("0x{}", "11".repeat(32))}),
        )
        .await;
    assert!(r.is_error, "{}", r.text);
    assert!(
        r.text.contains("bridges.cctp_v2.domains.base"),
        "{}",
        r.text
    );
    // And it is still serving.
    let r = s.call("list_modules", json!({})).await;
    assert!(!r.is_error, "{}", r.text);
    s.stop().await;
}

#[tokio::test]
async fn a_service_without_a_usable_config_returns_the_reason_from_every_config_tool() {
    let svc = MakinaxService::unavailable("no config file at /x/config.toml")
        .await
        .unwrap();
    let s = Session::start(svc).await;
    for (tool, args) in [
        ("list_modules", json!({})),
        ("get_module_state", json!({"module": "main"})),
        (
            "get_cctp_attestation",
            json!({"source_chain": "base", "tx_hash": format!("0x{}", "11".repeat(32))}),
        ),
    ] {
        let r = s.call(tool, args).await;
        assert!(r.is_error, "{tool}: {}", r.text);
        assert!(r.text.contains("/x/config.toml"), "{tool}: {}", r.text);
    }
    // A fixed list reads no config, and still answers.
    let r = s.call("list_owner_functions", json!({})).await;
    assert!(!r.is_error, "{}", r.text);
    s.stop().await;
}

#[tokio::test]
async fn an_unusable_rpc_url_fails_the_start_with_the_url_named() {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.rpc_urls.insert("base".into(), "not a url".into());
    let e = MakinaxService::new(cfg).await.err().expect("refused");
    assert!(e.to_string().contains("base"), "{e}");
}

/// The read-only build simulates with eth_call only, and says so rather
/// than silently using another backend.
#[cfg(not(feature = "write"))]
#[tokio::test]
async fn the_read_only_build_refuses_a_backend_it_does_not_have() {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.rpc_urls
        .insert("base".into(), "http://127.0.0.1:1".into());
    let svc = MakinaxService::new(cfg).await.unwrap();
    let r = call(
        &svc,
        "simulate_call",
        json!({"module": "main", "backend": "fork", "call": {"kind": "raw", "to": "0x2222222222222222222222222222222222222222", "data": "0x"}}),
    )
    .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("eth_call only"), "{}", r.text);
}

#[cfg(feature = "write")]
fn cfg_with_rpc() -> MakinaxConfig {
    let mut cfg = cfg_with_module_but_no_rpc();
    cfg.rpc_urls
        .insert("base".into(), "http://127.0.0.1:1".into());
    cfg
}

#[cfg(feature = "write")]
fn raw(to: &str, data: &str) -> serde_json::Value {
    json!({"module": "main", "call": {"kind": "raw", "to": to, "data": data}})
}

/// A raw private key is refused in this server's own words, which name
/// what it does accept and no option it does not offer.
#[cfg(feature = "write")]
#[tokio::test]
async fn a_private_key_signer_is_refused_naming_what_is_accepted() {
    let mut cfg = cfg_with_rpc();
    cfg.signer = makinax::ext::SignerConfig::PrivateKey(makinax::ext::PrivateKeySignerConfig {
        env: "SOME_KEY_ENV".into(),
    });
    let svc = MakinaxService::new(cfg).await.unwrap();
    let r = call(
        &svc,
        "execute_call",
        raw("0x2222222222222222222222222222222222222222", "0x12345678"),
    )
    .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("keystore"), "{}", r.text);
    assert!(!r.text.contains("allow_private_key"), "{}", r.text);
}

/// An owner-restricted selector is never sent, even to the module: every
/// owner function but the guardian's `pause()` and `unpause()`.
#[cfg(feature = "write")]
#[tokio::test]
async fn execute_refuses_an_owner_call_to_the_module() {
    let svc = MakinaxService::new(cfg_with_rpc()).await.unwrap();
    let mut refused = 0;
    for sig in makinax::ext::owner_function_names() {
        if ["pause()", "unpause()"].contains(sig) {
            continue;
        }
        let h = alloy::primitives::keccak256(sig.as_bytes());
        let data = format!("0x{}", alloy::primitives::hex::encode(&h[..4]));
        let r = call(
            &svc,
            "execute_call",
            raw("0x2222222222222222222222222222222222222222", &data),
        )
        .await;
        assert!(r.is_error, "{sig}: {}", r.text);
        assert!(
            r.text
                .contains("owner calls are encoded for the Safe owners"),
            "{sig}: {}",
            r.text
        );
        assert!(r.text.contains(&format!("`{sig}`")), "{sig}: {}", r.text);
        refused += 1;
    }
    assert_eq!(refused, makinax::ext::owner_function_names().len() - 2);
}

/// A guardian sends `pause()` and `unpause()` itself: both pass the check
/// and reach signing (here: no signer is configured, so that is where they
/// stop).
#[cfg(feature = "write")]
#[tokio::test]
async fn execute_allows_the_guardians_pause_and_unpause_up_to_signing() {
    let svc = MakinaxService::new(cfg_with_rpc()).await.unwrap();
    // pause() = 0x8456cb59, unpause() = 0x3f4ba83a
    for data in ["0x8456cb59", "0x3f4ba83a"] {
        let r = call(
            &svc,
            "execute_call",
            raw("0x2222222222222222222222222222222222222222", data),
        )
        .await;
        assert!(r.is_error, "{data}: {}", r.text);
        assert!(
            !r.text.contains("owner calls are encoded"),
            "{data}: {}",
            r.text
        );
        assert!(
            r.text.contains("no signer is configured"),
            "{data}: {}",
            r.text
        );
    }
}

/// A send to anything but the module is refused.
#[cfg(feature = "write")]
#[tokio::test]
async fn execute_refuses_a_target_other_than_the_module() {
    let svc = MakinaxService::new(cfg_with_rpc()).await.unwrap();
    for to in [
        // the Safe
        "0x1111111111111111111111111111111111111111",
        "0x3333333333333333333333333333333333333333",
    ] {
        let r = call(&svc, "execute_call", raw(to, "0x12345678")).await;
        assert!(r.is_error, "{to}: {}", r.text);
        assert!(
            r.text.contains("execute_call sends only to the module"),
            "{to}: {}",
            r.text
        );
    }
}

/// A plain operator call to the module passes the check and reaches
/// signing (here: no signer is configured, so that is where it stops).
#[cfg(feature = "write")]
#[tokio::test]
async fn execute_allows_an_operator_raw_call_to_the_module_up_to_signing() {
    let svc = MakinaxService::new(cfg_with_rpc()).await.unwrap();
    let r = call(
        &svc,
        "execute_call",
        raw("0x2222222222222222222222222222222222222222", "0x12345678"),
    )
    .await;
    assert!(r.is_error, "{}", r.text);
    assert!(r.text.contains("no signer is configured"), "{}", r.text);
}

/// Building and simulating a raw call stay unrestricted: the owner call
/// that execute refuses still builds.
#[tokio::test]
async fn build_of_a_raw_owner_call_is_not_restricted() {
    let svc = MakinaxService::new(cfg_with_module_but_no_rpc())
        .await
        .unwrap();
    let r = call(
        &svc,
        "build_call",
        json!({"module": "main", "call": {"kind": "raw", "to": "0x1111111111111111111111111111111111111111", "data": "0x8456cb59"}}),
    )
    .await;
    assert!(!r.is_error, "{}", r.text);
}
