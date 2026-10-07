#!/bin/sh
set -eu
binary=${1:-target/debug/kvm-mcp}
[ -x "$binary" ] || { echo 'build the executable first' >&2; exit 1; }
digest=$(sha256sum "$binary" | awk '{print $1}')
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
prefix="$work/install"
sh scripts/install.sh --binary "$binary" --sha256 "$digest" --prefix "$prefix" --service none
printf '%s\n' 'retained configuration marker' > "$prefix/config.toml"
cp "$prefix/config.toml" "$work/expected"
sh scripts/install.sh --binary "$binary" --sha256 "$digest" --prefix "$prefix" --service none
cmp "$prefix/config.toml" "$work/expected"
[ -f "$prefix/kvm-mcp.previous" ]
if sh scripts/install.sh --binary "$binary" --sha256 0000000000000000000000000000000000000000000000000000000000000000 --prefix "$work/bad" --service none; then
    echo 'bad checksum unexpectedly accepted' >&2; exit 1
fi
[ ! -e "$work/bad" ]
sh scripts/install.sh --binary "$binary" --sha256 "$digest" --prefix "$prefix" --service comet --service-root "$work/root"
sh -n "$work/root/etc/kvmd/user/scripts/S99kvm-mcp"
grep -q 'start-stop-daemon' "$work/root/etc/kvmd/user/scripts/S99kvm-mcp"
sh scripts/install.sh --binary "$binary" --sha256 "$digest" --prefix "$prefix" --service systemd --service-root "$work/root"
grep -q '^ExecStart=' "$work/root/etc/systemd/system/kvm-mcp.service"
cmp "$prefix/config.toml" "$work/expected"
echo 'Installer checks passed; all writes confined to the temporary test root.'
