# makinax-mcp

An MCP server for operating an existing makinaX module, built on the `makinax` library. It reads one config file and serves its tools over stdio.

Install, config, the tool list and the library's `ext` API are described in the repository README (`../../README.md`). The agent skill is `skills/makinax/SKILL.md`.

From source: `cargo build --release -p makinax-mcp` for the read-write build, and add `--no-default-features` for the read-only build, which has no `execute_call` and no signing code.
