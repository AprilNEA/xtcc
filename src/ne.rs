//! One-shot NetworkExtension administration, independent of the TCC broker.

use crate::bootstrap;
use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

#[derive(Debug, Args)]
pub(crate) struct Provider {
    extension: String,
    #[arg(
        long,
        value_name = "PLIST",
        help = "Replace the provider's vendor configuration dictionary"
    )]
    vendor_configuration: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    #[command(about = "List registered NE system extensions and saved network configurations")]
    List,
    #[command(
        about = "Approve one registered NE system extension; does not create a configuration"
    )]
    Approve { extension: String },
    #[command(about = "Create or enable a content filter for an approved system extension")]
    Filter(Provider),
    #[command(about = "Create or enable a DNS proxy for an approved system extension")]
    DnsProxy(Provider),
    #[command(about = "Create or enable a packet-tunnel VPN; does not connect the tunnel")]
    Vpn {
        #[command(flatten)]
        provider: Provider,
        #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new(), help = "Server address required by the tunnel provider")]
        server_address: String,
    },
    #[command(about = "Connect an enabled VPN and wait for its connected state")]
    Connect { configuration: String },
    #[command(about = "Disconnect a VPN and wait for its disconnected state")]
    Disconnect { configuration: String },
    #[command(about = "Read the live connection state of a saved VPN")]
    Status { configuration: String },
    #[command(about = "Enable a saved configuration by UUID; does not connect a VPN")]
    Enable { configuration: String },
    #[command(about = "Disable a saved configuration by UUID; does not uninstall its extension")]
    Disable { configuration: String },
    #[command(about = "Remove one saved configuration by UUID; does not uninstall its extension")]
    Remove { configuration: String },
}

enum Service {
    Extensions,
    Configurations,
}

unsafe extern "C" {
    fn csr_check(mask: u32) -> libc::c_int;
}

impl Service {
    fn invoke(&self, request: Value, timeout: u32) -> Result<Value> {
        let library = bootstrap::NativeLibrary::NetworkExtension.materialize()?;
        ensure!(
            !library.as_os_str().as_encoded_bytes().contains(&b':'),
            "DYLD_INSERT_LIBRARIES cannot represent a library path containing ':'"
        );
        let executable = match self {
            Self::Extensions => "/System/Library/ExtensionKit/Extensions/SettingsSystemExtensionController.appex/Contents/MacOS/SettingsSystemExtensionController",
            Self::Configurations => "/usr/libexec/nehelper",
        };
        let mut command = ProcessCommand::new(executable);
        command
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("XTCC_NE_ONESHOT", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if matches!(self, Self::Extensions) {
            command.env(
                "XPC_SERVICE_NAME",
                "com.apple.SystemExtensions-Settings.extension",
            );
        }
        let arm_timeout = move || {
            // SAFETY: alarm takes no pointers and arms only the newly spawned child's timer.
            unsafe { libc::alarm(timeout) };
            Ok(())
        };
        // SAFETY: The closure only calls the async-signal-safe alarm function after fork.
        unsafe { command.pre_exec(arm_timeout) };
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot launch NE client {executable}"))?;
        let mut input = child.stdin.take().context("NE client stdin is not piped")?;
        let written = serde_json::to_writer(&mut input, &request);
        drop(input);
        let output = child
            .wait_with_output()
            .context("cannot wait for NE client")?;
        if output.status.signal() == Some(libc::SIGALRM) {
            bail!("NE client timed out after {timeout}s; inspect ne list and, for VPNs, ne status before retrying a write");
        }
        written.context("cannot send NE request")?;
        let response: Value = serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "NE client returned no valid reply ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )
        })?;
        ensure!(
            response["ok"] == true,
            "NE operation failed: {}",
            response["error"]
        );
        ensure!(
            output.status.success(),
            "NE client failed: {}",
            output.status
        );
        Ok(response)
    }
}

impl Provider {
    fn configure(self, mut request: Value, timeout: u32) -> Result<Value> {
        let vendor_configuration = self
            .vendor_configuration
            .map(|path| {
                path.canonicalize()
                    .with_context(|| format!("cannot read {}", path.display()))
            })
            .transpose()?;
        let extensions = Service::Extensions.invoke(json!({"operation": "extensions"}), timeout)?;
        let matches: Vec<_> = extensions["extensions"]
            .as_array()
            .context("invalid NE extension list")?
            .iter()
            .filter(|item| item["identifier"] == self.extension)
            .collect();
        ensure!(
            matches.len() == 1,
            "expected one registered NE extension for {}; found {}. Activate it from its containing application first",
            self.extension,
            matches.len()
        );
        let target = matches[0];
        ensure!(
            target["enabled"] == true && target["active"] == true,
            "NE extension {} is not active and approved; run xtcc ne approve first",
            self.extension
        );
        request["extension"] = target.clone();
        request["vendor_configuration"] = json!(vendor_configuration);
        Service::Configurations.invoke(request, timeout)
    }
}

pub(crate) fn run(command: Command, timeout: u32, json_output: bool) -> Result<i32> {
    // sys/csr.h: CSR_ALLOW_UNRESTRICTED_FS and CSR_ALLOW_TASK_FOR_PID.
    const REQUIRED_SIP_EXCEPTIONS: u32 = (1 << 1) | (1 << 2);
    // SAFETY: csr_check reads the active SIP configuration and takes no pointers.
    ensure!(
        unsafe { csr_check(REQUIRED_SIP_EXCEPTIONS) } == 0,
        "NE controls require SIP filesystem and debugging restrictions to be disabled; xtcc does not change SIP"
    );
    let response = match command {
        Command::List => {
            let extensions =
                Service::Extensions.invoke(json!({"operation": "extensions"}), timeout)?;
            let configurations =
                Service::Configurations.invoke(json!({"operation": "configurations"}), timeout)?;
            json!({"ok": true, "extensions": extensions["extensions"],
                "configurations": configurations["configurations"]})
        }
        Command::Approve { extension } => Service::Extensions.invoke(
            json!({"operation": "approve", "identifier": extension}),
            timeout,
        )?,
        Command::Filter(provider) => provider.configure(json!({"operation": "filter"}), timeout)?,
        Command::DnsProxy(provider) => {
            provider.configure(json!({"operation": "dns-proxy"}), timeout)?
        }
        Command::Vpn {
            provider,
            server_address,
        } => provider.configure(
            json!({"operation": "vpn", "server_address": server_address}),
            timeout,
        )?,
        Command::Connect { configuration } => Service::Configurations.invoke(
            json!({"operation": "connect", "identifier": configuration}),
            timeout,
        )?,
        Command::Disconnect { configuration } => Service::Configurations.invoke(
            json!({"operation": "disconnect", "identifier": configuration}),
            timeout,
        )?,
        Command::Status { configuration } => Service::Configurations.invoke(
            json!({"operation": "status", "identifier": configuration}),
            timeout,
        )?,
        Command::Enable { configuration } => Service::Configurations.invoke(
            json!({"operation": "enable", "identifier": configuration}),
            timeout,
        )?,
        Command::Disable { configuration } => Service::Configurations.invoke(
            json!({"operation": "disable", "identifier": configuration}),
            timeout,
        )?,
        Command::Remove { configuration } => Service::Configurations.invoke(
            json!({"operation": "remove", "identifier": configuration}),
            timeout,
        )?,
    };
    let mut stdout = io::stdout().lock();
    if json_output {
        serde_json::to_writer(&mut stdout, &response)?;
    } else {
        serde_json::to_writer_pretty(&mut stdout, &response)?;
    }
    writeln!(stdout)?;
    Ok(0)
}
