//! Reads of a Merkl Distributor: its current root, what a user has claimed
//! of a token, and whether a user has redirected a token's claims.
//!
//! The Distributor's address is read from the harvest leaf itself
//! ([`distributor_of`]): it is the target of the leaf's `claim` call, which
//! the module's root commits to. A caller needs no address table of its own,
//! and cannot read a different contract from the one a claim would call.

use alloy::{
    network::Ethereum,
    primitives::{Address, B256, FixedBytes, U256},
    providers::Provider,
    sol,
    sol_types::SolCall,
};

use crate::{
    error::{Error, Result},
    instructions::Action,
};

sol! {
    /// The Merkl Distributor's claim entry point and the views a claim depends on.
    #[sol(rpc)]
    interface IMerklDistributor {
        struct Claim {
            uint208 amount;
            uint48 timestamp;
            bytes32 merkleRoot;
        }

        function claim(
            address[] calldata users,
            address[] calldata tokens,
            uint256[] calldata amounts,
            bytes32[][] calldata proofs
        ) external;

        function getMerkleRoot() external view returns (bytes32);

        function claimed(address user, address token)
            external view returns (uint208 amount, uint48 timestamp, bytes32 merkleRoot);

        function claimRecipient(address user, address token) external view returns (address);
    }
}

/// What a user has claimed of one token, as the Distributor records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClaimedRecord {
    /// Cumulative amount claimed.
    pub amount: U256,
    /// When the last claim was made (unix seconds; 0 if never).
    pub timestamp: u64,
    /// The root the last claim was proven under.
    pub merkle_root: B256,
}

/// weiroll call-type bits of a command's flags byte, and the CALL value.
const CALLTYPE_MASK: u8 = 0x03;
const CALLTYPE_CALL: u8 = 0x01;

/// The Distributor a harvest leaf claims from: the target of its one
/// `claim(address[],address[],uint256[],bytes32[][])` CALL command (bytes
/// 12..32 of the command word).
///
/// Refused when the leaf has no such command, or claims from more than one
/// target.
pub fn distributor_of(action: &Action) -> Result<Address> {
    let selector = FixedBytes::<4>::from(IMerklDistributor::claimCall::SELECTOR);
    let mut found: Option<Address> = None;
    for command in &action.instruction.commands {
        if command[0..4] != selector[..] || command[4] & CALLTYPE_MASK != CALLTYPE_CALL {
            continue;
        }
        let target = Address::from_slice(&command[12..32]);
        match found {
            Some(prior) if prior != target => {
                return Err(Error::InvalidParams(format!(
                    "action `{}` claims from more than one Merkl Distributor ({prior} and {target})",
                    action.qualified_name
                )));
            }
            _ => found = Some(target),
        }
    }
    found.ok_or_else(|| {
        Error::InvalidParams(format!(
            "action `{}` has no Merkl Distributor claim call",
            action.qualified_name
        ))
    })
}

/// `getMerkleRoot()`: the root claims are proven under right now.
pub async fn read_merkle_root<P: Provider<Ethereum>>(
    provider: &P,
    distributor: Address,
) -> Result<B256> {
    IMerklDistributor::new(distributor, provider)
        .getMerkleRoot()
        .call()
        .await
        .map_err(|e| Error::rpc(format!("merkl getMerkleRoot: {e}")))
}

/// `claimed(user, token)`.
pub async fn read_claimed<P: Provider<Ethereum>>(
    provider: &P,
    distributor: Address,
    user: Address,
    token: Address,
) -> Result<ClaimedRecord> {
    let r = IMerklDistributor::new(distributor, provider)
        .claimed(user, token)
        .call()
        .await
        .map_err(|e| Error::rpc(format!("merkl claimed: {e}")))?;
    Ok(ClaimedRecord {
        amount: U256::from(r.amount),
        timestamp: r.timestamp.to(),
        merkle_root: r.merkleRoot,
    })
}

/// `claimRecipient(user, token)`: where a claim of `token` for `user` is
/// paid, if `user` has redirected it; zero when it goes to `user`.
/// `token = address(0)` reads the redirect `user` set for every token; a
/// caller that needs "paid to `user`" checks it as well as the token's own.
pub async fn read_claim_recipient<P: Provider<Ethereum>>(
    provider: &P,
    distributor: Address,
    user: Address,
    token: Address,
) -> Result<Address> {
    IMerklDistributor::new(distributor, provider)
        .claimRecipient(user, token)
        .call()
        .await
        .map_err(|e| Error::rpc(format!("merkl claimRecipient: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selectors the leaf and the live Distributor use.
    #[test]
    fn selectors_are_the_distributors() {
        assert_eq!(
            IMerklDistributor::claimCall::SELECTOR,
            [0x71, 0xee, 0x95, 0xc0]
        );
        assert_eq!(
            IMerklDistributor::getMerkleRootCall::SELECTOR,
            [0x49, 0x59, 0x06, 0x57]
        );
        assert_eq!(
            IMerklDistributor::claimedCall::SELECTOR,
            [0x0c, 0x9c, 0xbf, 0x0e]
        );
        assert_eq!(
            IMerklDistributor::claimRecipientCall::SELECTOR,
            [0x4d, 0x27, 0x2d, 0x95]
        );
    }

    /// The Base answer to `claimed(caliber, SUMR)` decodes into the record.
    #[test]
    fn a_claimed_answer_decodes() {
        let raw = alloy::primitives::hex::decode(
            "0000000000000000000000000000000000000000000000038e20c8bc5760cad7\
             0000000000000000000000000000000000000000000000000000000069652511\
             ac055ebcceaa5af09fe377ccfe8d6b6259f91d7e6da7cdc325e80f82c89b07db",
        )
        .unwrap();
        let r = IMerklDistributor::claimedCall::abi_decode_returns(&raw).unwrap();
        assert_eq!(U256::from(r.amount), U256::from(65581638485014530775u128));
        assert_eq!(r.timestamp.to::<u64>(), 0x69652511);
    }
}
