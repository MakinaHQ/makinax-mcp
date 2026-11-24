//! Pins that `MakinaxConfig` parses a minimal config with the documented
//! defaults, and that the moved types (`RootfileSource`'s `local:`/`github:`
//! prefix parsing especially) keep their exact on-disk shape.

#[test]
fn a_minimal_config_parses_with_defaults() {
    let toml = r#"
[modules.main]
chain = "base"
safe = "0x0000000000000000000000000000000000000001"
module = "0x0000000000000000000000000000000000000002"
[rpc_urls]
base = "https://example.invalid"
"#;
    let c: makinax::config::MakinaxConfig = toml::from_str(toml).unwrap();
    assert_eq!(c.modules["main"].chain, "base");
    assert!(matches!(c.signer, makinax::config::SignerConfig::Unsigned));
    assert_eq!(c.events.chunk_blocks, 2000);
    assert_eq!(c.events.max_calls, 50);
    assert!(c.tenderly.is_none());
    assert!(c.bridges.cctp_v2.is_none());
    assert_eq!(
        c.rpc_urls["base"].primary(),
        Some("https://example.invalid")
    );
}

#[derive(Debug, serde::Deserialize)]
struct RootfileWrapper {
    r: makinax::config::RootfileSource,
}

#[test]
fn rootfile_source_parses_the_local_and_github_prefixes() {
    let w: RootfileWrapper = toml::from_str("r = \"local:/tmp/x.toml\"").unwrap();
    match w.r {
        makinax::config::RootfileSource::Local(p) => {
            assert_eq!(p.display().to_string(), "/tmp/x.toml")
        }
        other => panic!("expected Local, got {other:?}"),
    }

    let w: RootfileWrapper = toml::from_str("r = \"github:MakinaHQ/rootfiles\"").unwrap();
    match w.r {
        makinax::config::RootfileSource::Github(s) => assert_eq!(s, "MakinaHQ/rootfiles"),
        other => panic!("expected Github, got {other:?}"),
    }
}

#[test]
fn rootfile_source_without_a_recognised_prefix_is_refused() {
    let err = toml::from_str::<RootfileWrapper>("r = \"/tmp/bare-path.toml\"").unwrap_err();
    assert!(
        err.to_string().contains("local:") && err.to_string().contains("github:"),
        "{err}"
    );
}

#[test]
fn a_config_missing_events_and_fetch_sections_still_parses() {
    let toml = r#"
[rpc_urls]
mainnet = "https://one.example"
"#;
    let c: makinax::config::MakinaxConfig = toml::from_str(toml).unwrap();
    assert!(c.modules.is_empty());
    assert!(c.fetch.kyber.is_none());
    assert!(c.fork.sim_fallback);
    assert_eq!(c.fork.timeout_ms, 120_000);
}
