# xtcc

**A Rust control layer for macOS TCC research.**

[Simplified Chinese](README_CN.md) · [Validation](VALIDATION.md)

xtcc provides a command-line interface for managing temporary TCC bypass sessions and inspecting permission-check activity on development Macs. It combines a Rust CLI and broker with the native hooks from [tcc-bypass](https://github.com/unixzii/tcc-bypass).

> **Experimental:** intended for isolated Macs you own or are authorized to administer. Builds and offline tests pass; live injection and end-to-end behavior have not been verified. This is not a replacement for an application's normal permission requests.

## Features

- **Time-limited controls** with configurable expiration and broker reuse.
- **Structured logs** with replay, live streaming, snapshots, and JSON output.
- **Bounded memory use** with explicit log-gap reporting for slow readers.
- **Rust resource management** for Mach port rights, request tracking, and connection lifetimes.
- **Read-only diagnostics** to check tools and build artifacts without attaching to system processes.

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
| `libxtcc_inject.dylib` | Ad-hoc-signed `arm64e` native library |

Building and running `doctor` do not load the library or start the broker. `doctor` checks files and tool locations, not runtime permissions.

The Makefile uses `$HOME/.cargo/bin/cargo` by default. Override it with `make CARGO=/path/to/cargo` if needed.

## Command reference

| Command | Description |
| --- | --- |
| `build [--force]` | Build the CLI and native library. |
| `doctor` | Check tool locations and build artifacts. |
| `start [minutes]` | Enable or renew a temporary session. Defaults to 30 minutes; accepts 1-1440. Alias: `enable`. |
| `stop` | Disable the bypass without unloading the native library. Alias: `disable`. |
| `status` | Report connections, current state, expiration, and log counts. |
| `logs` | Replay buffered records, then follow new activity. |
| `logs --snapshot` | Read the current buffer and exit. |
| `logs --all` | Include Apple attribution records hidden by default. |

The global `--json` flag produces machine-readable output; log streams use newline-delimited JSON. Log filtering affects display only, not the scope of the bypass.

Control operations require root. xtcc never invokes `sudo` automatically, and elevated privileges alone do not guarantee debugger attachment or library loading. The upstream implementation expects SIP to be disabled; xtcc does not change SIP.

### Library location

Set `XTCC_LIBRARY` to override the native library path. Otherwise, xtcc looks beside the executable, then in the source directory. Do not replace build artifacts while they are in use.

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

Live injection, cross-version compatibility, and recovery from system-service failures remain unverified. See [VALIDATION.md](VALIDATION.md) for the exact test scope and results.

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
