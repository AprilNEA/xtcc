# xtcc validation results

The original results below describe the TCC-only implementation. See
[Network Extension validation](#network-extension-validation) for the NE runtime
checks added on November 7, 2026. Live TCC injection remains unverified.

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

## Network Extension validation

Content-filter approval, configuration, and real traffic handling passed on an
isolated Mac on November 7, 2026. The host and provider used ad-hoc signatures,
without an Apple developer certificate, Team ID, or provisioning profile. No
approval dialog was accepted manually.

### Environment and checks

- macOS 27.2, build 26B123, on Apple Silicon.
- SIP was already disabled. Boot arguments were empty. The implementation did
  not change SIP, AMFI, boot arguments, or system-extension developer mode.
- Xcode 27.2, Apple Clang 21.0.0, and Rust 1.99.1.
- `make` built the arm64 CLI and both arm64e libraries. All three artifacts
  passed `codesign --verify --strict`; native compilation used
  `-Wall -Wextra -Werror`.
- `make check` passed formatting, Clippy with warnings denied, 75 module tests,
  and 6 CLI integration tests. CI runs source checks rather than macOS runtime
  tests, so the complete test suite ran locally on the Mac.
- The same acceptance script passed 36 assertions on each of three fresh
  fixture pairs: debug, release, and the final release executable relocated
  beside both libraries outside the source directory. Read-only `doctor`
  confirmed that the relocated executable resolved the adjacent libraries.

### Observed before and after behavior

- **Two approval boundaries:** each fresh host's public activation request
  returned `USER_APPROVAL_REQUIRED`. After `ne approve`, the host's ordinary
  configuration save still timed out after 20 seconds without consent handling.
  `ne filter` saved the configuration and started the provider. Subsequently,
  the ordinary host could read and save its own configuration without a prompt.
- **Actual filtering:** pass-through mode returned HTTP 200 from `example.com`.
  After supplying a vendor rule for `com.apple.curl`, Apple curl failed with
  exit code 7, and provider logs independently recorded the selected flow being
  blocked. An otherwise equivalent client with a different signing identifier
  still returned HTTP 200 from the same URL. Disabling restored Apple curl's
  HTTP 200 response; re-enabling restored filtering.
- **Target isolation:** approving the target left a second, separately
  registered control extension unapproved. Removing the target configuration
  left the pre-existing configuration metadata and enabled states unchanged.
- **Persistence and idempotence:** the host read the exact nested vendor plist.
  Repeating `filter` without an override preserved that dictionary and reused
  the configuration UUID. Repeated approval, enable, and disable succeeded.
- **Rejected writes:** an invalid vendor plist left the saved vendor dictionary
  unchanged. An altered host Info.plist failed signature validation. Missing
  extensions, missing configuration UUIDs, and malformed UUIDs failed.
- **Failure boundaries:** an injected hanging test library caused `--timeout 1`
  to terminate the client and report the timeout within 5 seconds. Running
  without root exited with code 77 before launching an Apple client.
- **Removal:** removing the active configuration invoked the provider's stop
  callback. The owning host then read an empty, disabled configuration.

### Findings during validation

The initial implementation treated an identical save as a failure. macOS
reports that case as `NEConfigurationErrorDomain` code 9. The final implementation
treats only that explicit unchanged-save result as success and returns
`unchanged: true`; all other save errors remain failures.

An early fixture incorrectly required the provider PID to disappear after
disable. macOS stopped filtering but retained the idle provider process. The
final acceptance checks use the stop callback and restored traffic instead.
A relocation assertion also needed canonical paths because `/tmp` resolves to
`/private/tmp` on macOS. Neither correction changed product behavior.

### Packet-tunnel VPN and DNS proxy follow-up

On the same macOS build, a fresh isolated Mac passed real packet-tunnel and DNS
proxy acceptance. SIP was already disabled and boot arguments were empty. All
three new fixture applications used ad-hoc signatures with no Team ID or
provisioning profile. No approval dialog was accepted manually.

The script passed 71 assertions with the debug executable and another 71 with
the release executable relocated beside both libraries outside the source tree.
The final relocated build also passed the unchanged content-filter script's
36 assertions. `make` and `make check` passed, including Clippy with warnings
denied, all 81 tests, native warnings-as-errors, and signature verification for
all three build artifacts. The shared configuration refactor had separately
passed the 36 content-filter assertions before VPN and DNS behavior was added.

- **Starting state:** the earlier executable rejected `ne vpn` and
  `ne dns-proxy`. Fresh hosts had no saved configurations. Each system extension
  required approval, and ordinary host saves still timed out after approval of
  the extension alone. The new commands created the configurations without
  accepting a consent dialog.
- **Real VPN traffic:** a `NEPacketTunnelProvider` installed only the test
  destination route, `198.18.0.42/32`. A separate application sent a binary,
  asymmetric UDP payload to that address. The provider forwarded the complete
  IPv4 packet to an independent UDP peer, which used Scapy 2.8.0 to construct
  the return packet. The application received the exact reversed payload with
  the expected prefix and peer address. Provider and peer logs independently
  confirmed both directions. Ordinary HTTPS traffic still returned HTTP 200.
- **Real DNS traffic:** a `NEDNSProxyProvider` forwarded queries for the isolated
  DNS destination `198.18.0.53:53` to an independent dnslib 0.9.26 server.
  Apple `dig` received exactly `192.0.2.17` for a UDP A query and
  `2001:db8::53` for a TCP AAAA query. The provider forwarded unrelated DNS
  flows to their original destinations; ordinary system resolution and HTTPS
  traffic continued working while both providers were active.
- **Disable and recovery:** disabling each configuration invoked its stop
  callback and made its isolated destination unreachable. Re-enabling DNS
  restored both query transports. Enabling VPN did not connect it; an explicit
  `connect` restored packet exchange. Repeated connect and disconnect succeeded.
- **Saved data and isolation:** public host APIs read the exact nested vendor
  dictionaries and VPN server address. Updates without vendor overrides
  preserved that data and the configuration UUID. Explicit overrides replaced
  the dictionaries. Invalid plist roots, wrong provider kinds, unapproved
  extensions, connecting a disabled VPN, and using a DNS UUID for connection
  control failed. The control extension stayed unapproved and pre-existing
  configuration metadata and enabled states stayed unchanged.
- **Failed and hanging tunnels:** a deliberate provider startup error returned
  failure, not connected success. A provider that withheld its startup callback
  caused `connect --timeout 2` to fail after the client deadline. The subsequent
  status was `connecting`: terminating the client does not cancel the system's
  connection attempt. `disconnect` cancelled it, and a subsequent normal start
  restored packet exchange.
- **Removal:** both owning hosts could save their configurations without another
  consent prompt. Removal invoked the stop callbacks and left no configurations
  visible through either public host API.

The proof exposed two incorrect fixture assumptions. On this OS,
`startProxyWithOptions:` supplied the DNS vendor dictionary under `VendorData`;
reading the outer dictionary prevented the fixture's test-destination redirect.
After correcting the fixture, both transports passed. Disabling VPN removed its
live SystemConfiguration service, so the status command correctly reported
`Network service for connection not available` instead of `disconnected`.
Neither correction added a product fallback or changed the traffic assertions.

An initial repeated VPN save returned success without macOS marking the save
unchanged; subsequent repeats returned `unchanged: true`. The acceptance checks
configuration UUID reuse and equality of the public host configuration rather
than requiring a particular internal save disposition. The CLI still returns
`unchanged: true` only for the explicit macOS unchanged-save result.

The thread's `ne-vpn-dns-validation.tar.gz` evidence bundle contains the exact
acceptance and provider sources, successful and failed-attempt logs, independent
peer logs, relocated `doctor` output, artifact checksums, and cleanup state.
The proof uses the real macOS extension, configuration, connection, and traffic
paths; the tunnel peer and DNS server are controlled fixtures, not third-party
VPN services. No permanent privileged test suite was added to CI.

### Remaining limits

- Runtime evidence covers self-built socket-filter, packet-tunnel VPN, and DNS
  proxy providers, not arbitrary third-party applications or packet-filter
  providers. The tunnel fixture is a test transport, not a production VPN
  protocol or encryption implementation.
- IKEv2 credential provisioning and app-proxy VPN creation are not implemented.
  State operations for existing app-VPN configurations remain untested. VPN
  creation and enablement remain separate from connecting a tunnel.
- App-extension (`.appex`) activation, provider replacement or re-signing,
  Developer ID signing, team-bound app groups, and keychain access were not
  validated.
- Reboot persistence, SIP-enabled rejection, partial SIP configurations,
  cross-version compatibility, and system-service crash recovery were not
  validated. The code uses private APIs and does not establish support for all
  systems meeting the macOS 15 deployment target.
- The TCC broker was not started, `tccd` was not attached, and TCC permission
  outcomes remain unverified.

Test configurations were removed after successful acceptance runs. Extension
registration is separate from configuration removal; the temporary test apps
and their registrations belong only to the disposable validation Mac.

## Distribution restrictions

The fixed upstream commit contains no LICENSE file.
This project preserves provenance and does not assume or assign an upstream license.
Confirm authorization from the original author before public release or redistribution.
