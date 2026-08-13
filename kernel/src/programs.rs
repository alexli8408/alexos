//! User programs built into the kernel image.
//!
//! Until there is a disk to read from, `exec` resolves names against a table
//! of ELF images embedded at build time -- the same idea as an initramfs, minus
//! the archive format. `build.rs` scans `user/build/` and generates the table,
//! so adding a program is a matter of adding a file to the user crate.
//!
//! The images are `include_bytes!`d into `.rodata`, which means they are mapped
//! read-only and the loader parses them straight out of the kernel image with
//! no copy.

include!(concat!(env!("OUT_DIR"), "/programs.rs"));

/// Look up a program by name, with or without a leading slash.
pub fn find(name: &str) -> Option<&'static [u8]> {
    let name = name.strip_prefix('/').unwrap_or(name);
    PROGRAMS.iter().find(|(n, _)| *n == name).map(|(_, image)| *image)
}

/// Names of every embedded program, for `ls` and for the shell's error message.
pub fn names() -> impl Iterator<Item = &'static str> {
    PROGRAMS.iter().map(|(n, _)| *n)
}

/// How many programs are embedded.
pub fn count() -> usize {
    PROGRAMS.len()
}
