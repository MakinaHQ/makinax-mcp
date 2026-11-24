//! The generic chain-profile lookups this crate owns.

#[test]
fn chain_id_for_resolves_a_config_key() {
    assert_eq!(makinax::chain::chain_id_for("base"), Some(8453));
}

#[test]
fn by_id_resolves_a_verified_chain() {
    assert_eq!(makinax::chains::by_id(1).unwrap().key, "mainnet");
}

#[test]
fn by_key_refuses_an_unknown_chain() {
    assert!(makinax::chains::by_key("nope").is_none());
}
