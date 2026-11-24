#!/usr/bin/env bash
# Asserts the read-only makinax-mcp binary links no signing or write code.
#
# Usage:
#   scripts/assert-readonly.sh              # builds the read-only binary, then asserts
#   scripts/assert-readonly.sh <path-to-bin> # asserts an existing binary
#   scripts/assert-readonly.sh --live <path-to-read-write-bin>
#       # the reverse check: every marker IS in the read-write binary, so a
#       # marker that stopped being linked cannot keep this guard green
#
# With no argument the read-only binary is built into its OWN target dir
# (target/readonly-assert, or $READONLY_ASSERT_TARGET_DIR), so it can never
# race with or scan a read-write artifact left in target/release by another
# build. crates/makinax-mcp/tests/readonly_assert.rs pins every marker below.
set -euo pipefail

LIVE=0
if [[ "${1:-}" == "--live" ]]; then
  LIVE=1
  shift
  [[ -n "${1:-}" ]] || { echo "usage: $0 --live <path-to-read-write-bin>"; exit 2; }
fi
BIN="${1:-}"
if [[ -z "$BIN" ]]; then
  TARGET_DIR="${READONLY_ASSERT_TARGET_DIR:-target/readonly-assert}"
  cargo build -p makinax-mcp --bin makinax-mcp --release --no-default-features --target-dir "$TARGET_DIR"
  BIN="$TARGET_DIR/release/makinax-mcp"
fi
[[ -f "$BIN" ]] || { echo "binary not found: $BIN"; exit 2; }


# Markers, each present in the read-write build only:
#  - GCP KMS signer endpoints (makinax's signer construction)
#  - the keystore decrypt error (makinax's signer construction)
#  - execute_call's description
#  - the service's signer-construction log line
#  - the fork session that runs an owner batch as the Safe
MARKERS=(
  "cloudkms"
  "kms.googleapis"
  "keystore decrypt"
  "Sends immediately. No simulation is required."
  "the configured signer could not be built"
  "anvil_impersonateAccount"
)

version_out=$("$BIN" --version)
echo "$version_out"

if [[ "$LIVE" -eq 1 ]]; then
  if [[ "$version_out" != *"[read-write]"* ]]; then
    echo "marker liveness FAILED — $BIN identifies as: $version_out"
    exit 1
  fi
  missing=0
  for p in "${MARKERS[@]}"; do
    if ! strings "$BIN" | grep -F "$p" > /dev/null; then
      echo "marker absent from the read-write binary: '$p'"
      missing=1
    fi
  done
  if [[ "$missing" -ne 0 ]]; then
    echo "marker liveness FAILED — a marker that is never linked proves nothing"
    exit 1
  fi
  echo "OK: every marker is in the read-write binary $BIN"
  exit 0
fi

# Positive identification first: the binary must say it is the read-only
# variant, which catches asserting the wrong artifact.
if [[ "$version_out" != *"[read-only]"* ]]; then
  echo "read-only assertion FAILED — $BIN identifies as: $version_out"
  exit 1
fi

fail=0
for p in "${MARKERS[@]}"; do
  # No `grep -q`: with pipefail, -q exits on the first match and kills
  # `strings` with SIGPIPE, so the pipeline would report failure on a HIT.
  if strings "$BIN" | grep -F "$p" > /dev/null; then
    echo "FORBIDDEN marker present in read-only binary: '$p'"
    fail=1
  fi
done

if [[ "$fail" -ne 0 ]]; then
  echo "read-only assertion FAILED — write/signing code is linked into $BIN"
  exit 1
fi
echo "OK: no signing/write markers in $BIN"
