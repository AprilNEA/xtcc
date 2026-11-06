# xtcc validation results

The initial Rust control layer built successfully.
All 79 tests passed; no live injection was performed.
Initial validation completed on November 4, 2026, at 07:02 UTC+08:00.

## English localization regression

On November 6, 2026, the localized repository passed all 80 tests:
75 module tests and 5 CLI integration tests. The additional CLI test checks
English help and error output without invoking control operations.

Formatting checks, Clippy with warnings denied, the release build, and signature
verification for both artifacts passed. CLI help and the read-only doctor
command were also exercised. No live injection or service startup was performed.

A scan of the current repository files found no Chinese text or CJK punctuation
outside `README_CN.md`. Comments and CLI help are English. The Chinese README
remains available as the explicitly requested translation.

## Environment

- macOS 26.7, build 25G229.
- Apple Silicon, arm64 architecture.
- Rust 1.98.1.
- Apple Clang 21.0.0, Xcode 27 SDK.
- Minimum deployment target: macOS 15.0 for both Rust and native code.

Cargo declares Rust 1.85 as the minimum supported version.
Only Rust 1.98.1 was tested; the minimum version was not verified.

## Verified

`cargo fmt --check` passed.
`cargo clippy --all-targets --locked -- -D warnings` passed.
`cargo test --locked --offline` passed.
The suite included 75 module tests and 4 CLI integration tests.

`make` successfully produced both build artifacts:

- `target/release/xtcc`: an arm64 executable.
- `libxtcc_inject.dylib`: an arm64e dynamic library.

Both passed `codesign --verify --strict`.

CLI help, version output, and the read-only doctor command were exercised.
Both text and JSON doctor output worked correctly.
The embedded LLDB Python script passed a syntax check but was not executed.

## Key test coverage

Protocol tests cover message lengths, operation types, and invalid input.
The native ABI fixture uses the project's actual protocol header.
It compares Objective-C and Rust message layouts field by field.
It also compares the encoded bytes of equivalent JSON messages.

Mach tests create ports only within the test process.
They cover right transfers, reference counts, and death notifications.
They also cover queue timeouts, invalid-message cleanup, and rejection of forged notifications.
Tests do not look up or register the production service.

Broker tests cover handshakes, timeouts, and completeness checks in response aggregation.
Log tests cover entry capacity, byte limits, and replay boundaries.
Subscription tests cover slow readers, gaps, and cursor advancement.
CLI tests cover argument ranges, aliases, and hidden commands.
Output tests cover JSON, filtering, and terminal control-character sanitization.

## Not verified

- Live LLDB attachment, dynamic-library loading, and readiness.
- Full sessions between the Rust broker and a real injected endpoint.
- Cross-process port extraction and production service registration.
- Actual permission outcomes and recovery from system-service crashes.
- Compatibility across macOS versions.
- Display of background syslog diagnostics in system logs.

Neither `start` nor `_serve` was run.
No sudo commands were used, and SIP was not changed.
The TCC database was not modified.
The project had not been published at the time of the initial validation.

## Changes from upstream

The outer Python launcher and native helper control logic were rewritten in Rust.
LLDB's embedded Python remains in use.
Only namespace identifiers were changed in the Objective-C injection implementation.
The dynamic library now has a stable install name.

The program no longer invokes sudo automatically.
Users must explicitly decide whether to run it as root.
Argument validation takes place before runtime operations.
The new doctor command checks tools and files without modifying them.

Rust ownership types manage Mach rights.
Replies are pinned to the complete audit token after the handshake.
Requests check connection generations and operation matching.

Broker lifecycle messages use system syslog.
Permission events remain confined to a bounded in-memory buffer.
Some detailed port-extraction error diagnostics have not been preserved.

## Distribution restrictions

The fixed upstream commit contains no LICENSE file.
This project preserves provenance and does not assume or assign an upstream license.
Confirm authorization from the original author before public release or redistribution.
