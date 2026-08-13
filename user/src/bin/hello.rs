//! Smallest possible user program: proves the loader, the syscall path and the
//! return to user mode all work.

#![no_std]
#![no_main]

use alexos_user::{Args, getpid, getppid, println};

alexos_user::entry!(main);

fn main(args: Args) -> i32 {
    println!("hello from userspace!");
    println!("  pid {}, parent {}", getpid(), getppid());
    println!("  argc {}", args.len());
    for (i, arg) in args.enumerate() {
        println!("  argv[{i}] = {}", core::str::from_utf8(arg).unwrap_or("<non-utf8>"));
    }
    0
}
