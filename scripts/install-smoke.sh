#!/usr/bin/env bash
# Install a formula against a locally built tarball and run the formula's own
# test block, so the INSTALLED package is executed before it ships.
#
#   ./scripts/install-smoke.sh dist/makinax-mcp-aarch64-apple-darwin.tar.xz \
#                              packaging/homebrew/makinax-mcp.rb
#
# Offline: the formula copy points at the local tarball through a throwaway
# local tap. Homebrew is required; this fails loudly rather than passing where
# brew is absent.
set -euo pipefail

TARBALL="${1:?usage: install-smoke.sh <tarball.tar.xz> <formula.rb>}"
FORMULA="${2:?usage: install-smoke.sh <tarball.tar.xz> <formula.rb>}"
command -v brew >/dev/null || {
  echo "install-smoke: Homebrew is required (this is a macOS packaging test)" >&2
  exit 2
}
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_ENV_HINTS=1

TARBALL="$(cd "$(dirname "$TARBALL")" && pwd)/$(basename "$TARBALL")"
# The install name follows the formula's class (MakinaxMcp -> makinax-mcp).
NAME="$(sed -nE 's/^class ([A-Za-z0-9]+) < Formula/\1/p' "$FORMULA" \
        | sed -E 's/([a-z0-9])([A-Z])/\1-\2/g' | tr '[:upper:]' '[:lower:]')"
[ -n "$NAME" ] || { echo "install-smoke: no Formula class found in $FORMULA" >&2; exit 2; }
TAP="makinax-smoke/local"
cleanup() {
  brew uninstall --force "$NAME" >/dev/null 2>&1 || true
  brew untap --force "$TAP" >/dev/null 2>&1 || true
}
trap cleanup EXIT

SHA="$(shasum -a 256 "$TARBALL" | awk '{print $1}')"
brew untap --force "$TAP" >/dev/null 2>&1 || true
brew tap-new --no-git "$TAP" >/dev/null
TAPDIR="$(brew --repository "$TAP")"
REWRITTEN="$TAPDIR/Formula/${NAME}.rb"
sed -E -e "s|^( *)url \".*\"|\1url \"file://${TARBALL}\"|" \
       -e "s|^( *)sha256 \".*\"|\1sha256 \"${SHA}\"|" \
       "$FORMULA" > "$REWRITTEN"
# A sed no-match is silent and would install the PUBLISHED artifact instead.
grep -q "url \"file://${TARBALL}\"" "$REWRITTEN" || {
  echo "install-smoke: the url rewrite did not match" >&2; exit 2; }
grep -q "sha256 \"${SHA}\"" "$REWRITTEN" || {
  echo "install-smoke: the sha256 rewrite did not match" >&2; exit 2; }

brew uninstall --force "$NAME" >/dev/null 2>&1 || true
brew install "$TAP/$NAME"
BIN="$(brew --prefix)/bin"
[ -x "$BIN/makinax-mcp" ] || { echo "install-smoke: no makinax-mcp in $BIN after install" >&2; exit 1; }
[ -f "$(brew --prefix)/share/${NAME}/skills/makinax/SKILL.md" ] || {
  echo "install-smoke: the skill was not installed" >&2; exit 1; }
"$BIN/makinax-mcp" --version
brew test "$TAP/$NAME"
echo "install-smoke OK: $NAME"
