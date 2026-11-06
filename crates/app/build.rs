use std::env;
use std::path::PathBuf;

mod dependency_guard;

fn main() {
    for name in [
        "MONOCODE_NATIVE_VERSION",
        "MONOCODE_NATIVE_UPDATER_ENDPOINT",
        "MONOCODE_NATIVE_UPDATER_PUBKEY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let version = env::var("MONOCODE_NATIVE_VERSION")
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").unwrap());
    println!("cargo:rustc-env=MONOCODE_NATIVE_VERSION={version}");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    dependency_guard::check(&manifest_dir.join("../.."));
}
