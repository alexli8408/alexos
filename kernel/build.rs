//! Points the linker at linker.ld with an absolute path, so the build works
//! regardless of the directory cargo happens to be invoked from.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let script = dir.join("linker.ld");

    println!("cargo:rustc-link-arg=-T{}", script.display());
    // Frame pointers are what make kernel backtraces possible; see backtrace.rs.
    println!("cargo:rustc-link-arg=--gc-sections");

    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rerun-if-changed=src/entry.S");
}
