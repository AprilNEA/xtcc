//! xtcc：开发环境使用的 macOS TCC 控制程序。
//!
//! 测试只操作本进程创建的资源，不附加或修改系统进程。
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
