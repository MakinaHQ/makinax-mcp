# Contributing

This repository holds the `makinax` library (`crates/makinax`) and the
`makinax-mcp` server (`crates/makinax-mcp`). The toolchain is pinned in
`rust-toolchain.toml`.

## Rules a change must keep

- **Neutral.** Nothing here names a consumer or a product. The neutrality
  tests (`crates/makinax/tests/neutrality.rs`, which also scans the whole
  repository, and `crates/makinax-mcp/tests/neutrality.rs`) fail on a
  violation.
- **Stateless.** No database and no state directory: a config file in,
  results out.
- **Enforces nothing.** The library has no policy, no send lock and no
  default pre-send hook. Sending is possible only through an `Executor` built
  with a hook the caller chooses.
- **Owner calls are encoded, never sent.** The one exception is the
  guardian's `pause()` and `unpause()`: the contract lets a guardian key call
  them, so `execute_call` sends exactly those two selectors. Every other owner
  function is refused.
- **Two builds.** The read-only build (`--no-default-features`) compiles
  signing out rather than switching it off. `scripts/assert-readonly.sh`
  checks the read-only server binary for write markers, and with `--live`
  checks that the read-write binary carries every one of them.
- **`makinax::ext` is the supported API.** A change to it is recorded in
  `CHANGELOG.md`. Items outside `ext` may change in any release.
- **Stable paths.** Downstream crates may read these paths from a git
  checkout, so keep them where they are: `crates/makinax/src/exec.rs`,
  `src/build.rs`, `src/chain/module.rs`, `src/owner/`, `src/sim/simulate.rs`
  and `src/instructions/verify.rs`.
- **No ticket or plan references** in code, tests or docs. Say what the code
  does and why.

## Local checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-targets
cargo test --workspace --all-targets --no-default-features
scripts/makinax-coverage.sh
scripts/assert-readonly.sh
```

The tests that need foundry (anvil) or the network are `#[ignore]`d and run
in CI:

```sh
cargo test -p makinax --test exec -- --ignored          # local anvil
cargo test -p makinax --test fork_walk -- --ignored     # anvil fork of Base
cargo test -p makinax --test live_quotes -- --ignored   # live aggregators
```

## Releases

Tag `vX.Y.Z` on a commit of `main` with a green ci run. `release.yml` builds
both variants for macOS arm64 and Linux x86_64, checks that `--version`
prints `makinax-mcp X.Y.Z (<commit>) [read-write]` (and `[read-only]`),
publishes the tarballs, `SHA256SUMS` and `BUILD-INFO` to the public
`MakinaHQ/homebrew-makinax-mcp` repository, and on stable tags rewrites the
formulas in `packaging/homebrew/` with `sync-tap.sh` and commits them to the
tap. The versions in both crates' `Cargo.toml`, `Cargo.lock`, the skill's
front matter and the formulas move together.
