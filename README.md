# xtcc

**Development controls for macOS TCC and Network Extensions.**

[Simplified Chinese](README_CN.md) · [Validation](VALIDATION.md)

xtcc provides a command-line interface for managing temporary TCC bypass sessions and inspecting permission-check activity on development Macs. It combines a Rust CLI and broker with the native hooks from [tcc-bypass](https://github.com/unixzii/tcc-bypass).

The independent `ne` commands approve registered Network Extension system extensions and manage content filters, DNS proxies, and packet-tunnel VPNs, including ad-hoc-signed providers without an Apple developer certificate on SIP-disabled Macs.

> **Experimental:** intended for isolated Macs you own or are authorized to administer. NE runtime validation covers self-built content-filter, DNS-proxy, and packet-tunnel fixtures on macOS 27.2. Live TCC injection remains unverified. This is not a replacement for normal signing, provisioning, or user consent in distributed applications.

## Features

- **Time-limited controls** with configurable expiration and broker reuse.
- **Structured logs** with replay, live streaming, snapshots, and JSON output.
- **Bounded memory use** with explicit log-gap reporting for slow readers.
- **Rust resource management** for Mach port rights, request tracking, and connection lifetimes.
- **Read-only diagnostics** to check tools and build artifacts without attaching to system processes.
- **Targeted NE controls** for system-extension approval, provider configuration, persistent configuration enable/disable/removal, and VPN connection control.

## Getting started

### Requirements

- An Apple Silicon Mac.
- Rust and Cargo. The declared minimum Rust version is 1.85; see [validation notes](VALIDATION.md) for the tested toolchain.
- Xcode with the macOS SDK, Clang, and Python-enabled LLDB.
- The system `make` and `codesign` tools.

The build targets macOS 15.0 or later. This is a deployment target, not a compatibility guarantee: runtime support depends on private TCC interfaces and the OS version.

### Build

From the repository root:

```sh
make
./target/release/xtcc --help
./target/release/xtcc doctor
```

| Artifact | Purpose |
| --- | --- |
| `target/release/xtcc` | Rust CLI and broker |
| `libxtcc_inject.dylib` | Ad-hoc-signed `arm64e` TCC library |
| `libxtcc_ne.dylib` | Ad-hoc-signed `arm64e` one-shot NE client library |

Building and running `doctor` do not load the library or start the broker. `doctor` checks files and tool locations, not runtime permissions.

The Makefile uses `$HOME/.cargo/bin/cargo` by default. Override it with `make CARGO=/path/to/cargo` if needed.

## Command reference

| Command | Description |
| --- | --- |
| `build [--force]` | Build the CLI and native libraries. |
| `doctor` | Check tool locations and build artifacts. |
| `start [minutes]` | Enable or renew a temporary session. Defaults to 30 minutes; accepts 1-1440. Alias: `enable`. |
| `stop` | Disable the bypass without unloading the native library. Alias: `disable`. |
| `status` | Report connections, current state, expiration, and log counts. |
| `logs` | Replay buffered records, then follow new activity. |
| `logs --snapshot` | Read the current buffer and exit. |
| `logs --all` | Include Apple attribution records hidden by default. |
| `ne` | Approve NE system extensions and manage persistent network configurations. |

The global `--json` flag produces machine-readable output; log streams use newline-delimited JSON. Log filtering affects display only, not the scope of the bypass.

Control operations require root. xtcc never invokes `sudo` automatically, and elevated privileges alone do not guarantee debugger attachment or library loading. The upstream implementation expects SIP to be disabled; xtcc does not change SIP.

### Library location

Set `XTCC_LIBRARY` to override the native library path. Otherwise, xtcc looks beside the executable, then in the source directory. Do not replace build artifacts while they are in use.

Set `XTCC_NE_LIBRARY` to override the separate NE library with the same search rules. `doctor` checks both libraries without loading either one.

## Network Extensions

NE commands require root and disabled SIP filesystem/debugging restrictions. They do not modify SIP, AMFI, boot arguments, developer mode, or running system daemons. They use private APIs in newly launched, short-lived Apple clients; unsupported OS versions fail rather than installing a persistent hook.

Install your correctly structured, signed host application under `/Applications`, then let the host submit its normal `OSSystemExtensionRequest`. An extension awaiting user approval is sufficient. Replace the example identifier with the identifier shown by `ne list`:

```sh
sudo ./target/release/xtcc ne list
sudo ./target/release/xtcc ne approve com.example.FilterApp.provider
sudo ./target/release/xtcc ne filter com.example.FilterApp.provider
```

`approve` changes only the selected NE system extension. It does not register an uninstalled extension or approve a network configuration. `filter`, `dns-proxy`, and `vpn` create or update the containing application's configuration without a consent dialog. They derive the provider's designated requirement from its actual code signature, including the code hash for ad-hoc signing. Unsigned or tampered bundles fail signature validation.

The optional `--vendor-configuration` flag accepts a provider-specific plist dictionary for all three creation commands. Supplying the flag replaces vendor settings; omission preserves existing settings. If the provider needs no vendor settings, omit the flag and its file path from the examples below.

### Packet-tunnel VPN

Prepare `vpn.plist` using your provider's documented settings. Replace the example bundle identifier and server address with your provider's values, then approve the extension and create its VPN configuration:

```sh
sudo ./target/release/xtcc ne approve com.example.VPNApp.provider
sudo ./target/release/xtcc ne vpn com.example.VPNApp.provider --server-address vpn.example.com --vendor-configuration vpn.plist
```

Creation enables the configuration but does not connect. Replace the value of `VPN_UUID` below with `configuration.identifier` from the command output, not the extension's bundle identifier. You can also find the configuration UUID with `ne list`.

```sh
VPN_UUID='replace-with-configuration-uuid'
sudo ./target/release/xtcc ne connect "$VPN_UUID"
sudo ./target/release/xtcc ne status "$VPN_UUID"
sudo ./target/release/xtcc ne disconnect "$VPN_UUID"
```

`connect` waits for `connected`; `disconnect` waits for `disconnected` and leaves the saved configuration enabled for reuse. Test traffic through the routes installed by your provider before relying on the VPN. xtcc does not implement a tunnel protocol or obtain credentials.

Manage the saved configuration as needed:

- **Disable:** run `sudo ./target/release/xtcc ne disable "$VPN_UUID"`.
- **Reconnect after disabling:** run `sudo ./target/release/xtcc ne enable "$VPN_UUID"`, then `sudo ./target/release/xtcc ne connect "$VPN_UUID"`.
- **Delete:** run `sudo ./target/release/xtcc ne remove "$VPN_UUID"`.

### DNS proxy

Prepare `dns-proxy.plist` using your provider's documented settings. Replace the example bundle identifier, then approve the extension and create its DNS proxy configuration:

```sh
sudo ./target/release/xtcc ne approve com.example.DNSProxyApp.provider
sudo ./target/release/xtcc ne dns-proxy com.example.DNSProxyApp.provider --vendor-configuration dns-proxy.plist
```

macOS starts the provider from the enabled configuration; DNS proxies do not use `ne connect`, `ne disconnect`, or `ne status`. Replace the value of `DNS_UUID` below with `configuration.identifier` from the command output. Use `ne list` to inspect the saved configuration, then disable or re-enable it as needed:

```sh
DNS_UUID='replace-with-configuration-uuid'
sudo ./target/release/xtcc ne list
sudo ./target/release/xtcc ne disable "$DNS_UUID"
sudo ./target/release/xtcc ne enable "$DNS_UUID"
```

Verify both UDP and TCP DNS queries against your provider's intended rules, and inspect the provider's startup and flow logs. A saved `enabled: true` value or an ordinary successful DNS lookup alone does not prove interception. To delete the configuration, run `sudo ./target/release/xtcc ne remove "$DNS_UUID"`.

### Configuration controls and limitations

| Command | Target and effect |
| --- | --- |
| `ne list` | List NE system extensions and configuration UUIDs; do not change their state. |
| `ne approve <extension>` | Approve one registered NE system extension by bundle identifier. |
| `ne filter <extension>` | Create or enable a content filter. |
| `ne dns-proxy <extension>` | Create or enable a DNS proxy. |
| `ne vpn <extension> --server-address <address>` | Create or enable a packet-tunnel VPN; do not connect it. |
| `ne connect <uuid>` | Connect an enabled VPN and wait until macOS reports `connected`. |
| `ne disconnect <uuid>` | Disconnect a VPN and wait until macOS reports `disconnected`. |
| `ne status <uuid>` | Read the VPN's live connection state through SystemConfiguration. |
| `ne enable <uuid>` | Enable an existing content-filter, DNS-proxy, VPN, or app-VPN configuration; do not connect a VPN. |
| `ne disable <uuid>` | Disable the selected configuration without uninstalling its extension. |
| `ne remove <uuid>` | Delete the selected configuration without uninstalling its extension. |

- NE changes persist; they do not use the TCC session timer. `xtcc stop` does not undo them. Use `ne disable` or `ne remove` with the configuration UUID.
- VPN creation supports packet-tunnel system extensions, not IKEv2 credential provisioning or app-proxy VPN creation. App-extension (`.appex`) activation is not implemented.
- `enabled` and system-extension `active` describe saved configuration and registration state, not proof of provider startup or working traffic. Even a `connected` tunnel needs provider-specific traffic checks. When a disabled VPN has no live connection service, `ne status` reports that service as unavailable rather than inventing a disconnected state.
- Repeated approval and identical saves succeed without creating duplicates. A save explicitly reported unchanged by macOS returns `unchanged: true`; other errors remain failures. Multiple VPN configurations matching the same provider are rejected rather than selecting one arbitrarily.
- After a timeout, inspect `ne list` and, for VPNs, `ne status` before retrying a write. A timed-out `connect` can leave macOS still connecting; use `ne disconnect` to cancel the session.
- `ne --timeout <seconds>` bounds each client, defaults to 30 seconds, and accepts 1-300. Output is readable JSON; the global `--json` flag selects compact JSON. Vendor settings and credentials are not printed.

For an application you build without an Apple certificate, sign the embedded provider first, then the host, with `codesign --sign - --entitlements <file>`. Both still need their correct NE entitlements, bundle layout, and Mach-service/application-group declarations. xtcc does not re-sign third-party applications or supply entitlements, profiles, team-bound app-group access, or keychain access. After replacing or re-signing a provider, reactivate it from the host and rerun the matching creation command to refresh the code requirement. See [NE validation](VALIDATION.md#network-extension-validation) for tested conditions and remaining gaps.

## Architecture

The Rust CLI handles arguments and output, while a broker manages connections, requests, and log subscriptions. The native component retains the upstream Objective-C hooks and communicates with the broker through Mach messages.

The native library is built with Apple Clang for `arm64e`. LLDB still uses its embedded Python runtime for bootstrap, so xtcc is a Rust control-layer rewrite, not an entirely Rust implementation.

The control service is `dev.xtcc.control`, and the logging subsystem is `dev.xtcc`.

## Behavior and limitations

- The injected component stops bypassing checks when it loses its broker connection. `stop` does not unload it; method replacements remain until the target process exits.
- Permission logs stay in memory. The default buffer holds up to 1,024 records and 8 MiB of encoded data, with a 16 KiB limit per message body. Logs are lost when the broker exits; dropped logs do not block permission checks.
- Broker lifecycle diagnostics go to system syslog with an `xtcc:` prefix. They do not contain permission events.
- A `BYPASS` record means a hook supplemented an entitlement-check result, not that the final permission request necessarily succeeded.
- Do not use xtcc and the upstream tool against the same `tccd` process at the same time.

Live TCC injection, cross-version compatibility, and recovery from system-service failures remain unverified. See [VALIDATION.md](VALIDATION.md) for the exact test scope and results.

## Development

Run formatting checks, Clippy, and the test suite with:

```sh
make check
```

Tests cover protocol encoding, native ABI compatibility, Mach right ownership, broker state, log buffering, and CLI output. Mach loopback tests use only ports created by the test process; no tests attach to `tccd` or change TCC permissions.

The [Source checks](.github/workflows/source-checks.yml) workflow checks Rust formatting, Python syntax, and English-only source text on pushes and pull requests to `master`. It does not build binaries, run the program, upload artifacts, or publish releases. Publishing to crates.io and GitHub Releases is not configured.

# Acknowlodge

Thanks to [unixzii](https://github.com/unixzii) for [tcc-bypass](https://github.com/unixzii/tcc-bypass), the foundation for this project. The Rust control layer follows its protocol and lifecycle design.

The following files were retained from commit `78da98104bafff7ecb96935cf198cb4196a28ffd`:

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

Service and logging identifiers use the `dev.xtcc` namespace; the exported `cyan_tccd_*` symbols remain unchanged.

**Licensing:** no LICENSE file was found in the referenced upstream commit. Public source availability does not grant redistribution rights. Confirm permission with the original author before publishing or distributing upstream-derived code. Cargo publishing is disabled with `publish = false`.
