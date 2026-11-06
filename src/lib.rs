//! xtcc: a macOS TCC control program for development environments.
//!
//! Tests use only resources created by their own process and never attach to or modify system processes.
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(target_os = "macos"))]
compile_error!("xtcc currently supports macOS only");

pub mod bootstrap;
pub mod broker;
pub mod cli;
pub mod client;
pub(crate) mod lifecycle;
pub mod mach;
pub mod process;
pub mod protocol;
pub mod ring;
