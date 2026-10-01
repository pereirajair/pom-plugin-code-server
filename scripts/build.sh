#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
platform=""
output="${root}/dist-release"
profile="release"
code_server_version="${CODE_SERVER_VERSION:-latest}"

usage() {
  printf '%s\n' \
    'Usage: scripts/build.sh --platform <linux-x86_64|macos-aarch64|windows-x86_64> [options]' \
    '  --output <dir>                  artifact directory (default: dist-release)' \
    '  --code-server-version <tag>     official code-server release (default: latest)' \
    '  --debug                         build the debug profile'
}

die() {
  printf 'build: %s\n' "$1" >&2
  exit 2
}

while (($# > 0)); do
  case "$1" in
    --platform) (($# >= 2)) || die '--platform requires a value'; platform="$2"; shift 2 ;;
    --output) (($# >= 2)) || die '--output requires a value'; output="$2"; shift 2 ;;
    --code-server-version) (($# >= 2)) || die '--code-server-version requires a value'; code_server_version="$2"; shift 2 ;;
    --debug) profile="debug"; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ -n "$platform" ]] || die '--platform is required'
case "$platform" in
  linux-x86_64) target="x86_64-unknown-linux-gnu"; extension="so" ;;
  macos-aarch64) target="aarch64-apple-darwin"; extension="dylib" ;;
  windows-x86_64) target="x86_64-pc-windows-msvc"; extension="dll" ;;
  *) die "unsupported platform: $platform" ;;
esac
if [[ "$profile" == release ]]; then
  cargo_args=(build --locked --release --target "$target")
  artifact_dir="${root}/target/${target}/release"
else
  cargo_args=(build --locked --target "$target")
  artifact_dir="${root}/target/${target}/debug"
fi
if [[ "$extension" == dll ]]; then artifact="${artifact_dir}/pom_plugin_code_server.dll"; else artifact="${artifact_dir}/libpom_plugin_code_server.${extension}"; fi

"$root/scripts/build-ui.sh"
runtime_output="$("$root/scripts/fetch-runtime.sh" --platform "$platform" --code-server-version "$code_server_version")"
archive="$(printf '%s\n' "$runtime_output" | sed -n 's/^archive=//p')"
checksum="$(printf '%s\n' "$runtime_output" | sed -n 's/^sha256=//p')"
resolved_version="$(printf '%s\n' "$runtime_output" | sed -n 's/^version=//p')"
server_root="$(printf '%s\n' "$runtime_output" | sed -n 's/^server_root=//p')"
[[ -f "$archive" ]] || die 'official code-server archive is missing'
(
  cd "$root"
  CODE_SERVER_ARCHIVE="$archive" \
    CODE_SERVER_SHA256="$checksum" \
    CODE_SERVER_VERSION="$resolved_version" \
    CODE_SERVER_ROOT="$server_root" \
    cargo "${cargo_args[@]}"
)
[[ -f "$artifact" ]] || die "artifact not found: $artifact"

mkdir -p "$output"
destination="${output}/pom-plugin-code-server-${platform}.${extension}"
cp "$artifact" "$destination"
if command -v shasum >/dev/null 2>&1; then sha="$(shasum -a 256 "$destination" | awk '{print $1}')"; else sha="$(sha256sum "$destination" | awk '{print $1}')"; fi
size="$(wc -c < "$destination" | tr -d '[:space:]')"
printf 'artifact=%s\nsha256=%s\nsize=%s\nplatform=%s\ntarget=%s\ncode_server_version=%s\n' \
  "$destination" "$sha" "$size" "$platform" "$target" "$resolved_version"
