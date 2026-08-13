//! Points the linker at linker.ld with an absolute path, so the build works
//! regardless of the directory cargo happens to be invoked from.

use std::path::{Path, PathBuf};

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let script = dir.join("linker.ld");

    embed_user_programs(&dir);

    println!("cargo:rustc-link-arg=-T{}", script.display());
    // Frame pointers are what make kernel backtraces possible; see backtrace.rs.
    println!("cargo:rustc-link-arg=--gc-sections");

    println!("cargo:rerun-if-changed={}", script.display());
    println!("cargo:rerun-if-changed=src/entry.S");
}

/// Generate the table of embedded user programs.
///
/// Scans `user/build/` for ELF images and emits a `PROGRAMS` slice of
/// `(name, include_bytes!(...))`. Generating this rather than hand-listing the
/// programs means the kernel builds cleanly whether or not the user crate has
/// been built yet -- an empty table is a valid outcome, not a build failure.
fn embed_user_programs(kernel_dir: &Path) {
    let build_dir = kernel_dir.join("../user/build");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("programs.rs");

    let mut names: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&build_dir) {
        for entry in entries.flatten() {
            if entry.path().is_file()
                && let Some(name) = entry.file_name().to_str()
            {
                names.push(name.to_string());
            }
        }
    }
    // Sorted so the generated file is byte-identical between builds and does
    // not defeat caching just because readdir returned a different order.
    names.sort();

    let mut src = String::from(
        "/// (name, ELF image) for every program embedded at build time.\n\
         pub static PROGRAMS: &[(&str, &[u8])] = &[\n",
    );
    for name in &names {
        let path = build_dir.join(name).canonicalize().unwrap();
        src.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            name,
            path.display().to_string()
        ));
    }
    src.push_str("];\n");

    std::fs::write(&out, src).expect("failed to write the program table");
    println!("cargo:rerun-if-changed={}", build_dir.display());
}
