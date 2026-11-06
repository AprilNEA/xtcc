# xtcc

[Simplified Chinese](README_CN.md)

Temporarily bypass macOS TCC checks in a development environment.
The Rust control layer manages commands, connections, expiration times, and logs.
The Objective-C injection library retains the upstream implementation of private interfaces.

**This project is intended only for isolated development devices that you are authorized to administer.**
It is not a permission-request mechanism for ordinary applications.
It does not automatically make a system with SIP enabled meet the runtime requirements.

## Current Rewrite Scope

The outer Python launcher and Objective-C helper have been replaced with Rust.
The injection library is still compiled for `arm64e` using Apple Clang.
The two components communicate through Mach messages.

LLDB still executes Python scripts internally.
This project therefore does not eliminate Python entirely.
It is not a complete, pure-Rust implementation either.

## Requirements

- An Apple Silicon Mac.
- Rust and Cargo.
- Xcode, including the macOS SDK, Clang, and LLDB with Python support.
- System-provided `make` and `codesign`.

The native library's minimum deployment target is macOS 15.0.
This does not mean that every version of macOS 15 and later has been verified.
The private TCC interfaces remain dependent on the specific OS version.

Control operations require root privileges.
Injection also requires the system to permit debugger attachment and loading this library.
Upstream requires SIP to be disabled; this project does not modify SIP for you.

## Build and Offline Checks

```sh
make
make check
./target/release/xtcc --help
./target/release/xtcc doctor
```

The build produces:

- `target/release/xtcc`: the Rust command-line program and broker.
- `libxtcc_inject.dylib`: the ad-hoc-signed injection library.

Building does not load the library or start the broker.
Tests must not attach to `tccd` or change TCC permissions.
Mach loopback tests use only ports created by the tests themselves.
The native ABI fixtures only compare memory layouts and message encoding.

By default, `CARGO` points to `$HOME/.cargo/bin/cargo`.
If Cargo is installed elsewhere, run `make CARGO=/path/to/cargo`.

## Commands

`start [minutes]` temporarily enables the check bypass.
The duration defaults to 30 minutes and must be between 1 and 1440 minutes.
Repeated calls reuse a healthy broker and update the expiration time.
`enable` is an alias for `start`.

`stop` stops the bypass but does not unload the injection library.
`disable` is an alias for `stop`.
Method replacements already loaded remain in place until the corresponding process exits.

`status` queries the connection, state, expiration time, and log counts.
`logs` replays buffered records and continues reading new records.
`logs --snapshot` reads a single snapshot only.
`logs --all` includes Apple attribution records that are hidden by default.
`--json` produces machine-readable output.

`doctor` only checks tool locations and build artifacts.
It does not test debugger attachment permissions or enable the bypass.
The program never invokes `sudo` implicitly.

Use `XTCC_LIBRARY` to specify the injection library.
Otherwise, the program looks for the library next to the executable or in the source directory.
Do not replace the build artifacts used by the program while it is running.

## Behavior and Limitations

The permission-check hooks retain the upstream logic.
The injected component stops bypassing checks if it loses its connection to the broker.
Lost logs do not block permission checks.
All logs exist only in bounded in-memory buffers.

The default buffer holds 1,024 records, with a serialized-data limit of 8 MiB.
An individual message body is limited to 16 KiB.
Slow subscribers receive notifications of gaps in the logs.
Logs are not retained after the broker exits.
Broker lifecycle diagnostics are written to the system syslog.
These diagnostics begin with `xtcc:` and do not include permission events.

`BYPASS` means that a hook supplemented the entitlement-check result.
It does not guarantee that the final permission request succeeds.
The default hiding rules affect only log display, not the scope of the bypass.

The service identifier has been changed to `dev.xtcc.control`.
The logging subsystem has been changed to `dev.xtcc`.
Do not operate on the same `tccd` with this project and the upstream tool at the same time.

## Validation Status

Validation for this round is limited to builds, static checks, and offline tests.
The final results are recorded in `VALIDATION.md`.
Actual injection, compatibility across OS versions, and recovery from system-service failures have not been verified.

# Acknowlodge

Thanks to [unixzii](https://github.com/unixzii) for
[tcc-bypass](https://github.com/unixzii/tcc-bypass), whose design and implementation
provided the basis for this Rust control-layer rewrite.
The reference commit is `78da98104bafff7ecb96935cf198cb4196a28ffd`.

The following Objective-C files were retained from that commit:

- `native/tccd_inject.m`
- `native/Attribution.m`
- `native/Attribution.h`
- `native/ControlProtocol.h`

The service and logging identifiers have been changed to `dev.xtcc`.
The injection library's exported `cyan_tccd_*` symbols remain unchanged.
The Rust control layer follows the original protocol and lifecycle behavior.

No LICENSE file was found in the reference commit. The presence of upstream code
in this repository does not imply permission to redistribute it. Confirm
authorization with the original author before publishing or distributing it.
`Cargo.toml` sets `publish = false`.
