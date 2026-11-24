//! Read-only, block-pinned checks that CCTP deployment facts (a
//! [`CctpRoute`]) match the chain: every address has code, every contract
//! names the others, and the module's encoder builds the burn the route
//! implies.

// `sol!` generates one rpc builder argument per Solidity parameter, and
// `depositForBurnWithHook` has eight.
#![allow(clippy::too_many_arguments)]

use super::cctp::{CctpDeployment as Deployment, CctpRoute as Route, IMessageTransmitterV2};
use crate::chain::RootProvider;
use crate::{
    Error, Result,
    chain::module::{IBridgeEncoderRegistry, IERC20, IMakinaXModule},
};
use alloy::{
    primitives::{Address, B256, Bytes, U256},
    providers::Provider,
    sol,
    sol_types::SolCall,
};

sol! {
    #[sol(rpc)]
    interface ITokenMessengerFacts {
        function localMessageTransmitter() external view returns (address);
        function localMinter() external view returns (address);
        function remoteTokenMessengers(uint32 domain) external view returns (bytes32);
        function depositForBurnWithHook(uint256 amount, uint32 destinationDomain, bytes32 mintRecipient,
            address burnToken, bytes32 destinationCaller, uint256 maxFee, uint32 minFinalityThreshold, bytes hookData) external;
    }
    #[sol(rpc)]
    interface ITokenMinterFacts {
        function getLocalToken(uint32 remoteDomain, bytes32 remoteToken) external view returns (address);
    }
    struct EncoderOrder {
        uint16 bridgeId;
        uint256 destinationChainId;
        address recipient;
        address inputToken;
        uint256 inputAmount;
        uint256 minOutputAmount;
        bytes extraData;
    }
    #[sol(rpc)]
    interface ICctpEncoderFacts {
        function cctpV2TokenMessenger() external view returns (address);
        function getCctpDomain(uint256 chainId) external view returns (uint32);
        function getBridgeTransferData(EncoderOrder order) external view
            returns (address target, address spender, uint256 value, bytes data);
    }
}

/// The refusal [`verify_endpoint`] gives when the RPC serves another chain
/// than the deployment's. Exported so a consumer can match it exactly.
pub const RPC_CHAIN_DIFFERS: &str =
    "CCTP deployment: RPC chain differs from the route's deployment";
/// The refusal [`verify_route`] gives when the module's registry is not the
/// route's. Exported so a consumer can match it exactly.
pub const MODULE_REGISTRY_DIFFERS: &str =
    "CCTP deployment: module registry differs from the route's deployment";

fn refusal(message: impl Into<String>) -> Error {
    Error::InvalidParams(format!("CCTP deployment: {}", message.into()))
}
fn rpc(error: impl std::fmt::Display) -> Error {
    Error::rpc(format!("CCTP deployment read failed: {error}"))
}

pub async fn verify_endpoint(
    provider: &RootProvider,
    chain: u64,
    deployment: &Deployment,
) -> Result<u64> {
    if provider.get_chain_id().await.map_err(rpc)? != chain {
        return Err(Error::InvalidParams(RPC_CHAIN_DIFFERS.into()));
    }
    let block = provider.get_block_number().await.map_err(rpc)?;
    for address in [
        deployment.usdc,
        deployment.token_messenger,
        deployment.message_transmitter,
    ] {
        if provider
            .get_code_at(address)
            .block_id(block.into())
            .await
            .map_err(rpc)?
            .is_empty()
        {
            return Err(refusal(format!("no code at {address} on chain {chain}")));
        }
    }
    let transmitter = IMessageTransmitterV2::new(deployment.message_transmitter, provider);
    if transmitter
        .localDomain()
        .block(block.into())
        .call()
        .await
        .map_err(rpc)?
        != deployment.domain
        || transmitter
            .version()
            .block(block.into())
            .call()
            .await
            .map_err(rpc)?
            != 1
        || ITokenMessengerFacts::new(deployment.token_messenger, provider)
            .localMessageTransmitter()
            .block(block.into())
            .call()
            .await
            .map_err(rpc)?
            != deployment.message_transmitter
        || IERC20::new(deployment.usdc, provider)
            .decimals()
            .block(block.into())
            .call()
            .await
            .map_err(rpc)?
            != 6
    {
        return Err(refusal(
            "domain, version, transmitter relationship or USDC decimals mismatch",
        ));
    }
    Ok(block)
}

/// Claim recovery needs the destination Circle deployment, not a still-enabled source encoder.
pub async fn verify_destination(destination: &RootProvider, route: &Route) -> Result<u64> {
    route.validate().map_err(refusal)?;
    let block = verify_endpoint(destination, route.to_chain, &route.destination).await?;
    let messenger = ITokenMessengerFacts::new(route.destination.token_messenger, destination);
    if messenger
        .remoteTokenMessengers(route.source.domain)
        .block(block.into())
        .call()
        .await
        .map_err(rpc)?
        != route.source.token_messenger.into_word()
    {
        return Err(refusal(
            "destination does not recognize recorded source messenger",
        ));
    }
    let minter = messenger
        .localMinter()
        .block(block.into())
        .call()
        .await
        .map_err(rpc)?;
    if minter.is_zero()
        || ITokenMinterFacts::new(minter, destination)
            .getLocalToken(route.source.domain, route.source.usdc.into_word())
            .block(block.into())
            .call()
            .await
            .map_err(rpc)?
            != route.destination.usdc
    {
        return Err(refusal("destination token differs from recorded bridge"));
    }
    Ok(block)
}

pub async fn verify_route(
    source: &RootProvider,
    destination: &RootProvider,
    module: Option<Address>,
    route: &Route,
) -> Result<(u64, u64)> {
    route.validate().map_err(refusal)?;
    let sb = verify_endpoint(source, route.from_chain, &route.source).await?;
    let db = verify_endpoint(destination, route.to_chain, &route.destination).await?;
    if let Some(module) = module
        && IMakinaXModule::new(module, source)
            .registry()
            .block(sb.into())
            .call()
            .await
            .map_err(rpc)?
            != route.source.registry
    {
        return Err(Error::InvalidParams(MODULE_REGISTRY_DIFFERS.into()));
    }
    let encoder = ICctpEncoderFacts::new(route.source.encoder, source);
    if IBridgeEncoderRegistry::new(route.source.registry, source)
        .getBridgeEncoder(route.source.bridge_id)
        .block(sb.into())
        .call()
        .await
        .map_err(rpc)?
        != route.source.encoder
        || encoder
            .cctpV2TokenMessenger()
            .block(sb.into())
            .call()
            .await
            .map_err(rpc)?
            != route.source.token_messenger
        || encoder
            .getCctpDomain(U256::from(route.to_chain))
            .block(sb.into())
            .call()
            .await
            .map_err(rpc)?
            != route.destination.domain
    {
        return Err(refusal(
            "registry encoder, messenger or destination domain mismatch",
        ));
    }
    for (provider, block, local, remote) in [
        (source, sb, &route.source, &route.destination),
        (destination, db, &route.destination, &route.source),
    ] {
        if ITokenMessengerFacts::new(local.token_messenger, provider)
            .remoteTokenMessengers(remote.domain)
            .block(block.into())
            .call()
            .await
            .map_err(rpc)?
            != remote.token_messenger.into_word()
        {
            return Err(refusal("remote TokenMessenger relationship mismatch"));
        }
    }
    let minter = ITokenMessengerFacts::new(route.destination.token_messenger, destination)
        .localMinter()
        .block(db.into())
        .call()
        .await
        .map_err(rpc)?;
    if minter.is_zero()
        || ITokenMinterFacts::new(minter, destination)
            .getLocalToken(route.source.domain, route.source.usdc.into_word())
            .block(db.into())
            .call()
            .await
            .map_err(rpc)?
            != route.destination.usdc
    {
        return Err(refusal(
            "destination minter does not map the source token to declared USDC",
        ));
    }
    Ok((sb, db))
}

/// The registry may point to an upgraded encoder. Validate what it actually builds.
pub async fn verify_encoding(
    source: &RootProvider,
    block: u64,
    route: &Route,
    recipient: Address,
    amount: U256,
    minimum: U256,
) -> Result<()> {
    let finality = route.finality;
    let output = ICctpEncoderFacts::new(route.source.encoder, source)
        .getBridgeTransferData(EncoderOrder {
            bridgeId: route.source.bridge_id,
            destinationChainId: U256::from(route.to_chain),
            recipient,
            inputToken: route.source.usdc,
            inputAmount: amount,
            minOutputAmount: minimum,
            extraData: super::cctp::encode_extra_data(finality),
        })
        .block(block.into())
        .call()
        .await
        .map_err(rpc)?;
    let expected = ITokenMessengerFacts::depositForBurnWithHookCall {
        amount,
        destinationDomain: route.destination.domain,
        mintRecipient: recipient.into_word(),
        burnToken: route.source.usdc,
        destinationCaller: B256::ZERO,
        maxFee: amount
            .checked_sub(minimum)
            .ok_or_else(|| refusal("output floor exceeds input"))?,
        minFinalityThreshold: finality.min_finality_threshold(),
        hookData: Bytes::from(
            alloy::hex::decode("636374702d666f72776172640000000000000000000000000000000000000000")
                .expect("constant hook bytes"),
        ),
    }
    .abi_encode();
    if output.target != route.source.token_messenger
        || output.spender != route.source.token_messenger
        || !output.value.is_zero()
        || output.data.as_ref() != expected
    {
        return Err(refusal(
            "encoder output differs from reviewed CCTP burn semantics",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridges::cctp::{CctpCapabilities, CctpFinality};
    use alloy::{providers::ProviderBuilder, sol_types::SolValue, transports::mock::Asserter};

    fn deployment(domain: u32, byte: u8) -> Deployment {
        Deployment {
            domain,
            usdc: Address::repeat_byte(byte),
            token_messenger: Address::repeat_byte(byte + 1),
            message_transmitter: Address::repeat_byte(byte + 2),
            registry: Address::repeat_byte(byte + 3),
            encoder: Address::repeat_byte(byte + 4),
            bridge_id: 3,
            capabilities: CctpCapabilities {
                standard: true,
                fast: false,
            },
        }
    }
    /// A synthetic directed route: chain 8453 (domain 6) → chain 1 (domain 0).
    fn route() -> Route {
        Route {
            from_chain: 8453,
            to_chain: 1,
            source: deployment(6, 0x10),
            destination: deployment(0, 0x40),
            finality: CctpFinality::Standard,
            message_sender: None,
        }
    }
    fn endpoint(chain: u64, d: &Deployment) -> Vec<String> {
        vec![
            format!("0x{chain:x}"),
            "0x2a".into(),
            "0x01".into(),
            "0x01".into(),
            "0x01".into(),
            word(U256::from(d.domain)),
            word(U256::from(1)),
            word(d.message_transmitter),
            word(U256::from(6)),
        ]
    }
    fn word(v: impl SolValue) -> String {
        alloy::hex::encode_prefixed(v.abi_encode())
    }
    fn provider(responses: &[String]) -> RootProvider {
        let a = Asserter::new();
        for response in responses {
            a.push_success(response);
        }
        ProviderBuilder::new().connect_mocked_client(a).erased()
    }
    fn answers(r: &Route) -> (Vec<String>, Vec<String>) {
        let mut src = endpoint(r.from_chain, &r.source);
        src.extend([
            word(r.source.registry),
            word(r.source.encoder),
            word(r.source.token_messenger),
            word(U256::from(r.destination.domain)),
            word(r.destination.token_messenger.into_word()),
        ]);
        let mut dst = endpoint(r.to_chain, &r.destination);
        dst.extend([
            word(r.source.token_messenger.into_word()),
            word(Address::repeat_byte(0x77)),
            word(r.destination.usdc),
        ]);
        (src, dst)
    }

    /// Synthetic RPC fixtures exercise the real verifier: matching facts are
    /// accepted, and every single mismatched answer is refused.
    #[tokio::test]
    async fn a_route_accepts_matching_rpc_facts_and_refuses_each_mismatch() {
        let route = route();
        let (src, dst) = answers(&route);
        assert_eq!(
            verify_route(
                &provider(&src),
                &provider(&dst),
                Some(Address::repeat_byte(9)),
                &route
            )
            .await
            .unwrap(),
            (42, 42)
        );
        for source in [true, false] {
            let original = if source { &src } else { &dst };
            for i in 0..original.len() {
                if i == 1 || (!source && i == 10) {
                    continue;
                } // Any readable block height is valid.
                let mut bad = original.clone();
                bad[i] = match i {
                    0 => "0xffff".into(),
                    2..=4 => "0x".into(),
                    _ => word(U256::MAX),
                };
                let (s, d) = if source { (&bad, &dst) } else { (&src, &bad) };
                assert!(
                    verify_route(
                        &provider(s),
                        &provider(d),
                        Some(Address::repeat_byte(9)),
                        &route
                    )
                    .await
                    .is_err(),
                    "source {source} response {i}"
                );
            }
        }
        // Claim recovery is independent of source registry/encoder availability.
        assert_eq!(
            verify_destination(&provider(&dst), &route).await.unwrap(),
            42
        );
    }

    /// One deployment against one endpoint: matching facts return the block
    /// they were read at; a different chain, an address with no code, or a
    /// wrong domain is refused.
    #[tokio::test]
    async fn verify_endpoint_checks_one_deployment_at_one_block() {
        let d = deployment(6, 0x10);
        let good = endpoint(8453, &d);
        assert_eq!(
            verify_endpoint(&provider(&good), 8453, &d).await.unwrap(),
            42
        );
        let e = verify_endpoint(&provider(&good), 1, &d).await.unwrap_err();
        assert!(
            matches!(&e, Error::InvalidParams(m) if m == RPC_CHAIN_DIFFERS),
            "{e}"
        );
        let mut no_code = good.clone();
        no_code[3] = "0x".into();
        let e = verify_endpoint(&provider(&no_code), 8453, &d)
            .await
            .unwrap_err();
        assert!(
            e.to_string()
                .contains(&format!("no code at {}", d.token_messenger)),
            "{e}"
        );
        let mut wrong_domain = good;
        wrong_domain[5] = word(U256::from(0));
        assert!(
            verify_endpoint(&provider(&wrong_domain), 8453, &d)
                .await
                .is_err()
        );
    }

    /// The two refusals a consumer may reword are the exported constants.
    #[tokio::test]
    async fn the_named_refusals_are_the_exported_constants() {
        let route = route();
        let (mut src, dst) = answers(&route);
        src[0] = "0x1".into();
        let e = verify_route(&provider(&src), &provider(&dst), None, &route)
            .await
            .unwrap_err();
        assert!(
            matches!(&e, Error::InvalidParams(m) if m == RPC_CHAIN_DIFFERS),
            "{e}"
        );

        let (mut src, dst) = answers(&route);
        src[9] = word(Address::repeat_byte(0x99));
        let e = verify_route(
            &provider(&src),
            &provider(&dst),
            Some(Address::repeat_byte(9)),
            &route,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(&e, Error::InvalidParams(m) if m == MODULE_REGISTRY_DIFFERS),
            "{e}"
        );
    }

    #[tokio::test]
    async fn encoder_calldata_is_checked() {
        let route = route();
        let recipient = Address::repeat_byte(0x33);
        let amount = U256::from(5_000_000);
        let minimum = U256::from(4_950_000);
        let call = ITokenMessengerFacts::depositForBurnWithHookCall {
            amount,
            destinationDomain: route.destination.domain,
            mintRecipient: recipient.into_word(),
            burnToken: route.source.usdc,
            destinationCaller: B256::ZERO,
            maxFee: amount - minimum,
            minFinalityThreshold: 2000,
            hookData: alloy::hex::decode(
                "636374702d666f72776172640000000000000000000000000000000000000000",
            )
            .unwrap()
            .into(),
        }
        .abi_encode();
        let response = |target, spender, value, data: Vec<u8>| {
            alloy::hex::encode_prefixed(
                (target, spender, value, Bytes::from(data)).abi_encode_params(),
            )
        };
        let good = response(
            route.source.token_messenger,
            route.source.token_messenger,
            U256::ZERO,
            call.clone(),
        );
        assert!(
            verify_encoding(&provider(&[good]), 42, &route, recipient, amount, minimum)
                .await
                .is_ok()
        );
        for field in 0..4 {
            let mut data = call.clone();
            if field == 3 {
                data[4] ^= 1;
            }
            let bad = response(
                if field == 0 {
                    Address::ZERO
                } else {
                    route.source.token_messenger
                },
                if field == 1 {
                    Address::ZERO
                } else {
                    route.source.token_messenger
                },
                if field == 2 {
                    U256::from(1)
                } else {
                    U256::ZERO
                },
                data,
            );
            assert!(
                verify_encoding(&provider(&[bad]), 42, &route, recipient, amount, minimum)
                    .await
                    .is_err()
            );
        }
    }
}
