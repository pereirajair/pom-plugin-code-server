#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
plan="$(env RELEASE_LINUX=true VERSION_LINUX=v0.1.0 "$root/scripts/ci-plan.sh")"
jq -e '.include | length == 1 and .[0].platform == "linux-x86_64" and .[0].target == "x86_64-unknown-linux-gnu"' <<<"$plan" >/dev/null

if env RELEASE_WINDOWS=true VERSION_WINDOWS=bad "$root/scripts/ci-plan.sh" >/dev/null 2>&1; then
  echo 'ci-plan accepted an invalid selected version' >&2
  exit 1
fi
if "$root/scripts/ci-plan.sh" >/dev/null 2>&1; then
  echo 'ci-plan accepted an empty selection' >&2
  exit 1
fi
printf 'release matrix plan tests passed\n'
