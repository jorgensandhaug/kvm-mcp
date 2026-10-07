# KVM MCP guide

A Linux-only Rust MCP server for out-of-band KVM control. Runs on the appliance or another Linux host. The first backend targets **GL.iNet Comet X / GL-RM4PE** through its existing KVMD HTTP/WebSocket APIs. New hardware implements the `KvmBackend` trait; the MCP tools do not depend on vendor route names.

**Status:** initial implementation with automated protocol/mock tests and partial physical qualification on Comet X firmware 1.8.1 release2. Private HTTPS, screenshots, keyboard/pointer input and a complete bootable installer transfer have been verified; see [validation](../VALIDATION.md) for remaining checks. No firmware replacement, cloud account, runtime Node/Python package, database or external object-storage service is needed. Commands below run from the repository root.

## Design

```text
MCP client -- private HTTPS --> kvm-mcp -- local HTTP/WebSocket --> KVMD --> HDMI/USB/ATX
HTTP transfer client --------> /transfer/<one-time capability> ----> virtual-media storage
```

Uses the official `rmcp` SDK and Streamable HTTP at `/mcp`. MCP requests require a bearer credential separate from the Comet administrator password. Credentials stay server-side in mode-0600 files. HTTPS backends are certificate-verified; provide a CA file for a private certificate. Cleartext backend HTTP is allowed only on loopback.

The server binds only to loopback. Put it behind a private HTTPS proxy, such as Tailscale Serve. `public_url` must be reachable by both the MCP client and the process moving files. A successful request from a laptop does not prove a remote agent has the same route. Do not use Tailscale Funnel for a private-only deployment.

## Tools

| Tool | Purpose |
| --- | --- |
| `kvm_status` | Targets, active port, discovered ATX/media availability, keymaps |
| `kvm_select` | Select a target, confirming observed selection |
| `kvm_capture` | Native MCP JPEG image plus target, dimensions and timestamp; 1280px default width |
| `kvm_input` | Chord, text, absolute pointer, click, scroll, release-all |
| `kvm_power` | ATX on/off/hard-off/reset/button actions |
| `media_list` | Stored images, connection state and storage information |
| `media_mount` / `media_eject` | Read-only virtual media, explicit target |
| `media_delete` | Explicitly delete an image; not the same as eject |
| `media_transfer` | Mint a scoped single-use HTTP upload/download URL |
| `media_transfer_revoke` | Revoke an unconsumed ticket |

Comet target IDs are **1..4**; firmware channel numbers are translated internally. A simple operation mutex serializes hardware actions and active media transfers. A long transfer can therefore delay console operations; there is no elaborate reservation system. Keep the selected target stable while an installer needs virtual media. Switching is refused while media is connected, rather than silently disconnecting it. Eject names the current target and never switches another machine to satisfy the request.

That mutex coordinates this server instance, not a person using the vendor UI or another control client. Do not change channels from another client during a hardware operation.

Text is **emulated keyboard input, not clipboard access**; use the correct keymap. Pointer coordinates are normalized `[0,1]` over the full returned image. Chords use DOM names such as `ControlLeft`, `KeyA`, `Delete`, and automatically release; arbitrary indefinitely held keys are intentionally not exposed. `release_all` resets HID and can briefly reconnect it. There is no structured BIOS API: agents inspect the image and send input.

An input response acknowledges the firmware request, not completion of the guest's reaction. In particular, long text may still be appearing in an immediate screenshot. Check the resulting console before dependent actions or retries.

ATX controls require the compatible board on the selected channel. The backend deliberately does not use RM4PE's stub `/switch/atx/*` methods. A firmware response is not physical proof that the desired computer changed power state. Mutations are not automatically replayed after ambiguous network failures.

Backend HTTP redirects and automatic retries are disabled, and backend credentials are not routed through environment-configured HTTP proxies. Read-only snapshot warmup errors are retried explicitly; hardware mutations are not.

## Media: MCP is the control plane, HTTP is the byte path

Ask `media_transfer` for:

```json
{"direction":"upload","image":"installer.iso","size_bytes":1073741824,"expires_in_seconds":900}
```

It returns `url`, `method`, `content_length`, `expires_at_unix`, `single_use`, and a non-secret `ticket_id` for revocation. Send the **raw file**, not multipart form data:

```sh
# TRANSFER_URL is the returned secret capability; avoid shell tracing/history/logs.
curl --fail --upload-file installer.iso "$TRANSFER_URL"
```

For download, request `{"direction":"download","image":"installer.iso"}` and use:

```sh
curl --fail --output installer.iso "$TRANSFER_URL"
```

The HTTP client needs the URL, not the MCP administrator token or Comet password. These URLs are bearer credentials; possession grants access to precisely that image/direction. They contain 256 bits of OS randomness, are stored only as hashes in memory, expire in 10..3600 seconds, and are atomically consumed when a valid transfer begins. A restart invalidates unused tickets. Default upload maximum is 64 GiB, configurable to the appliance/storage; remaining disk space is enforced by KVMD. The ticket registry holds at most 128 pending grants and prunes expired ones on use.

The application does not log request URLs. Redact `/transfer/` paths in any external proxy/access logging too.

Uploads require an exact `Content-Length`; oversized, undersized, encoded and ranged bodies are rejected. Bytes stream with backpressure, without staging a second image or reading the whole file into memory. Uploads do not implicitly overwrite existing images; delete explicitly or choose another name. KVMD is asked to remove incomplete writes. Inspect `media_list` after failure; do not assume every firmware build cleans up identically.

**No fake resume promise:** the firmware interface used here does not provide verified resumable uploads or ranged downloads. HEAD/Range requests are rejected without consuming the grant. Once a transfer starts, even a failed attempt consumes it; obtain a fresh ticket after inspecting state. Expiry limits when the transfer may begin, not its duration. Transfer duration is bounded separately (default two hours); revocation does not cancel an already-started stream. The upload response verifies the firmware's written-byte count. File hashes should be checked by the transfer client/source; the initial server does not claim a stored-image cryptographic verification result.

An agent may choose a local file, a download tool, or a streaming pipeline that supplies the known length. The server intentionally does not fetch arbitrary user-supplied Internet URLs: this avoids an SSRF-capable fetch proxy and leaves source choice, networking and credentials with the transfer client.

## Build and run

Rust 1.88 or newer; Cargo.lock is included. Build on a development machine, not the Comet:

```sh
cargo test --locked
cargo build --release --locked
./target/release/kvm-mcp --version
```

For portable releases, install the appropriate Rust musl target and a compatible linker (for example via `cargo-zigbuild`), then:

```sh
cargo zigbuild --release --locked --target aarch64-unknown-linux-musl
cargo zigbuild --release --locked --target x86_64-unknown-linux-musl
sh scripts/package.sh aarch64-unknown-linux-musl x86_64-unknown-linux-musl
cargo metadata --locked --format-version 1 | node scripts/licenses.mjs
```

Packaging uses `jq` and the optional build-time Node helper to retain dependency license files; neither is required to run the server. Distribute those notices with binaries, along with the Rust toolchain/standard-library and C-runtime license materials for the toolchain used to build them.

Copy `examples/config.toml` outside the checkout. Create a strong random MCP token (at least 32 characters) and a file containing the Comet password, each mode 0600. Set the firmware API origin, private HTTPS origin and absolute secret paths. No default working credentials are shipped. The server reads the password when authenticating; Comet two-factor login is not automatically bypassed.

```sh
kvm-mcp --config /path/config.toml
# Example private HTTPS proxy; check existing Serve mappings and CLI first:
tailscale serve --bg --https=8443 http://127.0.0.1:9100
```

Use `https://<your-private-hostname>:8443/mcp` and a configured `Authorization: Bearer ...` header in the MCP client. Origins from unrelated browser sites are rejected; CORS is not opened. `/health` is a liveness-only loopback/proxied endpoint, not a test of the KVM login or hardware.

On firmware that redirects HTTP to a self-signed HTTPS listener without a valid
hostname certificate, do not disable backend certificate verification. For an
on-appliance installation, `examples/comet-backend-proxy.conf` provides a separate
loopback-only nginx listener to the existing KVMD and streamer Unix sockets.
Inspect its paths against the installed firmware, place it in the dedicated
installation directory, and install the accompanying `S98kvm-mcp-proxy` hook
before starting the MCP hook. Configure the backend as `http://127.0.0.1:9101`.
This leaves the vendor's nginx configuration untouched. The socket permissions
on the tested firmware require root; the listener must remain loopback-only.

## Persistent installation

Inspect the installer, then provide a binary and its digest from a trusted build/release:

```sh
sh scripts/install.sh --binary ./kvm-mcp-<version>-aarch64-unknown-linux-musl \
  --sha256 <trusted-sha256> --service comet
```

Alternatively `--url https://<release-host>/<asset>` downloads over verified HTTPS; a trusted `--sha256` is still required. This supports a future curl-delivered installer without baking in a fictitious repository or release URL. Do not treat a checksum obtained from an untrusted source as a signature.

The installer checks Linux/ELF64/architecture/digest, verifies the executable can run, atomically replaces the binary, retains the previous binary, and preserves configuration/secrets. It **does not start by default**; configure `config.toml` before explicitly using `--start` or starting the service. Reinstall is idempotent. On an already running service, a binary update takes effect only after an explicit restart.

- Comet: `/etc/kvmd/user/kvm-mcp/`, launched by `/etc/kvmd/user/scripts/S99kvm-mcp` through stock `S99custom`. No vendor startup file is replaced. Boot startup is supported; the initial BusyBox launcher is not a crash-respawn supervisor.
- Ordinary Linux: `/opt/kvm-mcp/`, with a systemd unit and restart-on-failure.
- Other init: `--service none --prefix /your/dedicated/path`, then run manually.

Firmware update preservation depends on the vendor's keep-settings process. Test reboot, reinstall and an authorized keep-settings update separately. Factory-reset survival is not promised. Uninstallation is manual: stop/disable the service, remove only the generated hook/unit and dedicated installation directory after deciding whether to retain secrets/configuration.

## Extending and qualifying

Implement `src/backend.rs::KvmBackend`, wire the new configuration/constructor, and run the existing contract tests against the new adapter. Status, targets/capabilities and media inventory use shared Rust types, not vendor-specific JSON shapes. Report unsupported capabilities explicitly. Do not expose a generic unaudited vendor-URL proxy. Future optional features—Wake-on-LAN, EDID/settings, live WebRTC/audio/camera, device firmware updates—are not advertised as implemented by the core tools.

The capture WebSocket drains events and sends keepalives while needed, then closes after 30 seconds without another capture. An idle MCP server does not deliberately keep the firmware video encoder active forever.

Before relying on real hardware: verify exact firmware, local listener/authentication, native MCP image rendering, all channel HDMI/USB mappings, BIOS keys/keymap/pointer geometry, ATX presence and safe power actions, boot-media mount/eject/switch behavior, network interruption recovery, reboot persistence and measured memory/CPU. Source inspection and mock tests do not substitute for those checks.

## Source basis and licensing

This is an independent MIT implementation of the documented/source-visible protocol, not a repackaging of vendor firmware. Protocol research used GL.iNet `glkvm` commit `3e8dd23c4bd638a4650433664cc3b0f3b4d29395` and published RM4PE 1.10.1 firmware. The vendor's public source version and installed firmware can differ.

- GL.iNet APIs: https://github.com/gl-inet/glkvm/tree/3e8dd23c4bd638a4650433664cc3b0f3b4d29395/kvmd/apps/kvmd/api
- Rust SDK: https://github.com/modelcontextprotocol/rust-sdk
- Built-in Tailscale: https://docs.gl-inet.com/kvm/en/faq/remote_access_via_tailscale/
- Tailscale Serve: https://tailscale.com/kb/1242/tailscale-serve

Dependencies retain their own licenses. No private hostnames, accounts, workspace identifiers, credentials or deployment assumptions belong in this repository.
