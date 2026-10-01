#!/usr/bin/env bash
# Fetches the official, standalone code-server release for the selected target.
# The archive is checksum-verified here and embedded into the POM plugin during
# the native build, so installed nodes do not download code-server at startup.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
platform=""
version="${CODE_SERVER_VERSION:-latest}"
output="${root}/build"

usage() {
  printf '%s\n' \
    'Usage: scripts/fetch-runtime.sh --platform <linux-x86_64|macos-aarch64|windows-x86_64> [options]' \
    '  --code-server-version <tag|version>  official code-server release (default: latest)' \
    '  --output <dir>                       download directory (default: build)'
}

die() {
  printf 'fetch-runtime: %s\n' "$1" >&2
  exit 2
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi
}

while (($# > 0)); do
  case "$1" in
    --platform) (($# >= 2)) || die '--platform requires a value'; platform="$2"; shift 2 ;;
    --code-server-version) (($# >= 2)) || die '--code-server-version requires a value'; version="$2"; shift 2 ;;
    --output) (($# >= 2)) || die '--output requires a value'; output="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ -n "$platform" ]] || die '--platform is required'
case "$platform" in
  linux-x86_64) release_platform="linux-amd64" ;;
  macos-aarch64) release_platform="macos-arm64" ;;
  windows-x86_64) release_platform="windows-amd64" ;;
  *) die "unsupported platform: $platform" ;;
esac
for tool in curl jq tar; do command -v "$tool" >/dev/null 2>&1 || die "$tool is required"; done

headers=(-H 'Accept: application/vnd.github+json')
if [[ -n "${GITHUB_TOKEN:-}" ]]; then headers+=(-H "Authorization: Bearer ${GITHUB_TOKEN}"); fi
api='https://api.github.com/repos/coder/code-server/releases'
if [[ "$version" == latest ]]; then
  release_url="${api}/latest"
else
  tag="$version"
  [[ "$tag" == v* ]] || tag="v${tag}"
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || die "invalid code-server release tag: $version"
  release_url="${api}/tags/${tag}"
fi
release="$(curl -fsSL "${headers[@]}" "$release_url")" || die "could not resolve code-server release $version"
resolved_tag="$(jq -er '.tag_name | strings' <<<"$release")" || die 'GitHub returned no release tag'
resolved_version="${resolved_tag#v}"
[[ "$resolved_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]] || die "invalid resolved code-server version: $resolved_tag"
asset="code-server-${resolved_version}-${release_platform}.tar.gz"
asset_url="$(jq -er --arg asset "$asset" '.assets[] | select(.name == $asset) | .browser_download_url' <<<"$release")" \
  || die "release $resolved_tag has no $asset asset"
digest="$(jq -er --arg asset "$asset" '.assets[] | select(.name == $asset) | .digest // empty' <<<"$release")" \
  || die "release $resolved_tag does not publish a SHA-256 digest for $asset"
[[ "$digest" =~ ^sha256:[0-9a-fA-F]{64}$ ]] || die "invalid GitHub SHA-256 digest for $asset"
expected="${digest#sha256:}"

mkdir -p "$output/downloads"
archive="${output}/downloads/${asset}"
if [[ ! -f "$archive" ]] || [[ "$(sha256 "$archive")" != "$expected" ]]; then
  rm -f "$archive"
  curl -fL --retry 3 --retry-delay 2 -o "$archive" "$asset_url"
fi
actual="$(sha256 "$archive")"
[[ "$actual" == "$expected" ]] || die "SHA-256 mismatch for $asset"

# Verify the upstream tarball has the root directory expected by the runtime.
server_root="${asset%.tar.gz}"
tar -tzf "$archive" | awk -v root="$server_root/" 'index($0, root) == 1 { found=1 } END { exit !found }' \
  || die "release archive does not contain $server_root"
printf '%s\n' "$actual" > "${archive}.sha256"
printf 'archive=%s\nsha256=%s\nversion=%s\nserver_root=%s\nplatform=%s\nasset=%s\n' \
  "$archive" "$actual" "$resolved_version" "$server_root" "$platform" "$asset"
