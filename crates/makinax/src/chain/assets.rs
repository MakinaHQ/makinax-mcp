//! Known asset identities used by legacy asset/funding helpers.
//! CCTP deployment support comes exclusively from signed bridge metadata.
use alloy::primitives::{Address, address};

pub fn usdc_for(chain_id: u64) -> Option<Address> {
    match chain_id {
        1 => Some(address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")),
        8453 => Some(address!("833589fcd6edb6e08f4c7c32d4f71b54bda02913")),
        _ => None,
    }
}
