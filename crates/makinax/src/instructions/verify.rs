//! Verify the loaded rootfile's merkle root matches the on-chain `allowedInstrRoot`.
//!
//! A mismatch means the caller is operating against a stale rootfile: every
//! proof it builds would revert on-chain. See also
//! [`super::proofs::check_root`], which turns the comparison into a
//! structured [`crate::error::Error::RootMismatch`].

use alloy::{
    network::Ethereum,
    primitives::{Address, B256},
    providers::Provider,
};

use crate::{chain::module::IMakinaXModule, error::Result};

/// Reads `allowedInstrRoot()` from the on-chain module.
pub async fn fetch_onchain_root<P: Provider<Ethereum>>(
    provider: &P,
    module: Address,
) -> Result<B256> {
    let module = IMakinaXModule::new(module, provider);
    let root = module
        .allowedInstrRoot()
        .call()
        .await
        .map_err(|e| crate::error::Error::rpc(format!("allowedInstrRoot: {}", e)))?;
    Ok(root)
}

/// True if the loaded library's root matches the on-chain root.
///
/// The ZERO root is never a match, on either side. A module with no rootfile
/// carries an empty placeholder library whose merkle root is `0x00…00`, and a
/// module that has never had `setAllowedInstrRoot` called reports exactly the
/// same value — so plain equality would report "root matches" for a module
/// where nothing whatsoever is configured. That is the worst possible false
/// green: it is precisely the state of a freshly deployed module.
///
/// Fixed HERE rather than at the call sites, because this is the one predicate
/// every comparison should route through, and "audit the other call sites" is
/// not enforcement.
pub fn matches(loaded: B256, onchain: B256) -> bool {
    if loaded == B256::ZERO || onchain == B256::ZERO {
        return false;
    }
    loaded == onchain
}

#[cfg(test)]
mod zero_root_tests {
    use super::*;

    /// An empty whitelist must mean "nothing permitted", NEVER "everything",
    /// and must never read as agreement with the chain.
    ///
    /// A module with no rootfile carries an empty placeholder library whose
    /// merkle root is 0x00…00. A module that has never had
    /// `setAllowedInstrRoot` called reports exactly the same value. Plain
    /// equality would therefore report "root matches" for a module where
    /// NOTHING is configured — the worst possible false green.
    #[test]
    fn an_empty_library_never_matches_an_unset_on_chain_root() {
        assert!(
            !matches(B256::ZERO, B256::ZERO),
            "an empty library against a module with no root must NOT read as a match"
        );
        let real = B256::repeat_byte(0xab);
        assert!(
            !matches(B256::ZERO, real),
            "an empty library must not match a real on-chain root"
        );
        assert!(
            !matches(real, B256::ZERO),
            "a real library must not match an unset on-chain root"
        );
        assert!(matches(real, real), "genuine agreement still matches");
    }

    /// The empty library permits nothing: no action resolves, and there is
    /// nothing to iterate. "Empty" must never be read as "unrestricted".
    #[test]
    fn an_empty_library_permits_nothing() {
        let lib = crate::instructions::InstructionLibrary::empty("none");
        assert_eq!(lib.count(), 0);
        assert!(lib.iter().next().is_none());
        assert!(
            lib.get("morpho.deposit.usdc").is_err(),
            "an empty whitelist must refuse every action name"
        );
        assert!(lib.position_ids().is_empty());
        assert!(lib.is_placeholder(), "and it must know what it is");
    }

    /// The on-chain read returns the module's `allowedInstrRoot` as read,
    /// and a failed read is an `Rpc` error naming the call, never a zero
    /// root (which would read as "unset").
    #[tokio::test]
    async fn fetch_onchain_root_returns_the_root_or_names_the_failed_call() {
        use alloy::providers::{ProviderBuilder, mock::Asserter};
        let module = Address::repeat_byte(0x11);
        let root = B256::repeat_byte(0xcd);
        let a = Asserter::new();
        a.push_success(&root);
        let p = ProviderBuilder::new().connect_mocked_client(a);
        assert_eq!(fetch_onchain_root(&p, module).await.unwrap(), root);

        let a = Asserter::new();
        a.push_failure_msg("execution reverted");
        let p = ProviderBuilder::new().connect_mocked_client(a);
        match fetch_onchain_root(&p, module).await {
            Err(crate::error::Error::Rpc(m)) => {
                assert!(m.starts_with("allowedInstrRoot: "), "{m}")
            }
            other => panic!("expected an Rpc error, got {other:?}"),
        }
    }
}
