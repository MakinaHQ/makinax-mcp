#!/usr/bin/env bash
# Rewrite both Homebrew formulas from a PUBLISHED release.
#
# The formulas' version and sha256 are never hand-maintained: they come from
# the release's SHA256SUMS, or the formula is not written. release.yml runs
# this on stable tags and commits the result to the tap; by hand:
#
#   ./packaging/homebrew/sync-tap.sh v0.1.0
set -euo pipefail

TAG="${1:-}"
# Where the checksums are read from. release.yml points this at the public
# distribution repo, so the sha256 describes the bytes at the formula's url.
REPO="${MAKINAX_RELEASE_REPO:-MakinaHQ/homebrew-makinax-mcp}"
# Where users download from (public). Every formula url names this host.
DIST_REPO="${MAKINAX_DIST_REPO:-MakinaHQ/homebrew-makinax-mcp}"
if [[ -z "$TAG" ]]; then
  echo "usage: $0 <tag>   e.g. $0 v0.1.0" >&2
  exit 2
fi
command -v gh >/dev/null || { echo "error: gh is required" >&2; exit 2; }

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# `${REPO}` braced: a bare name followed by a non-ASCII byte can swallow that
# byte into the identifier, which `set -u` reports as unbound.
echo "fetching SHA256SUMS for ${TAG} from ${REPO}…"
gh release download "$TAG" --repo "$REPO" --pattern SHA256SUMS --dir "$tmp"

version="${TAG#v}"
for variant in makinax-mcp makinax-mcp-readonly; do
  asset="${variant}-aarch64-apple-darwin.tar.xz"
  # Exact asset name: the Linux lines in the same file are ignored.
  sha="$(awk -v a="$asset" '$2 == a || $2 == "*"a {print $1}' "$tmp/SHA256SUMS" | head -1)"
  if [[ -z "$sha" ]]; then
    echo "error: no checksum for $asset in $TAG's SHA256SUMS; refusing to write a formula without one" >&2
    exit 1
  fi
  f="$here/${variant}.rb"
  # GNU sed on CI, BSD sed on macOS: write through a temp file.
  tmpf="$(mktemp)"
  # Indentation-agnostic anchors. A no-match is silent here; release.yml's
  # placeholder check is what catches it.
  sed -E -e "s|^( *)version \".*\"|\1version \"${version}\"|" \
         -e "s|^( *)sha256 \".*\"|\1sha256 \"${sha}\"|" \
         -e "s|(^ *url \"https://github\.com/)[^/]+/[^/]+(/releases/download/)|\1${DIST_REPO}\2|" \
         "$f" > "$tmpf"
  mv "$tmpf" "$f"
  echo "  ${variant}.rb -> version ${version}, sha256 ${sha:0:12}…"
done

cat <<EOF2

Both formulas updated. To publish by hand:
  gh repo clone ${DIST_REPO} /tmp/tap
  cp $here/makinax-mcp.rb $here/makinax-mcp-readonly.rb /tmp/tap/Formula/
  cp $here/TAP-README.md /tmp/tap/README.md
  cd /tmp/tap && git add Formula README.md && git commit -m "makinax-mcp ${version}" && git push

Then check on a machine with no Rust toolchain:
  brew install makinahq/makinax-mcp/makinax-mcp-readonly && makinax-mcp --version
EOF2
