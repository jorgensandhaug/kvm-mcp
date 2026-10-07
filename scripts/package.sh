#!/bin/sh
set -eu
# Run after building each requested musl target. Does not publish releases.
version=$(cargo metadata --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "kvm-mcp") | .version')
[ -n "$version" ] || { echo 'cannot identify package version' >&2; exit 1; }
mkdir -p dist
for target in "$@"; do
    case "$target" in aarch64-unknown-linux-musl|x86_64-unknown-linux-musl) ;; *) echo 'unsupported target' >&2; exit 1;; esac
    binary="target/$target/release/kvm-mcp"
    [ -f "$binary" ] || { echo "missing build: $target" >&2; exit 1; }
    cp "$binary" "dist/kvm-mcp-$version-$target"
    sha256sum "dist/kvm-mcp-$version-$target"
done
# Keep the retained manifest in sync when an existing release is rebuilt.
sha256sum dist/kvm-mcp-"$version"-*-unknown-linux-musl > dist/SHA256SUMS
