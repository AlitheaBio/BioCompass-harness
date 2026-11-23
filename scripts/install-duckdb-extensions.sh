#!/bin/sh
# Bootstrap and SHA-256 verify offline-bundled DuckDB v2.0.0 extensions.
# Usage: ./scripts/install-duckdb-extensions.sh [--verify-only] [platform|all] [destination]
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"
VERSION="v2.0.0"
MANIFEST="$REPO_ROOT/vendor/duckdb-extensions/$VERSION.sha256"

verify_only=0
if [ "${1:-}" = "--verify-only" ]; then
    verify_only=1
    shift
fi

detect_host_platform() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os:$arch" in
        Darwin:arm64|Darwin:aarch64) echo "osx_arm64" ;;
        Linux:x86_64|Linux:amd64)    echo "linux_amd64" ;;
        Linux:aarch64|Linux:arm64)   echo "linux_arm64" ;;
        *)
            echo "Unsupported host platform for DuckDB extensions: $os/$arch" >&2
            exit 2
            ;;
    esac
}

resolve_platforms() {
    case "${1:-}" in
        "")          detect_host_platform ;;
        osx_arm64|linux_amd64|linux_arm64) echo "$1" ;;
        amd64)       echo "linux_amd64" ;;
        arm64)       echo "linux_arm64" ;;
        all)         printf '%s\n' linux_amd64 linux_arm64 osx_arm64 ;;
        *)
            echo "Unsupported DuckDB extension platform: $1" >&2
            exit 2
            ;;
    esac
}

check_sha256() {
    checksum_file="$1"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$checksum_file" >/dev/null
    else
        shasum -a 256 -c "$checksum_file" >/dev/null
    fi
}

platforms="$(resolve_platforms "${1:-}")"
destination="${2:-$REPO_ROOT/vendor/duckdb-extensions}"

if [ ! -f "$MANIFEST" ]; then
    echo "Missing DuckDB extension manifest: $MANIFEST" >&2
    exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

for platform in $platforms; do
    selected="$stage/$platform.sha256"
    awk -v prefix="$platform/" 'index($2, prefix) == 1' "$MANIFEST" > "$selected"
    count="$(wc -l < "$selected" | tr -d ' ')"
    if [ "$count" -ne 4 ]; then
        echo "Expected 4 extensions for $platform in $MANIFEST, found $count" >&2
        exit 1
    fi

    if [ -d "$destination/$VERSION/$platform" ] && (cd "$destination/$VERSION" && check_sha256 "$selected") 2>/dev/null; then
        echo "Verified DuckDB extensions ($VERSION/$platform) in $destination/$VERSION/$platform"
        continue
    fi

    if [ "$verify_only" -eq 1 ]; then
        echo "DuckDB extension verification failed or missing for $VERSION/$platform in $destination/$VERSION/$platform" >&2
        exit 1
    fi

    mkdir -p "$stage/$platform"
    while read -r _checksum file; do
        echo "Downloading https://extensions.duckdb.org/$VERSION/$file.gz ..."
        curl --proto '=https' --proto-redir '=https' --fail --silent --show-error --location \
            --retry 3 --connect-timeout 15 --max-time 180 \
            "https://extensions.duckdb.org/$VERSION/$file.gz" -o "$stage/extension.gz"
        gzip -dc "$stage/extension.gz" > "$stage/$file"
    done < "$selected"

    (cd "$stage" && check_sha256 "$selected")
    chmod 0755 "$stage/$platform"
    chmod 0644 "$stage/$platform"/*.duckdb_extension
    install -d -m 0755 "$destination" "$destination/$VERSION"
    rm -rf "$destination/$VERSION/$platform"
    mv "$stage/$platform" "$destination/$VERSION/$platform"
    echo "Installed and verified DuckDB extensions ($VERSION/$platform) in $destination/$VERSION/$platform"
done
