# xtcc

**CLI controls for macOS TCC bypass sessions and Network Extensions on development Macs.**

[Simplified Chinese](README_CN.md) · [Validation](VALIDATION.md)

`xtcc` is a single-binary controller for temporary TCC permission bypasses and Network Extension (`ne`) configurations on isolated, SIP-disabled Apple Silicon Macs. Its `arm64e` Objective-C payloads are compiled and ad-hoc-signed at build time (`build.rs`), embedded into the `xtcc` executable, and materialized on demand under `/private/var/run/xtcc`:

- **TCC bypass (`start`, `stop`, `status`, `logs`):** injects the Objective-C hooks from [tcc-bypass](https://github.com/unixzii/tcc-bypass) (`libxtcc_inject.dylib`) into `tccd` via LLDB and coordinates time-limited sessions and structured permission-check logs through a Rust Mach broker (`dev.xtcc.control`).
- **Network Extensions (`ne`):** launches short-lived Apple client binaries with `libxtcc_ne.dylib` to approve staged system extensions and configure Content Filters, DNS Proxies, and Packet-Tunnel VPNs—including ad-hoc-signed bundles without an Apple Developer certificate.

> **Experimental:** use `xtcc` only on isolated Macs you own or are authorized to administer. NE controls are validated on macOS 27.2 with self-built content-filter, DNS-proxy, and packet-tunnel fixtures; live TCC injection, cross-version compatibility, and service-failure recovery remain unverified (see [VALIDATION.md](VALIDATION.md)). `xtcc` does not replace standard code signing, provisioning, or user permission requests in distributed applications.

## Quickstart

Runtime control commands require an Apple Silicon Mac (macOS 15.0+), `root` (`sudo`), and disabled SIP restrictions (TCC injection also requires Python-enabled LLDB). `xtcc` never invokes `sudo` automatically and never modifies SIP, AMFI, boot arguments, developer mode, or running NE daemons.

Download the standalone Apple Silicon binary, license, and checksum from [GitHub Releases](https://github.com/AprilNEA/xtcc/releases), verify the downloads, and install the binary onto your `PATH`:

```sh
curl --fail --location --remote-name https://github.com/AprilNEA/xtcc/releases/latest/download/xtcc &&
curl --fail --location --remote-name https://github.com/AprilNEA/xtcc/releases/latest/download/xtcc.LICENSE &&
curl --fail --location --remote-name https://github.com/AprilNEA/xtcc/releases/latest/download/xtcc.sha256 &&
shasum --algorithm 256 --check xtcc.sha256 &&
sudo install -m 0755 xtcc /usr/local/bin/xtcc &&
sudo install -d /usr/local/share/licenses/xtcc &&
sudo install -m 0644 xtcc.LICENSE /usr/local/share/licenses/xtcc/LICENSE &&
xtcc doctor
```

Alternatively, with the [build requirements](#build-requirements) installed, build from crates.io with `cargo install xtcc --locked`. For a local checkout, run `make` and install `target/release/xtcc` instead.

`xtcc doctor` checks runtime tool paths (`xcrun` and LLDB) and embedded payloads; Rust, Cargo, `make`, and a compiler are not required. The check is read-only and requires neither `root` privileges nor library extraction or process attachment.

The runtime directory and extracted libraries use permissions `0755`, independent of the caller's `umask`.

## TCC Bypass

Start a timed bypass session, inspect connected `tccd` processes and permission-check events, and stop the bypass when finished:

```sh
sudo xtcc start 30
sudo xtcc status
sudo xtcc logs --snapshot
sudo xtcc stop
```

Session and log behavior:

- **Session lifetime:** `start [minutes]` (alias `enable`) defaults to 30 minutes and accepts `1`–`1440`. Running `start` again reuses a healthy broker and updates the expiration time.
- **Disabling without unloading:** `stop` (alias `disable`) stops bypassing checks without unloading `libxtcc_inject.dylib`; method replacements remain in `tccd` until the process exits. The injected hooks also stop bypassing automatically if they lose their connection to the broker.
- **Log streaming and filtering:** `logs` replays buffered events and streams new ones (`--snapshot` exits after replay; `--all` includes `com.apple.*` responsible processes hidden by default). Filtering affects display only, not the scope of the bypass.
- **Buffer limits:** logs stay in memory with a capacity of 1,024 records and 8 MiB of serialized data (16 KiB per message body). Slow readers receive gap notices, dropped logs never block permission checks, and logs clear when the broker exits.
- **Diagnostics and interpretation:** broker lifecycle messages go to system syslog under the `xtcc:` prefix without permission events. A `BYPASS` log entry means a hook supplemented an entitlement check, not that the overall permission request succeeded. Do not use `xtcc` and upstream `tcc-bypass` against the same `tccd` process.

## Network Extensions

`xtcc ne` operates independently of the TCC broker and requires `root` plus disabled SIP filesystem and debugging restrictions. It separates **system extension approval** (`OSSystemExtensionClient`) from **network configuration management** (`NEConfigurationManager`):

1. **Stage the extension from the host app:** install the signed host application in `/Applications` and trigger its standard `OSSystemExtensionRequest` so the extension reaches the awaiting-approval state. For ad-hoc builds without an Apple Developer certificate, sign the embedded provider bundle first and then the host app with `codesign --sign - --entitlements <file>`. Both bundles must declare valid NE entitlements, bundle structure, and Mach-service or app-group identifiers.
2. **Approve and configure with `xtcc ne`:** run `ne approve <extension>` with the extension bundle identifier from `ne list`, then run `ne filter`, `ne dns-proxy`, or `ne vpn` to create or update the host's network configuration without a consent dialog. `xtcc` binds the provider's designated requirement from its on-disk code signature (including the code-directory hash for ad-hoc signatures) and rejects unsigned or tampered bundles.

All three creation commands accept `--vendor-configuration <plist>` with a provider-defined `NSDictionary` plist. Passing the flag replaces saved vendor settings; omitting it preserves existing settings.

### Content Filter

List registered extensions, approve the staged filter provider by bundle identifier, and create or enable its content-filter configuration:

```sh
sudo xtcc ne list
sudo xtcc ne approve com.example.FilterApp.provider
sudo xtcc ne filter com.example.FilterApp.provider
```

### DNS Proxy

Approve the staged DNS proxy provider and create or enable its configuration (omit `--vendor-configuration` if your provider needs no vendor plist):

```sh
sudo xtcc ne approve com.example.DNSProxyApp.provider
sudo xtcc ne dns-proxy com.example.DNSProxyApp.provider --vendor-configuration dns-proxy.plist
```

macOS launches DNS proxy providers directly from the enabled configuration; they do not use `ne connect`, `ne disconnect`, or `ne status`. Use the returned `configuration.identifier` UUID to disable, re-enable, or delete the configuration:

```sh
DNS_UUID='replace-with-configuration-uuid'
sudo xtcc ne list
sudo xtcc ne disable "$DNS_UUID"
sudo xtcc ne enable "$DNS_UUID"
sudo xtcc ne remove "$DNS_UUID"
```

### Packet-Tunnel VPN

Approve the staged packet-tunnel provider and create its VPN configuration with a non-empty `--server-address`:

```sh
sudo xtcc ne approve com.example.VPNApp.provider
sudo xtcc ne vpn com.example.VPNApp.provider --server-address vpn.example.com --vendor-configuration vpn.plist
```

Creating a VPN configuration enables it but does not start the tunnel. Pass `configuration.identifier` from the command output (or `ne list`) to `ne connect`, `ne status`, `ne disconnect`, `ne disable`, `ne enable`, or `ne remove`:

```sh
VPN_UUID='replace-with-configuration-uuid'
sudo xtcc ne connect "$VPN_UUID"
sudo xtcc ne status "$VPN_UUID"
sudo xtcc ne disconnect "$VPN_UUID"
```

`connect` waits until macOS reports `connected`; `disconnect` waits for `disconnected` and leaves the configuration enabled. If you disable the configuration with `ne disable "$VPN_UUID"`, run `ne enable "$VPN_UUID"` before reconnecting.

### Operational Rules and Limits

- **Persistence:** NE changes persist in system preferences and do not use the TCC session timer. `xtcc stop` does not revert them; clean up with `ne disable <uuid>` or `ne remove <uuid>`.
- **Supported provider scope:** `ne` creates configurations for content-filter, DNS-proxy, and packet-tunnel system extensions. It does not implement IKEv2 credential provisioning, app-proxy VPN creation, or `.appex` activation.
- **Signing and provider updates:** `xtcc` does not re-sign third-party apps or supply entitlements, provisioning profiles, team-bound app groups, or keychain access. After replacing or re-signing a provider, reactivate it from the host app and rerun the creation command to refresh its designated requirement.
- **Traffic verification:** `active`, `enabled`, and `connected` report registration and session states, not working traffic. Verify provider logs and actual traffic (both UDP and TCP queries for DNS proxies, and routed packets for VPNs). When a disabled VPN has no live SystemConfiguration service, `ne status` reports the service as unavailable rather than `disconnected`.
- **Idempotence:** repeated approvals and identical saves reuse the existing configuration UUID (`unchanged: true` when macOS reports `NEConfigurationErrorDomain` code 9). Multiple VPN configurations matching one provider are rejected.
- **Timeouts:** each client call defaults to `--timeout 30` (`1`–`300` seconds). After a timeout, check `ne list` (and `ne status` for VPNs) before retrying; if `connect` times out while macOS is still connecting, run `ne disconnect <uuid>` to cancel the attempt. See [NE validation](VALIDATION.md#network-extension-validation) for tested scenarios.

## Command Reference

### Top-Level Commands

| Command | Description |
| --- | --- |
| `build [--force]` | Build the standalone CLI with embedded native libraries via `make`; requires the original source checkout. |
| `doctor` | Check runtime tool paths and embedded native libraries in read-only mode without root. |
| `start [minutes]` | Enable or renew a TCC bypass session (`1`–`1440`, default `30`). Alias: `enable`. |
| `stop` | Disable the active TCC bypass without unloading `libxtcc_inject.dylib`. Alias: `disable`. |
| `status` | Show connected `tccd` processes, bypass state, expiration, and log counts. |
| `logs [--snapshot] [--all]` | Replay and follow TCC logs (`--snapshot` reads once; `--all` includes `com.apple.*`). |
| `ne [--timeout <seconds>] <subcommand>` | Manage Network Extensions (`--timeout` accepts `1`–`300`, default `30`). |

### Network Extension Subcommands

| Subcommand | Target | Description |
| --- | --- | --- |
| `ne list` | *(none)* | List registered NE system extensions and saved network configurations. |
| `ne approve <extension>` | Bundle ID | Approve one staged NE system extension. |
| `ne filter <extension> [--vendor-configuration <plist>]` | Bundle ID | Create or update and enable a Content Filter configuration. |
| `ne dns-proxy <extension> [--vendor-configuration <plist>]` | Bundle ID | Create or update and enable a DNS Proxy configuration. |
| `ne vpn <extension> --server-address <addr> [--vendor-configuration <plist>]` | Bundle ID | Create or update and enable a Packet-Tunnel VPN configuration without connecting. |
| `ne connect <uuid>` | Config UUID | Start an enabled VPN session and wait for `connected`. |
| `ne disconnect <uuid>` | Config UUID | Stop a VPN session and wait for `disconnected`. |
| `ne status <uuid>` | Config UUID | Read live VPN connection status via SystemConfiguration. |
| `ne enable <uuid>` | Config UUID | Enable a saved `contentFilter`, `dnsProxy`, `VPN`, or `appVPN` configuration. |
| `ne disable <uuid>` | Config UUID | Disable a saved configuration without uninstalling its system extension. |
| `ne remove <uuid>` | Config UUID | Delete a saved configuration without uninstalling its system extension. |

Pass the global `--json` flag for compact machine-readable JSON output (`logs --json` emits newline-delimited JSON; `ne` outputs pretty-printed JSON by default and compact JSON with `--json`). Vendor dictionaries and credentials are never printed.

## Development

### Build Requirements

- Apple Silicon Mac (`arm64` / `arm64e`), targeting macOS 15.0 or later (runtime compatibility depends on private macOS frameworks for the active OS version).
- Rust and Cargo (the development toolchain is pinned in `rust-toolchain.toml`).
- Xcode with the macOS SDK, Apple Clang, and Python-enabled LLDB.
- System `make` and `codesign`.

### Build and Check

Run from the repository root:

```sh
make
make check
```

- **`make`:** runs `cargo build --release --locked` using Cargo from `PATH`. `build.rs` compiles and ad-hoc-signs both `arm64e` native libraries (`libxtcc_inject.dylib` and `libxtcc_ne.dylib`) and embeds them into `target/release/xtcc`. Use `make CARGO=/path/to/cargo` to select an explicit Cargo executable.
- **`make rebuild` / `xtcc build --force`:** clears only `xtcc` release artifacts before rebuilding the executable and both native libraries. Dependency caches and debug artifacts are retained.
- **`make check`:** runs `cargo fmt --check`, Clippy, and the offline test suite (protocol encoding, native ABI compatibility, embedded library materialization, forced builds, Mach right ownership, broker state, log buffering, and CLI output). Build regression tests use an isolated temporary target directory. Loopback tests use local test ports only and never attach to `tccd` or alter system permissions.
- **CI:** the [Source checks](.github/workflows/source-checks.yml) workflow verifies Rust formatting, Python syntax, and English-only repository text outside `README_CN.md`.

### Publish

Before publishing, confirm permission to redistribute upstream-derived code. Store a crates.io API token as the repository Actions secret `CARGO_REGISTRY_TOKEN`; crates.io requires a token for the first publication of a new crate.

Push a tag named `v<version>`, matching `package.version` in `Cargo.toml`. The [Release workflow](.github/workflows/release.yml) validates the tag and license metadata, then runs two jobs on Apple Silicon macOS:

- **GitHub Release:** runs `make check` and `cargo publish --dry-run --locked`, builds and verifies the standalone executable, and uploads `xtcc`, `xtcc.LICENSE`, and `xtcc.sha256`. The checksum covers both the binary and the license.
- **crates.io:** after the GitHub Release succeeds, publishes the verified source package. The package includes the native sources, headers, and LLDB bootstrap required for installation.

Run the Release workflow manually to preflight a branch before tagging. Manual runs perform the checks, builds, and package verification without creating a GitHub Release or uploading a crate.

If a publication fails, rerun only the failed job. Published crate versions cannot be replaced; use a new version and tag for subsequent changes.

## Acknowledgements

Thanks to [unixzii](https://github.com/unixzii) for [tcc-bypass](https://github.com/unixzii/tcc-bypass), which established the TCC hook protocol and lifecycle design used here. The following files are retained from commit `78da98104bafff7ecb96935cf198cb4196a28ffd`:

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

Service and logging identifiers use the `dev.xtcc` namespace (`dev.xtcc.control` and `dev.xtcc`), while exported `cyan_tccd_*` symbols remain unchanged.

**Licensing:** xtcc is released under the [MIT License](LICENSE). The referenced upstream commit did not include a `LICENSE` file; the project-level MIT declaration does not itself establish permission from the original author.
