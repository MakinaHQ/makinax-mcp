#!/usr/bin/env bash
# `gh release create <tag> [args...]` for release.yml, tolerating exactly one
# failure: a release for this tag already exists, which is what a re-run of a
# failed release meets. Every other failure (an empty repository, a missing or
# read-only token, the network) fails the step. Swallowing those with
# `|| echo` let the next upload die with "release not found" after the
# private release was already out.
#
# "Already exists" is not taken on the error's word alone: the text must say
# so AND `gh release view <tag>` must then find the release. An unrelated
# error that happens to contain the phrase still fails.
set -euo pipefail

tag="${1:?usage: $0 <tag> [gh release create args...]}"
repo=()
prev=""
for a in "$@"; do
  case "${a}" in
    # The confirming `gh release view` must look at the same repository as the
    # create. Only the `--repo <owner/name>` form is parsed; any other spelling
    # is refused rather than silently confirmed against the checked-out repo.
    -R | -R?* | --repo=*)
      echo "::error::pass the repository to gh-release-create.sh as --repo <owner/name>" >&2
      exit 2
      ;;
  esac
  if [ "${prev}" = "--repo" ]; then repo=(--repo "${a}"); fi
  prev="${a}"
done

if out=$(gh release create "$@" 2>&1); then
  printf '%s\n' "${out}"
  exit 0
fi
case "${out}" in
  *"already exists"*)
    if gh release view "${tag}" ${repo[@]+"${repo[@]}"} >/dev/null 2>&1; then
      echo "release ${tag} already exists; uploading assets to it"
      exit 0
    fi
    ;;
esac
echo "::error::could not create the release ${tag}: ${out}" >&2
exit 1
