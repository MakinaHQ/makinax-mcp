---
name: makinax
version: 0.1.0
description: Operate an existing makinaX module through the makinax-mcp tools. Use to read a module's state and events, build, prove, simulate and send operator calls, and encode owner calls for the Safe owners.
---
# makinaX modules

## What a module is
A makinaX module is a Safe module. It lets named operators run actions for the Safe, but only the actions whose instructions are in a merkle tree whose root the Safe has set (`allowedInstrRoot`). The rootfile lists those actions. Loss caps, cooldowns and the operating mode (OPEN, FENCED, WALLED) limit what an allowed action may do.

## Roles
- Owner: the Safe. Only the Safe can set the root, add or remove operators and guardians, change the mode, caps, cooldowns, feeds, recipients and swapper targets, and sweep what the module itself holds back to the Safe (`sweepNative`, `sweepERC20`, `sweepERC721`, `sweepERC6909`). The Safe owners sign these in the Safe app.
- Operator: an account the Safe added. It sends the module's operator calls: manage a position, harvest, swap, bridge.
- Guardian: can pause and unpause the module. The Safe is always a guardian. A guardian key sends `pause()` or `unpause()` with `execute_call` (kind `raw`, to the module).

## Reading state
- `list_modules` lists the configured modules. `get_module_state` reads one at the latest block. `get_balances`, `get_oracle_price` and `list_actions` read the rest.
- `get_module_events` reads a block range in chunks. When it could not read the whole range, the result has `next_from_block`: call again from there. It is a live read of a range, not a history. Full history needs an indexer.

## Operator calls
1. `check_root`: the rootfile's root must match the module's on-chain root. If it does not, every proof would revert: stop and get the current rootfile.
2. `get_proof` for the action. An action the rootfile does not list is refused.
3. `build_call` (calldata) or `preview_call` (a readable description).
4. Optionally `simulate_call`. The read-only build simulates with eth_call only.
5. `execute_call` sends it (read-write build only). It sends at once to the module and nowhere else.

## Owner calls
Owner calls are encoded, never sent. The one exception is the guardian's `pause()` and `unpause()`, which `execute_call` sends. Use `list_owner_functions`, then `encode_owner_call` for one call or `build_owner_batch` for a Safe Transaction Builder batch (JSON the Safe app imports). `simulate_owner_batch` can check the batch first. Hand the batch to the Safe owners to sign and execute.

## Fetchers
`get_swap_quote`, `get_bridge_quote` and `get_cctp_attestation` are optional; each can be turned off in the `[fetch]` config. Fetched data is checked against the request before anything is built: a quote or order for another target, recipient, token or amount is refused, and the refusal names the field.

## One sender per key
The server does not coordinate with other processes. Use each operator key from one sender only, or nonces will clash.

## Deploying
This tool operates existing modules. A new module is deployed through the Makina module factory directly.
