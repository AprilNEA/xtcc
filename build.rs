use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Module {
    name: &'static str,
    sources: &'static [&'static str],
    headers: &'static [&'static str],
    linker_flags: &'static [&'static str],
}

const MODULES: &[Module] = &[
    Module {
        name: "libxtcc_inject.dylib",
        sources: &["native/tccd_inject.m", "native/Attribution.m"],
        headers: &["native/Attribution.h", "native/ControlProtocol.h"],
        linker_flags: &["-lbsm"],
    },
    Module {
        name: "libxtcc_ne.dylib",
        sources: &["native/ne.m", "native/ne/connection.m"],
        headers: &["native/ne/connection.h"],
        linker_flags: &[
            "-framework",
            "NetworkExtension",
            "-framework",
            "SystemExtensions",
            "-framework",
            "Security",
            "-framework",
            "SystemConfiguration",
        ],
    },
];

fn run(command: &mut Command, step: &str) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{step} could not start: {error}"));
    assert!(
        output.status.success(),
        "{step} failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn build_module(root: &Path, out_dir: &Path, module: &Module) {
    for path in module.sources.iter().chain(module.headers.iter()) {
        println!("cargo::rerun-if-changed={path}");
    }
    let output_path = out_dir.join(module.name);
    let install_name = format!("-Wl,-install_name,@rpath/{}", module.name);
    let mut clang = Command::new("/usr/bin/xcrun");
    clang
        .current_dir(root)
        .args([
            "clang",
            "-arch",
            "arm64e",
            "-fobjc-arc",
            "-framework",
            "Foundation",
        ])
        .args(module.linker_flags)
        .args([
            "-mmacosx-version-min=15.0",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-dynamiclib",
            &install_name,
            "-o",
        ])
        .arg(&output_path)
        .args(module.sources);
    run(&mut clang, &format!("compile {}", module.name));

    let mut codesign = Command::new("/usr/bin/codesign");
    codesign
        .args(["--force", "--sign", "-", "--identifier", "dev.xtcc"])
        .arg(&output_path);
    run(&mut codesign, &format!("codesign {}", module.name));
}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    for module in MODULES {
        build_module(&root, &out_dir, module);
    }
}
