# makinax

This repository holds two crates:

- `makinax` (`crates/makinax`): a library for reading, building, proving, simulating and sending calls to a makinaX module. Its supported API is `makinax::ext`, described below.
- `makinax-mcp` (`crates/makinax-mcp`): an MCP server over that library. It reads a module's state and events, builds and proves operator calls, simulates them, sends them (read-write build only), and encodes owner calls for the Safe owners.

Both keep no state: a config file in, results out.

## Install

Homebrew, macOS on Apple Silicon:

```sh
brew tap MakinaHQ/makinax-mcp
brew install makinax-mcp            # read-write
brew install makinax-mcp-readonly   # read-only: signing is compiled out
```

The two install the same command, `makinax-mcp`, so they conflict: pick one. Linux x86_64 tarballs (glibc 2.39 or newer) are published with each release at https://github.com/MakinaHQ/homebrew-makinax-mcp/releases.

```sh
makinax-mcp --version   # makinax-mcp <version> (<commit>) [read-write] or [read-only]
makinax-mcp tools       # the tool list as JSON
```

To register the server with an MCP host:

```json
{
  "mcpServers": {
    "makinax": { "command": "makinax-mcp", "args": ["--config", "/path/to/config.toml"] }
  }
}
```

The agent skill that describes the tools is `crates/makinax-mcp/skills/makinax/SKILL.md`; Homebrew installs it under `$(brew --prefix)/share/<formula>/skills/makinax/`.

## Config


The config path is `--config <path>`, else `MAKINAX_CONFIG`, else `~/.config/makinax/config.toml` (under `$XDG_CONFIG_HOME` when set). `MAKINAX__<SECTION>__<KEY>` environment variables override single keys, for example `MAKINAX__RPC_URLS__BASE`. A missing default file is an empty config: the server starts, and each module tool says what is missing.

```toml
[rpc_urls]
base = "https://<your-base-rpc>"
# Several endpoints fail over: base = ["https://<primary>", "https://<fallback>"]

[modules.my-module]
chain = "base"
safe = "0x<safe-address>"
module = "0x<module-address>"
rootfile = "local:/path/to/rootfile.toml"
operator_address = "0x<operator-address>"

[modules.my-module.swappers]
kyber = 1   # the swapper id the module maps to this aggregator

# Read-write build only. An encrypted keystore or a GCP KMS key; raw private
# keys are refused.
[signer]
type = "keystore"
path = "/path/to/operator-keystore.json"
passphrase_env = "<ENV_VAR_WITH_THE_PASSPHRASE>"

# Optional.
[fork]
base_anvil_path = "/path/to/base-anvil"   # fork simulation on Base needs Base Anvil

[fetch]
across = false   # turn off a fetcher: kyber, odos, across, cctp_attestation
```

## Tools

Read:
- `list_modules`: the configured modules (no RPC call).
- `get_module_state`: Safe, operating mode, pause and suspension, root, operators and guardians, loss caps, cooldowns, swapper targets, bridge recipients, accounting currency.
- `get_balances`, `get_oracle_price`, `list_actions`.
- `get_module_events`: a block range in chunks; when it stops short, `next_from_block` says where to continue.

Prove and build:
- `check_root`: the rootfile's root against the module's on-chain root.
- `get_proof`: the merkle proof for one action.
- `build_call` (calldata) and `preview_call` (a readable description).

Owner calls (encoded, never sent; a guardian's `pause()` and `unpause()` are the exception, sent with `execute_call`):
- `list_owner_functions`, `encode_owner_call`, `build_owner_batch` (a Safe Transaction Builder batch).

Fetch:
- `get_swap_quote`, `get_bridge_quote` (Across), `get_cctp_attestation` (Circle).

Simulate:
- `simulate_call`, `simulate_owner_batch`.

Send (read-write build only):
- `execute_call`: signs and sends a call to the module, and nowhere else. It refuses owner functions, except the guardian's `pause()` and `unpause()`. It sends at once and requires no simulation. Use one sender per key: the server does not coordinate with other processes using the same key.

## Read-write and read-only

The read-write build serves all 19 tools. The read-only build serves 18: `execute_call` does not exist, and signing is compiled out rather than switched off. In the read-only build `simulate_call` uses `eth_call` instead of a local fork, and `simulate_owner_batch` checks each call on its own with `eth_call` (a later call does not see an earlier one's effects). The output says which backend ran.

From source: `cargo build --release -p makinax-mcp` for read-write, and add `--no-default-features` for read-only.

## The library: `makinax::ext`

`makinax::ext` is the only supported integration API of the `makinax` crate; everything else may change in any release. It re-exports what a caller needs to work with a module:

- Config and errors: `MakinaxConfig`, `ModuleConfig`, `RootfileSource`, `Error`, `Result`.
- Chains and providers: `chains`, `Providers`, `provider_from_endpoints`, `chain_id_for`.
- Reads: `read_module_state`, `read_module_events` (with `EventPage::next_from_block`), `read_balances`, `read_price`, `read_feed_routes`.
- Instructions and proofs: `InstructionLibrary`, `proof_for`, `check_root`.
- Building: `CallSpec`, `build_call`, `build_call_with_cctp`, `build_bridge`, `BuiltCall`, `decode_revert`.
- Owner calls: `encode_owner_call`, the typed `owner_calls::*`, `build_batch`.
- Fetched data: the quote types and the checks a fetched value passes (`validate_bridge_order`, `validate_swap_quote`, `check_field`).
- Simulation (`write`): `simulate`, `ForkManager`, `SafeForkSession`.
- Sending (`write`): `Executor`, `PreSendHook`, `AllowAll`, `build_wallet`.
- Contract bindings: `bindings::IMakinaXModule` and the contracts it talks to.

```rust
use makinax::ext::{CallSpec, InstructionLibrary, MakinaxConfig, Providers, build_call, check_root};

let cfg: MakinaxConfig = toml::from_str(&std::fs::read_to_string("config.toml")?)?;
let module = &cfg.modules["my-module"];
let providers = Providers::from_config(&cfg).await?;
let provider = providers.get(&module.chain)?;
let lib = InstructionLibrary::load(module.rootfile.as_ref().expect("a rootfile")).await?;
check_root(&lib, provider, module.module).await?; // a stale rootfile stops here
let call = build_call(module, &lib, &CallSpec::ManagePosition {
    mgmt_action: "<protocol.action.name>".into(),
    mgmt_params: Default::default(),
    acct_action: None,
    acct_params: Default::default(),
})?;
```

The library enforces nothing and keeps no state. Sending is possible only through an `Executor` built with a pre-send hook of the caller's choosing; there is no default hook, and `AllowAll` must be named to be used. Without the `write` feature there is no way to sign or send.

Deploying a module is done through the Makina module factory directly; this server and library operate existing modules.

## Security model

The library and the server enforce no policy of their own. The limits on a module (who may call it, which actions it allows, how much loss it accepts, where funds may go) live in the on-chain module, and in whatever product embeds the library, through the pre-send hook that product supplies. The server sends only through the signer its operator configures, and the read-only build has no signing code. Errors and logs mask the keys in connection addresses. To report a vulnerability, see [SECURITY.md](SECURITY.md).

## License

Business Source License 1.1; see [LICENSE](LICENSE) for the terms, the change date and the change license.
