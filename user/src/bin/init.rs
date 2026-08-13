//! The first user process.
//!
//! Starts a shell and then does the one job init can never delegate: reaping.
//! When a process exits before its parent, it is reparented here, and if nobody
//! collects its exit status it stays a zombie forever holding a kernel stack.

#![no_std]
#![no_main]

use alexos_user::{Args, exec, fork, println, wait};

alexos_user::entry!(main);

fn main(_args: Args) -> i32 {
    println!("init: starting");

    loop {
        match fork() {
            0 => {
                exec(b"sh\0");
                // exec only returns on failure.
                println!("init: could not exec sh");
                return 1;
            }
            pid if pid > 0 => {
                // Reap everything, not just the shell: orphans land here too.
                loop {
                    let mut status = 0;
                    let reaped = wait(&mut status);
                    if reaped < 0 {
                        break;
                    }
                    if reaped == pid {
                        println!("init: shell exited with {status}, restarting");
                        break;
                    }
                }
            }
            err => {
                println!("init: fork failed with {err}");
                return 1;
            }
        }
    }
}
