#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
platform=""
version=""
output="${root}/dist-release"
code_server_version="${CODE_SERVER_VERSION:-latest}"

usage() {
  printf '%s\n' \
    'Usage: scripts/package.sh --platform <linux-x86_64|macos-aarch64|windows-x86_64> --version <semver> [options]' \
    '  --output <dir>                  package directory (default: dist-release)' \
    '  --code-server-version <tag>     official code-server release (default: latest)'
}

die() {
  printf 'package: %s\n' "$1" >&2
  exit 2
}

while (($# > 0)); do
  case "$1" in
    --platform) (($# >= 2)) || die '--platform requires a value'; platform="$2"; shift 2 ;;
    --version) (($# >= 2)) || die '--version requires a value'; version="$2"; shift 2 ;;
    --output) (($# >= 2)) || die '--output requires a value'; output="$2"; shift 2 ;;
    --code-server-version) (($# >= 2)) || die '--code-server-version requires a value'; code_server_version="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done
[[ -n "$platform" ]] || die '--platform is required'
[[ -n "$version" ]] || die '--version is required'
command -v jq >/dev/null 2>&1 || die 'jq is required'
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] || die 'version must be a semantic version without the v prefix'

plugin_code="$(jq -er '.plugin_code | strings' "$root/ui/manifest.json")" || die 'UI manifest has no plugin code'
release_code="$(jq -er '.plugin_code | strings' "$root/release/manifest.json")" || die 'release manifest has no plugin code'
[[ "$plugin_code" =~ ^[a-z][a-z0-9_]{1,63}$ ]] || die 'manifest plugin code is invalid'
[[ "$plugin_code" == "$release_code" ]] || die 'release manifest plugin code does not match the UI manifest'
[[ "$(jq -er '.schema | numbers' "$root/release/manifest.json")" == 1 ]] || die 'release manifest schema is unsupported'
case "$platform" in
  linux-x86_64) os=linux; arch=x86_64; extension=so; target=x86_64-unknown-linux-gnu ;;
  macos-aarch64) os=macos; arch=aarch64; extension=dylib; target=aarch64-apple-darwin ;;
  windows-x86_64) os=windows; arch=x86_64; extension=dll; target=x86_64-pc-windows-msvc ;;
  *) die "unsupported platform: $platform" ;;
esac

build_output="$("$root/scripts/build.sh" --platform "$platform" --output "$output" --code-server-version "$code_server_version")"
artifact="$(printf '%s\n' "$build_output" | sed -n 's/^artifact=//p')"
sha="$(printf '%s\n' "$build_output" | sed -n 's/^sha256=//p')"
size="$(printf '%s\n' "$build_output" | sed -n 's/^size=//p')"
resolved_code_server="$(printf '%s\n' "$build_output" | sed -n 's/^code_server_version=//p')"
metadata="${output}/pom-plugin-code-server-${platform}.metadata.json"
public_manifest="${output}/pom-plugin-${platform}.json"
feature_set="$(jq -c '.feature_set' "$root/release/manifest.json")" || die 'release feature set is invalid'
cat > "$metadata" <<JSON
{
  "plugin_code": "${plugin_code}",
  "version": "${version}",
  "platform": "${platform}",
  "target": "${target}",
  "plugin_abi": 1,
  "sha256": "${sha}",
  "size": ${size},
  "artifact": "$(basename "$artifact")",
  "code_server_version": "${resolved_code_server}"
}
JSON
jq -n \
  --slurpfile contract "$root/release/manifest.json" \
  --arg release_id "v${version}" --arg version "$version" \
  --arg os "$os" --arg arch "$arch" --arg asset "$(basename "$artifact")" \
  --arg sha256 "$sha" --argjson size "$size" \
  '($contract[0]) + {release_id:$release_id, version:$version, os:$os, arch:$arch, asset:$asset, sha256:$sha256, size:$size}' \
  > "$public_manifest" || die 'could not generate the GitHub release manifest'
printf 'artifact=%s\nmetadata=%s\npublic_manifest=%s\nsha256=%s\nsize=%s\nversion=%s\nplatform=%s\ncode_server_version=%s\n' \
  "$artifact" "$metadata" "$public_manifest" "$sha" "$size" "$version" "$platform" "$resolved_code_server"
