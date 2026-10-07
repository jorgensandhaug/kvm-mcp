# kvm-mcp

A Linux-only Rust MCP server for controlling physical computers through an IP KVM.
The first backend supports **GL.iNet Comet X (GL-RM4PE)**; a hardware-neutral Rust
trait keeps other KVM adapters separate from the MCP tools.

```text
Agent → private HTTPS / MCP → kvm-mcp → Comet HTTP/WebSocket APIs → computer
File-transfer client → scoped HTTP upload/download link → virtual-media storage
```

Runs directly on the Comet or another Linux host. One executable; no Node,
Python, database or cloud service at runtime. MIT licensed.

## What it does

- Select one of four targets and capture screenshots as native MCP images.
- Send keyboard chords/text and mouse movement, clicks and scrolling.
- Control power through an attached compatible ATX accessory.
- List, mount, eject and delete virtual-media images.
- Issue expiring, single-use upload/download links so large files bypass MCP messages.

## Build

The checked toolchain is Rust **1.95.0**. From this repository:

```sh
cargo build --release --locked
cargo test --locked
./target/release/kvm-mcp --version
```

Linux ARM64 and x86-64 musl builds are covered by the CI workflow. See the
[build and installation guide](docs/guide.md#build-and-run) for cross-compiling,
checksummed installation and persistent Comet/systemd startup. Source publication
does not imply a downloadable binary release exists.

## Configure and connect

1. Copy [the example configuration](examples/config.toml) outside the checkout.
2. Create separate mode-0600 files for the MCP bearer token and Comet password.
   Use a randomly generated MCP token of at least 32 characters.
3. Set the backend origin and the private HTTPS origin clients can reach.
   On-appliance firmware may require the supplied
   [loopback proxy](docs/guide.md#build-and-run).
4. Run `kvm-mcp --config /path/config.toml`, then expose its loopback listener
   through a private HTTPS proxy such as Tailscale Serve.
5. Configure your MCP client with `https://<your-private-hostname>:8443/mcp`
   and an `Authorization: Bearer …` header.

Both the MCP client and file-transfer client must have private network access.
The server does not enroll devices, change firewall rules or publish itself.

## Limits and documentation

Physically tested on Comet X firmware **1.8.1 release2**, including booting an
installer through virtual media and surviving an appliance reboot. This is an
initial implementation, not universal firmware support or a security audit.

- ATX needs the accessory; physical power/reset remains unverified.
- Text is keyboard emulation, not clipboard access.
- Transfers are single-use and not resumable; they serialize other operations.
- Firmware-update preservation is untested. Factory-reset survival is not promised.

See the [full guide](docs/guide.md), [validation record](VALIDATION.md),
[contribution guide](CONTRIBUTING.md) and [security policy](SECURITY.md).
