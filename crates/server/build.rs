use std::{env, fs, path::PathBuf};

/// Expose the linked source version and match upstream's Windows stack requirement.
fn main() {
    let manifest=PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../vendor/codex/codex-rs/Cargo.toml");
    let source: toml::Value=toml::from_str(&fs::read_to_string(&manifest).unwrap()).unwrap();
    println!("cargo:rerun-if-changed={}",manifest.display());
    println!("cargo:rustc-env=CODEX_VERSION={}",source["workspace"]["package"]["version"].as_str().unwrap());
    if env::var("CARGO_CFG_TARGET_OS").unwrap()=="windows" { println!("cargo:rustc-link-arg=/STACK:8388608"); }
}
