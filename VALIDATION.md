# Validation of 0.1.0

Checked October 7, 2026, using Rust 1.95.0.

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked`: 8 unit tests and 6 integration tests passed.
- Integration tests use a local HTTP/WebSocket Comet simulator, including native MCP image output, input, target selection, power, media mounting and streaming transfers. They are not physical-device tests.
- Linux ARM64 and x86-64 musl release executables built successfully, without an ELF interpreter dependency.
- x86-64 executable starts natively; ARM64 executable starts under QEMU user emulation. Both report `kvm-mcp 0.1.0`.
- Installer staging tests passed for manual, Comet and systemd modes, including checksum rejection, configuration preservation and binary backup. These staging tests do not start services; the separate physical Comet test below does.
- Publication review tightened secret-file reads to check the opened descriptor,
  reject executable permissions and bound reads even if a file grows. Regression
  coverage includes oversized and executable secret files. This change has not
  been redeployed to the physical appliance.
- CI workflow is supplied but has not been executed on a hosted CI service.

## Physical qualification: Comet X 1.8.1 release2

- ARM64 musl executable installed and running on the appliance.
- MCP reached through private Tailscale HTTPS; missing bearer token rejected.
- Authenticated status, fresh JPEG captures, keyboard text/chords, pointer motion,
  and selecting the connected fourth port verified.
- Uploaded a 1,738,014,720-byte installer image through an MCP transfer ticket,
  downloaded it through a second ticket, and matched the official SHA-256.
- Mounted the image read-only as optical media and booted the connected computer
  into its live installer using MCP keyboard control.
- Verified that switching away from the selected port is rejected while the
  installer media remains connected, with the selected port unchanged.
- Found and fixed a client read-timeout that aborted uploads awaiting response
  headers after 60 seconds. Control request deadlines remain bounded separately;
  long transfers use the configured transfer timeout.
- Ejected the installer after successful SSD installation, uploaded/deleted a
  disposable probe image, and verified a revoked download grant returns HTTP 410.
- Switched from the connected fourth port to the empty first port and back after
  ejecting media. This verifies selection, not operation of another attached PC.
- Rebooted the appliance: custom startup launched the backend proxy and MCP,
  Tailscale Serve returned automatically, and an authenticated fresh console
  capture succeeded. The executable checksum was unchanged.
- No USB ATX accessory was present: firmware reports ATX disabled. Physical
  power-button/reset behavior is not verified.

## Physical qualification still required

Verify computers attached to other physical ports and an attached USB ATX board.
Firmware-update preservation remains untested. ARM emulation alone does not
establish any of these properties.

Input acknowledgement does not establish that the guest has finished processing
all keystrokes. Long text can still be appearing in an immediately requested
screenshot. Observe the console before making a dependent decision; do not
blindly repeat text merely because an early capture shows only a prefix.

Large transfers stream without full-file buffering or staging. They currently hold the backend operation lock, so other control operations wait for the transfer. Transfers are not resumable. This is an initial implementation, not a claim of production qualification or an independent security audit.
