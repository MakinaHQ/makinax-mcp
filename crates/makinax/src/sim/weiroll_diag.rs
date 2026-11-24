//! Why a weiroll command reverted with nothing to say.
//!
//! The VM reports `ExecutionFailed(command_index, target, message)`. When the
//! target is an old-style `require` with no string — WETH9's `withdraw` and
//! `transferFrom` are both that shape — `message` is empty, and the caller is
//! told only that something failed. A gas top-up that is short by a hair and a
//! gas top-up aimed at the wrong forwarder read identically.
//!
//! This recovers the command list from the calldata that was about to be sent
//! and asks each token the instruction moves for the Safe's balance, so the
//! answer can be "needs 0.0005 WETH, holds 0.00003". Nothing here gates a
//! send: it runs only after a failure, and only to say more about it.

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};

/// One weiroll command, resolved against the instruction's state.
///
/// Command word layout: `[0..4]` selector, `[4]` flags, `[5..11]` six input
/// state indices (`0xFF` ends the list), `[11]` output index, `[12..32]` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandCall {
    pub index: usize,
    pub selector: [u8; 4],
    pub target: Address,
    /// The state words fed to the call, in argument order. A word the state
    /// does not carry is omitted rather than guessed.
    pub args: Vec<Vec<u8>>,
}

/// The `(address,uint256)` signatures a module instruction set commonly emits. Only for these is
/// the second word CALLED an amount; every other command's words are reported
/// as words, with no claim about what they mean.
fn known_signature(selector: &[u8; 4]) -> Option<&'static str> {
    for sig in [
        "approve(address,uint256)",
        "transfer(address,uint256)",
        "unwrapAndSend(address,uint256)",
    ] {
        if keccak256(sig)[..4] == selector[..] {
            return Some(sig);
        }
    }
    None
}

/// Resolve every command against the state words it references.
pub(crate) fn decode_commands(commands: &[B256], state: &[Bytes]) -> Vec<CommandCall> {
    commands
        .iter()
        .enumerate()
        .map(|(index, cmd)| {
            let c = cmd.as_slice();
            let mut args = Vec::new();
            for b in &c[5..11] {
                if *b == 0xFF {
                    break;
                }
                if let Some(word) = state.get(usize::from(b & 0x7F)) {
                    args.push(word.to_vec());
                }
            }
            CommandCall {
                index,
                selector: [c[0], c[1], c[2], c[3]],
                target: Address::from_slice(&c[12..32]),
                args,
            }
        })
        .collect()
}

/// What the instruction asks a token to move, for the calls whose signature is
/// known AND whose target is the token itself — `approve` and `transfer`. A
/// forwarder call like `unwrapAndSend` names an amount but not a token, so it
/// is deliberately absent: its shortfall shows up on the token the paired
/// `approve` names.
///
/// `failing_target` is the address weiroll said reverted, and it is a filter,
/// not decoration. An `approve` succeeds on a balance it exceeds — ERC-20
/// approval checks nothing — so an instruction that approves 1000 and then
/// moves 10 would otherwise have every unrelated failure reported as "the Safe
/// is 990 short", which is a plausible wrong answer and worse than the empty
/// reason it replaced. An amount is claimed only when the revert came from the
/// token itself, or from the exact address that token was asked to approve.
pub(crate) fn requested_amounts(
    calls: &[CommandCall],
    failing_target: Address,
) -> Vec<(Address, U256)> {
    let spender = |call: &CommandCall| -> Option<Address> {
        let word = call.args.first().filter(|w| w.len() == 32)?;
        (word[..12] == [0u8; 12]).then(|| Address::from_slice(&word[12..]))
    };
    let mut out: Vec<(Address, U256)> = Vec::new();
    for call in calls {
        let Some(sig) = known_signature(&call.selector) else {
            continue;
        };
        if sig.starts_with("unwrapAndSend") {
            continue;
        }
        if call.target != failing_target && spender(call) != Some(failing_target) {
            continue;
        }
        let Some(word) = call.args.get(1).filter(|w| w.len() == 32) else {
            continue;
        };
        let amount = U256::from_be_slice(word);
        if amount.is_zero() || amount == U256::MAX {
            continue;
        }
        match out.iter_mut().find(|(t, _)| *t == call.target) {
            Some((_, a)) if *a < amount => *a = amount,
            Some(_) => {}
            None => out.push((call.target, amount)),
        }
    }
    out
}

/// The instructions inside a built module call, whichever entrypoint it uses.
pub(crate) fn instructions_in(
    data: &[u8],
) -> Vec<crate::chain::module::IMakinaXModule::Instruction> {
    use crate::chain::module::IMakinaXModule as M;
    use alloy::sol_types::SolCall;
    if let Ok(c) = M::managePositionCall::abi_decode(data) {
        return vec![c.mgmt, c.acct];
    }
    if let Ok(c) = M::harvestCall::abi_decode(data) {
        return vec![c.instruction];
    }
    Vec::new()
}

/// A token balance behind a silent revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shortfall {
    pub token: Address,
    pub symbol: String,
    pub decimals: u8,
    pub held: U256,
    pub needed: U256,
}

/// Render base units with a decimal point, trailing zeros trimmed. Presentation
/// only — the base-unit integers travel alongside it.
pub fn units(value: U256, decimals: u8) -> String {
    let d = usize::from(decimals);
    if d == 0 {
        return value.to_string();
    }
    let digits = format!("{value:0width$}", width = d + 1);
    let (whole, frac) = digits.split_at(digits.len() - d);
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

impl Shortfall {
    pub fn sentence(&self) -> String {
        format!(
            "the Safe holds {} {} and this needs {}",
            units(self.held, self.decimals),
            self.symbol,
            units(self.needed, self.decimals)
        )
    }
}

/// Ask each named token for the Safe's balance and keep the ones that are
/// short. Bounded by the instruction's own command list; runs only on the
/// failure path.
pub(crate) async fn shortfalls(
    provider: &crate::chain::RootProvider,
    safe: Address,
    wanted: &[(Address, U256)],
) -> Vec<Shortfall> {
    use crate::chain::module::IERC20;
    let mut out = Vec::new();
    for (token, needed) in wanted.iter().take(4) {
        let erc20 = IERC20::new(*token, provider);
        let Ok(held) = erc20.balanceOf(safe).call().await else {
            continue;
        };
        if held >= *needed {
            continue;
        }
        out.push(Shortfall {
            token: *token,
            symbol: erc20
                .symbol()
                .call()
                .await
                .unwrap_or_else(|_| format!("{token:?}")),
            decimals: erc20.decimals().call().await.unwrap_or(18),
            held,
            needed: *needed,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const WETH: Address = Address::new([0x11; 20]);
    const FORWARDER: Address = Address::new([0x22; 20]);

    /// The gas-top-up blueprint: `approve` on the wrapped token, then
    /// `unwrapAndSend` on the forwarder, both reading the same amount word.
    fn top_up(amount: U256) -> (Vec<B256>, Vec<Bytes>) {
        let mut spender = [0u8; 32];
        spender[12..].copy_from_slice(FORWARDER.as_slice());
        let mut recipient = [0u8; 32];
        recipient[12..].copy_from_slice(&[0x33; 20]);
        let state = vec![
            Bytes::from(spender),
            Bytes::from(amount.to_be_bytes::<32>()),
            Bytes::from(recipient),
        ];
        let command = |sig: &str, args: [u8; 2], target: Address| {
            let mut w = [0u8; 32];
            w[..4].copy_from_slice(&keccak256(sig)[..4]);
            w[4] = 1;
            w[5] = args[0];
            w[6] = args[1];
            w[7] = 0xFF;
            w[11] = 0xFF;
            w[12..].copy_from_slice(target.as_slice());
            B256::from(w)
        };
        (
            vec![
                command("approve(address,uint256)", [0, 1], WETH),
                command("unwrapAndSend(address,uint256)", [2, 1], FORWARDER),
            ],
            state,
        )
    }

    #[test]
    fn a_command_resolves_to_the_state_words_it_feeds_its_target() {
        let (commands, state) = top_up(U256::from(500_000_000_000_000u64));
        let calls = decode_commands(&commands, &state);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].target, WETH);
        assert_eq!(calls[1].target, FORWARDER);
        // 0xFF ended the list at byte 7, so exactly two words, not six.
        assert_eq!(calls[0].args.len(), 2);
        assert_eq!(
            U256::from_be_slice(&calls[0].args[1]),
            U256::from(500_000_000_000_000u64)
        );
    }

    #[test]
    fn only_a_call_whose_target_is_the_token_names_an_amount_for_that_token() {
        let (commands, state) = top_up(U256::from(500_000_000_000_000u64));
        let wanted = requested_amounts(&decode_commands(&commands, &state), FORWARDER);
        // The forwarder call carries the same amount but the forwarder is not
        // a token; claiming it would send the probe to a contract with no
        // balanceOf and lose the real answer.
        assert_eq!(wanted, vec![(WETH, U256::from(500_000_000_000_000u64))]);
    }

    #[test]
    fn an_unknown_selector_contributes_no_amount() {
        let (mut commands, state) = top_up(U256::from(1u64));
        let mut w = commands[0].0;
        w[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        commands[0] = B256::from(w);
        assert!(
            requested_amounts(&decode_commands(&commands, &state), WETH)
                .iter()
                .all(|(t, _)| *t != WETH)
        );
    }

    #[test]
    fn an_infinite_approval_is_not_a_shortfall() {
        let (commands, state) = top_up(U256::MAX);
        assert!(requested_amounts(&decode_commands(&commands, &state), FORWARDER).is_empty());
    }

    /// The false-positive guard: a revert somewhere else in the instruction
    /// must not be explained by an approval this Safe happens not to cover.
    #[test]
    fn an_unrelated_target_is_not_explained_by_the_approval() {
        let (commands, state) = top_up(U256::from(500_000_000_000_000u64));
        let elsewhere = Address::new([0x44; 20]);
        assert!(
            requested_amounts(&decode_commands(&commands, &state), elsewhere).is_empty(),
            "only the token, or the address it approved, may be named"
        );
        // And the token's own revert still is.
        assert_eq!(
            requested_amounts(&decode_commands(&commands, &state), WETH),
            vec![(WETH, U256::from(500_000_000_000_000u64))]
        );
    }

    #[test]
    fn base_units_render_without_inventing_precision() {
        assert_eq!(units(U256::from(500_000_000_000_000u64), 18), "0.0005");
        assert_eq!(units(U256::from(30_000_000_000_000u64), 18), "0.00003");
        assert_eq!(units(U256::from(1_000_000u64), 6), "1");
        assert_eq!(units(U256::from(7u64), 0), "7");
    }

    #[test]
    fn the_sentence_says_both_numbers() {
        let s = Shortfall {
            token: WETH,
            symbol: "WETH".into(),
            decimals: 18,
            held: U256::from(30_000_000_000_000u64),
            needed: U256::from(500_000_000_000_000u64),
        };
        assert_eq!(
            s.sentence(),
            "the Safe holds 0.00003 WETH and this needs 0.0005"
        );
    }
}
