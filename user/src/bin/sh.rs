//! The AlexOS shell.
//!
//! Reads a line, splits it into arguments, and either handles it as a builtin
//! or forks and execs it. That fork/exec split is the reason `cd` and `exit`
//! have to be builtins: a child cannot change its parent's state, so anything
//! that must outlive the command has to run in the shell itself.
//!
//! There is no heap in userland, so the argument vector is built in place: the
//! separators in the input line are overwritten with NULs and the pointer array
//! points into that same buffer. Nothing is copied and nothing is allocated.

#![no_std]
#![no_main]

use alexos_user::{Args, exec_raw, fork, print, println, ps, read_line, uptime_ms, waitpid};

alexos_user::entry!(main);

/// Longest command line accepted.
const LINE_MAX: usize = 256;
/// Most arguments in one command, including the program name.
const ARGS_MAX: usize = 16;

fn main(_args: Args) -> i32 {
    println!();
    println!("AlexOS shell. Type `help` for the command list.");

    let mut line = [0u8; LINE_MAX];

    loop {
        print!("$ ");
        let len = read_line(&mut line);
        if len == 0 {
            continue;
        }

        // Split in place. `argv` ends up pointing into `line`, which is why the
        // whole command has to be dispatched before the next read overwrites it.
        let mut argv = [0usize; ARGS_MAX + 1];
        let argc = match split(&mut line[..len], &mut argv) {
            Some(argc) => argc,
            None => {
                println!("sh: too many arguments (limit {ARGS_MAX})");
                continue;
            }
        };
        if argc == 0 {
            continue;
        }

        // SAFETY: `split` NUL-terminated every argument inside `line` and left
        // `argv` NULL-terminated.
        let name = unsafe { cstr_at(argv[0]) };

        match name {
            b"exit" => {
                println!("sh: goodbye");
                return 0;
            }
            b"help" => builtin_help(),
            b"uptime" => {
                let ms = uptime_ms();
                println!("up {}.{:03}s", ms / 1000, ms % 1000);
            }
            b"ps" => ps(),
            _ => run_external(&mut line[..len], &argv, argc),
        }
    }
}

/// Fork, exec the command in the child, and wait for it in the parent.
fn run_external(line: &mut [u8], argv: &[usize; ARGS_MAX + 1], _argc: usize) {
    match fork() {
        0 => {
            // SAFETY: argv points into `line`, which is this process's own
            // stack and stays valid until exec replaces the image.
            let err = unsafe { exec_raw(line, argv.as_ptr()) };
            // Reached only if exec failed; the name is still in the buffer.
            println!("sh: command not found (error {err})");
            alexos_user::exit(127);
        }
        pid if pid > 0 => {
            let mut status = 0;
            waitpid(pid, &mut status);
            if status != 0 {
                println!("sh: exited with status {status}");
            }
        }
        err => println!("sh: fork failed with {err}"),
    }
}

fn builtin_help() {
    println!("builtins:");
    println!("  help          this message");
    println!("  exit          leave the shell");
    println!("  uptime        milliseconds since boot");
    println!("  ps            list tasks");
    println!();
    println!("anything else is looked up as a program, for example:");
    println!("  hello         print a greeting");
    println!("  forktest      exercise fork and wait");
    println!("  echo a b c    print the arguments back");
}

/// Split `line` on whitespace, NUL-terminating each field in place and filling
/// `argv` with pointers to them.
///
/// Returns the argument count, or `None` if there are more than `ARGS_MAX`.
fn split(line: &mut [u8], argv: &mut [usize; ARGS_MAX + 1]) -> Option<usize> {
    let mut argc = 0;
    let mut i = 0;

    while i < line.len() {
        while i < line.len() && line[i] == b' ' {
            // Overwrite separators so the preceding field is NUL-terminated.
            line[i] = 0;
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        if argc == ARGS_MAX {
            return None;
        }
        argv[argc] = line[i..].as_ptr() as usize;
        argc += 1;
        while i < line.len() && line[i] != b' ' {
            i += 1;
        }
    }

    // The last field runs to the end of the buffer, where `read_line` left the
    // byte after the text untouched -- so terminate it explicitly.
    if argc > 0 && line.len() < LINE_MAX {
        // Safe because `line` is a prefix of a LINE_MAX buffer and there is at
        // least one byte past `len`.
        // SAFETY: writing one byte past the slice, still inside the backing
        // array the caller owns.
        unsafe { *line.as_mut_ptr().add(line.len()) = 0 };
    }

    argv[argc] = 0;
    Some(argc)
}

/// Borrow the NUL-terminated string at `ptr`.
///
/// # Safety
/// `ptr` must point at a NUL-terminated string that outlives the borrow.
unsafe fn cstr_at(ptr: usize) -> &'static [u8] {
    // SAFETY: contract delegated to the caller.
    unsafe {
        let p = ptr as *const u8;
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        core::slice::from_raw_parts(p, len)
    }
}
