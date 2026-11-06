use std::env;
use std::path::PathBuf;

mod dependency_guard;

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    dependency_guard::check(&manifest_dir.join("../.."));
}
