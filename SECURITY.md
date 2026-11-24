# Security

## Reporting a vulnerability

Please report a suspected vulnerability privately, through GitHub's private
vulnerability reporting on this repository: open the repository's **Security**
tab and choose **Report a vulnerability**. Do not open a public issue or pull
request for it.

Include what you can of:

- the affected crate (`makinax` or `makinax-mcp`), version and build
  (read-write or read-only; `makinax-mcp --version` prints all three);
- what an attacker can do and what they need for it;
- steps or a minimal config and tool call that reproduce it.

Leave real keys, passphrases and keyed RPC URLs out of the report.

## Scope

This repository holds the `makinax` library and the `makinax-mcp` server. The
makinaX contracts and third-party services the server talks to (RPC
providers, aggregators, bridges) are not maintained here.
