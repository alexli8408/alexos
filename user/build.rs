//! Link user programs at the fixed base in linker.ld.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let script = dir.join("linker.ld");

    println!("cargo:rustc-link-arg=-T{}", script.display());
    println!("cargo:rerun-if-changed={}", script.display());
}
