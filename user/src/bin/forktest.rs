//! Exercises fork and waitpid.
//!
//! Each child writes to a stack variable the parent also has. If fork were
//! sharing memory instead of copying it, the parent's copy would change too --
//! so the value the parent prints at the end is the actual test.

#![no_std]
#![no_main]

use alexos_user::{Args, exit, fork, getpid, println, wait};

alexos_user::entry!(main);

const CHILDREN: usize = 3;

fn main(_args: Args) -> i32 {
    let mut sentinel = 0xAAu64;
    println!("forktest: parent is pid {}, sentinel {:#x}", getpid(), sentinel);

    for i in 0..CHILDREN {
        match fork() {
            0 => {
                // Child. Scribble on the sentinel; the parent must not see it.
                sentinel = 0xBB + i as u64;
                println!("  child {} (pid {}) set sentinel {:#x}", i, getpid(), sentinel);
                exit(i as i32 + 1);
            }
            pid if pid > 0 => println!("  forked child {i} as pid {pid}"),
            err => {
                println!("forktest: fork failed with {err}");
                return 1;
            }
        }
    }

    let mut reaped = 0;
    while reaped < CHILDREN {
        let mut status = 0;
        let pid = wait(&mut status);
        if pid < 0 {
            println!("forktest: wait failed with {pid}");
            return 1;
        }
        println!("  reaped pid {pid} with status {status}");
        reaped += 1;
    }

    println!("forktest: parent sentinel is still {sentinel:#x}");
    if sentinel != 0xAA {
        println!("forktest: FAILED -- a child's write reached the parent");
        return 1;
    }
    println!("forktest: ok");
    0
}
