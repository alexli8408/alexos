//! The AlexOS system call ABI.
//!
//! Included verbatim by both the kernel and the user runtime, so the two can
//! never disagree about a number. A duplicated constant list is the kind of
//! thing that stays correct right up until someone inserts a syscall in the
//! middle of one copy.
//!
//! Calling convention, following the RISC-V C ABI so that a syscall costs
//! nothing to set up beyond the `ecall`:
//!
//! | register | meaning                        |
//! |----------|--------------------------------|
//! | `a7`     | syscall number                 |
//! | `a0..a5` | arguments                      |
//! | `a0`     | return value                   |
//!
//! Errors come back as small negative values, mirroring Linux. A return of
//! `-2` means `ENOENT`, and so on; user code checks `ret < 0`.

#![allow(dead_code)]

/// Terminate the calling process. `a0` is the exit status. Does not return.
pub const SYS_EXIT: usize = 0;
/// Write `a2` bytes from `a1` to file descriptor `a0`.
pub const SYS_WRITE: usize = 1;
/// Read up to `a2` bytes into `a1` from file descriptor `a0`.
pub const SYS_READ: usize = 2;
/// Give up the rest of the current scheduling quantum.
pub const SYS_YIELD: usize = 3;
/// Return the calling process's id.
pub const SYS_GETPID: usize = 4;
/// Return the parent's process id.
pub const SYS_GETPPID: usize = 5;
/// Duplicate the calling process. Returns 0 in the child, the child's pid in
/// the parent, and a negative value on failure.
pub const SYS_FORK: usize = 6;
/// Replace the current image with the program named by the NUL-terminated
/// path in `a0`, passing the NULL-terminated argument vector in `a1`.
/// Returns only on failure.
pub const SYS_EXEC: usize = 7;
/// Wait for a child to exit. `a0` is a pid, or -1 for any child; `a1` receives
/// the exit status if non-null. Returns the reaped pid.
pub const SYS_WAITPID: usize = 8;
/// Move the program break by `a0` bytes and return the previous break.
pub const SYS_SBRK: usize = 9;
/// Milliseconds since boot.
pub const SYS_UPTIME: usize = 10;
/// Write a one-line description of every task to the console.
pub const SYS_PS: usize = 11;

/// Standard input.
pub const STDIN: usize = 0;
/// Standard output.
pub const STDOUT: usize = 1;
/// Standard error.
pub const STDERR: usize = 2;

/// Operation not permitted / bad argument.
pub const EINVAL: isize = -1;
/// No such file or directory.
pub const ENOENT: isize = -2;
/// Bad file descriptor.
pub const EBADF: isize = -3;
/// Out of memory.
pub const ENOMEM: isize = -4;
/// No child processes.
pub const ECHILD: isize = -5;
/// Bad address: a pointer argument did not resolve in the caller's space.
pub const EFAULT: isize = -6;
/// Function not implemented.
pub const ENOSYS: isize = -7;
