#!/usr/bin/env bash
# Pins scripts/gh-release-create.sh against a stubbed `gh`, and pins that
# release.yml creates both releases through it with nothing masking a failure.
#
# Tolerated: a create that succeeds, and a create that fails with "already
# exists" when `gh release view <tag> --repo <same repo>` then finds the
# release (a re-run). Everything else fails the step and surfaces gh's text.
set -euo pipefail
cd "$(dirname "$0")/.."

script="$PWD/scripts/gh-release-create.sh"
tag="v9.9.9"
repo="example-org/example-dist"
fail=0

# run <create-failure-or-empty> <exists:0|1>: prints the log, returns the status.
run() {
  local failure="$1" exists="$2" dir out status
  dir=$(mktemp -d)
  {
    echo '#!/bin/sh'
    echo 'case "$2" in'
    if [ -z "$failure" ]; then
      echo '  create) echo created; exit 0 ;;'
    else
      printf "  create) printf '%%s\\\\n' '%s' >&2; exit 1 ;;\n" "$failure"
    fi
    if [ "$exists" = 1 ]; then
      echo "  view) case \"\$*\" in *'release view $tag --repo $repo'*) exit 0 ;; esac; exit 1 ;;"
    else
      echo "  view) echo 'release not found' >&2; exit 1 ;;"
    fi
    echo 'esac'
    echo 'exit 9'
  } > "$dir/gh"
  chmod +x "$dir/gh"
  status=0
  out=$(PATH="$dir:$PATH" bash "$script" "$tag" --repo "$repo" --title "$tag" 2>&1) || status=$?
  rm -rf "$dir"
  printf '%s\n' "$out"
  return "$status"
}

expect_ok() {
  local name="$1" failure="$2" exists="$3" log
  if log=$(run "$failure" "$exists"); then echo "ok: $name"; else echo "FAIL: $name was refused:"; echo "$log"; fail=1; fi
}

expect_fail() {
  local name="$1" failure="$2" exists="$3" log
  if log=$(run "$failure" "$exists"); then
    echo "FAIL: $name was tolerated:"; echo "$log"; fail=1
  elif ! grep -qF -- "$failure" <<<"$log"; then
    echo "FAIL: $name: gh's text is not surfaced:"; echo "$log"; fail=1
  else
    echo "ok: $name"
  fi
}

exists_msg="HTTP 422: Validation Failed (https://api.github.com/repos/$repo/releases) Release.tag_name already exists"

expect_ok "a successful create" "" 0
expect_ok "an existing release (a re-run)" "$exists_msg" 1
expect_fail "an unrelated 'already exists' with no release" \
  "HTTP 422: Validation Failed: a ruleset named release already exists" 0
expect_fail "a tag-exists error with no release behind it" "$exists_msg" 0
for failure in \
  "HTTP 422: Validation Failed (https://api.github.com/repos/$repo/releases) Repository is empty." \
  "HTTP 403: Resource not accessible by personal access token" \
  "HTTP 404: Not Found" \
  "error connecting to api.github.com"; do
  expect_fail "$failure" "$failure" 1
done

# Any repository spelling but `--repo <owner/name>` is refused before gh runs:
# the confirming view would otherwise check the checked-out repository.
for form in "-R $repo" "-R$repo" "--repo=$repo"; do
  # shellcheck disable=SC2086 # the form is split on purpose
  if log=$(PATH="/nonexistent:$PATH" bash "$script" "$tag" $form --title "$tag" 2>&1); then
    echo "FAIL: repository form '$form' was accepted:"; echo "$log"; fail=1
  elif ! grep -qF -- "--repo <owner/name>" <<<"$log"; then
    echo "FAIL: repository form '$form' was not refused by the script:"; echo "$log"; fail=1
  else
    echo "ok: repository form '$form' is refused"
  fi
done

# Both release creations go through the script, and nothing masks a failure.
wf=.github/workflows/release.yml
code=$(grep -v '^[[:space:]]*#' "$wf")
n=$(grep -cF './scripts/gh-release-create.sh "$GITHUB_REF_NAME"' <<<"$code" || true)
if [ "$n" != 2 ]; then
  echo "FAIL: release.yml creates $n releases through gh-release-create.sh, expected 2"; fail=1
else
  echo "ok: both releases are created through gh-release-create.sh"
fi
if grep -qF 'gh release create' <<<"$code"; then
  echo "FAIL: release.yml calls \`gh release create\` directly"; fail=1
else
  echo "ok: no direct gh release create"
fi
if grep -qF '|| echo' <<<"$code"; then
  echo "FAIL: release.yml masks a failure with \`|| echo\`"; fail=1
else
  echo "ok: no || echo"
fi

[ "$fail" = 0 ] && echo "GH-RELEASE-CREATE OK"
exit "$fail"
