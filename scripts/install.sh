#!/bin/sh
# kvm-mcp installer: no package manager, credentials, enrollment or network exposure.
set -eu
umask 077
fail() { printf '%s\n' "kvm-mcp: $*" >&2; exit 1; }
binary= url= checksum= prefix= service=auto service_root= start=no
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary|--url|--sha256|--prefix|--service|--service-root)
            [ "$#" -ge 2 ] || fail "missing value for $1"
            case "$1" in
                --binary) binary=$2;; --url) url=$2;; --sha256) checksum=$2;;
                --prefix) prefix=$2;; --service) service=$2;; --service-root) service_root=$2;;
            esac
            shift 2;;
        --start) start=yes; shift;;
        --help) printf '%s\n' 'install.sh (--binary FILE | --url HTTPS_URL) --sha256 HEX [--prefix DIR] [--service auto|comet|systemd|none] [--start]'; exit 0;;
        *) fail "unknown argument";;
    esac
done
[ "$(uname -s)" = Linux ] || fail 'Linux only'
case "$(uname -m)" in aarch64|arm64) machine=183;; x86_64|amd64) machine=62;; *) fail 'supported architectures: ARM64 and x86_64';; esac
[ "${#checksum}" = 64 ] || fail 'a trusted SHA-256 digest is required'
case "$checksum" in *[!a-fA-F0-9]*) fail 'invalid digest';; esac
[ -n "$binary" ] || [ -n "$url" ] || fail 'provide --binary or --url'
[ -z "$binary" ] || [ -z "$url" ] || fail 'choose binary OR URL'
case "$service" in auto|comet|systemd|none) ;; *) fail 'invalid service type';; esac
if [ "$service" = auto ]; then
    if [ -d /etc/kvmd/user ] && [ -f /etc/init.d/S99custom ]; then service=comet
    elif command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then service=systemd
    else fail 'unknown init system; explicitly use --service none for a manual install'; fi
fi
if [ -z "$prefix" ]; then
    if [ "$service" = comet ]; then prefix=/etc/kvmd/user/kvm-mcp; else prefix=/opt/kvm-mcp; fi
fi
case "$prefix" in /*) ;; *) fail 'prefix must be absolute';; esac
case "$prefix" in /|*..*|*[!a-zA-Z0-9/_.-]*) fail 'use a dedicated simple absolute prefix';; esac
case "$service_root" in ""|/*) ;; *) fail 'service-root must be absolute';; esac
case "$service_root" in *..*|*[!a-zA-Z0-9/_.-]*) fail 'invalid service-root';; esac
if [ "$service" != none ] && [ -z "$service_root" ]; then [ "$(id -u)" = 0 ] || fail 'service installation requires root'; fi
if [ "$service" = comet ] && [ -z "$service_root" ]; then
    command -v start-stop-daemon >/dev/null 2>&1 || fail 'Comet startup requires start-stop-daemon; use --service none for manual startup'
fi
if [ "$start" = yes ]; then
    [ -z "$service_root" ] || fail 'cannot start a staged service'
    [ "$service" != none ] || fail 'manual mode cannot start a service'
    [ -f "$prefix/config.toml" ] || fail '--start requires an existing configured config.toml'
fi
hook="$service_root/etc/kvmd/user/scripts/S99kvm-mcp"
unit="$service_root/etc/systemd/system/kvm-mcp.service"
if [ "$service" = comet ]; then
    [ ! -L "$hook" ] || fail 'service hook is a symlink'
    if [ -f "$hook" ]; then grep -q '^# kvm-mcp managed$' "$hook" || fail 'refusing to replace an unrelated hook'; fi
elif [ "$service" = systemd ]; then
    [ ! -L "$unit" ] || fail 'service unit is a symlink'
    if [ -f "$unit" ]; then grep -q '^# kvm-mcp managed$' "$unit" || fail 'refusing to replace an unrelated unit'; fi
fi
tmp=$(mktemp -d)
cleanup() { rm -f "$tmp/binary"; rmdir "$tmp"; }
trap cleanup EXIT HUP INT TERM
if [ -n "$url" ]; then
    case "$url" in https://*) ;; *) fail 'download URL must use HTTPS';; esac
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --output "$tmp/binary" "$url"
else
    [ -f "$binary" ] || fail 'binary not found'
    cp "$binary" "$tmp/binary"
fi
actual=$(sha256sum "$tmp/binary" | awk '{print $1}')
expected=$(printf '%s' "$checksum" | tr 'A-F' 'a-f')
[ "$actual" = "$expected" ] || fail 'checksum mismatch; nothing installed'
magic=$(od -An -tx1 -N6 "$tmp/binary" | tr -d ' \n')
[ "$magic" = 7f454c460201 ] || fail 'expected little-endian Linux ELF64 executable'
arch=$(od -An -tu2 -j18 -N2 "$tmp/binary" | tr -d ' \n')
[ "$arch" = "$machine" ] || fail 'binary architecture does not match host'
chmod 755 "$tmp/binary"
"$tmp/binary" --version >/dev/null || fail 'verified binary cannot run on this host'
[ ! -L "$prefix" ] || fail 'prefix must not be a symlink'
mkdir -p "$prefix"
[ "$(readlink -f "$prefix")" = "$prefix" ] || fail 'prefix must not traverse symlinks'
for file in kvm-mcp kvm-mcp.new kvm-mcp.previous config.toml config.toml.example; do
    [ ! -L "$prefix/$file" ] || fail 'refusing symlink inside installation'
done
chmod 700 "$prefix"
cp "$tmp/binary" "$prefix/kvm-mcp.new"
chmod 755 "$prefix/kvm-mcp.new"
if [ -f "$prefix/kvm-mcp" ]; then cp "$prefix/kvm-mcp" "$prefix/kvm-mcp.previous"; fi
mv -f "$prefix/kvm-mcp.new" "$prefix/kvm-mcp"
if [ ! -f "$prefix/config.toml.example" ]; then
    cat > "$prefix/config.toml.example" <<EOF
listen = "127.0.0.1:9100"
public_url = "https://kvm.example.test:8443"
auth_token_file = "$prefix/mcp.secret"
max_upload_bytes = 68719476736
transfer_timeout_seconds = 7200
[comet]
url = "http://127.0.0.1"
username = "admin"
password_file = "$prefix/comet.secret"
ports = 4
EOF
fi
if [ "$service" = comet ]; then
    mkdir -p "$(dirname "$hook")"
    [ ! -L "$hook" ] || fail 'service hook is a symlink'
    if [ -f "$hook" ]; then grep -q '^# kvm-mcp managed$' "$hook" || fail 'refusing to replace an unrelated hook'; fi
    cat > "$hook" <<EOF
#!/bin/sh
# kvm-mcp managed
set -eu
case "\${1:-}" in
    start)
        [ -f '$prefix/config.toml' ] || exit 0
        start-stop-daemon -S -b -m -p /run/kvm-mcp.pid -x '$prefix/kvm-mcp' -- --config '$prefix/config.toml'
        ;;
    stop)
        start-stop-daemon -K -p /run/kvm-mcp.pid -x '$prefix/kvm-mcp' || true
        # Wait for graceful shutdown; do not start a duplicate or force-kill a transfer.
        if [ -r /run/kvm-mcp.pid ]; then
            pid=\$(cat /run/kvm-mcp.pid)
            case "\$pid" in ''|*[!0-9]*) exit 1;; esac
            count=0
            while kill -0 "\$pid" 2>/dev/null; do
                count=\$((count + 1))
                [ "\$count" -lt 30 ] || { echo 'still shutting down; retry start later' >&2; exit 1; }
                sleep 1
            done
        fi
        ;;
    restart) "\$0" stop; "\$0" start;;
    *) echo 'usage: S99kvm-mcp start|stop|restart' >&2; exit 1;;
esac
EOF
    chmod 755 "$hook"
    if [ "$start" = yes ]; then "$hook" restart; fi
elif [ "$service" = systemd ]; then
    mkdir -p "$(dirname "$unit")"
    [ ! -L "$unit" ] || fail 'service unit is a symlink'
    if [ -f "$unit" ]; then grep -q '^# kvm-mcp managed$' "$unit" || fail 'refusing to replace an unrelated unit'; fi
    cat > "$unit" <<EOF
# kvm-mcp managed
[Unit]
Description=KVM MCP adapter
After=network-online.target
Wants=network-online.target
ConditionPathExists=$prefix/config.toml
[Service]
Type=simple
ExecStart=$prefix/kvm-mcp --config $prefix/config.toml
Restart=on-failure
RestartSec=3
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
[Install]
WantedBy=multi-user.target
EOF
    if [ -z "$service_root" ]; then
        systemctl daemon-reload
        systemctl enable kvm-mcp.service
        if [ "$start" = yes ]; then systemctl restart kvm-mcp.service; fi
    fi
fi
printf '%s\n' "Installed verified binary in $prefix; startup: $service. Existing configuration and credentials preserved."
printf '%s\n' 'Configure the example and mode-0600 secret files before starting. No Tailscale, firewall or enrollment settings were changed.'
