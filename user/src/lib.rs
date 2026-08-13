//! Userland runtime for AlexOS.
//!
//! Provides `_start`, the syscall wrappers, `print!`/`println!`, and a panic
//! handler. Programs written against it look like ordinary Rust with a `main`.
//!
//! There is deliberately no heap. Every program here is small enough to work
//! out of fixed-size stack buffers, and a userland allocator would be a second
//! allocator to get right for no benefit at this size. The consequence is
//! visible in the API: things like `read_line` take a caller-provided buffer
//! rather than returning a `String`.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

use core::arch::asm;
use core::panic::PanicInfo;

#[path = "../../abi/syscall.rs"]
pub mod nr;

/// Raw system call. Only `ecall` and register placement; every wrapper below
/// goes through here.
///
/// # Safety
/// The arguments must be valid for the syscall being made -- pointers must be
/// readable or writable for the stated length, and so on. The kernel validates
/// them and returns an error rather than faulting, so the practical risk is a
/// wrong answer, not memory unsafety.
#[inline(always)]
unsafe fn syscall(id: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    // SAFETY: `ecall` traps to the kernel, which preserves everything except
    // a0 per the ABI in abi/syscall.rs.
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") a0 => ret,
            in("a1") a1,
            in("a2") a2,
            in("a7") id,
            options(nostack),
        );
    }
    ret
}

/// Terminate the process.
pub fn exit(code: i32) -> ! {
    // SAFETY: no pointer arguments.
    unsafe { syscall(nr::SYS_EXIT, code as usize, 0, 0) };
    unreachable!("exit returned")
}

/// Write bytes to a file descriptor. Returns the count written.
pub fn write(fd: usize, buf: &[u8]) -> isize {
    // SAFETY: `buf` is a live slice, so pointer and length agree.
    unsafe { syscall(nr::SYS_WRITE, fd, buf.as_ptr() as usize, buf.len()) }
}

/// Read into `buf`. Returns the count read, which may be less than requested.
pub fn read(fd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: `buf` is a live mutable slice.
    unsafe { syscall(nr::SYS_READ, fd, buf.as_mut_ptr() as usize, buf.len()) }
}

/// Give up the rest of this quantum.
pub fn sched_yield() {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_YIELD, 0, 0, 0) };
}

/// This process's id.
pub fn getpid() -> usize {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_GETPID, 0, 0, 0) as usize }
}

/// The parent's process id.
pub fn getppid() -> usize {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_GETPPID, 0, 0, 0) as usize }
}

/// Duplicate this process. Returns 0 in the child and the child's pid in the
/// parent.
pub fn fork() -> isize {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_FORK, 0, 0, 0) }
}

/// Replace this image. `path` and every entry of `argv` must be NUL-terminated.
/// Returns only on failure.
///
/// # Safety
/// `argv` must be a NULL-terminated array of pointers to NUL-terminated
/// strings, all valid for the duration of the call.
pub unsafe fn exec_raw(path: &[u8], argv: *const usize) -> isize {
    // SAFETY: contract delegated to the caller; `path` is a live slice.
    unsafe { syscall(nr::SYS_EXEC, path.as_ptr() as usize, argv as usize, 0) }
}

/// Replace this image with `path`, passing no arguments beyond the name.
pub fn exec(path: &[u8]) -> isize {
    let argv: [usize; 2] = [path.as_ptr() as usize, 0];
    // SAFETY: `argv` is NULL-terminated and both entries outlive the call.
    unsafe { exec_raw(path, argv.as_ptr()) }
}

/// Wait for a child to exit. Returns its pid, and stores its status in `status`.
pub fn waitpid(pid: isize, status: &mut i32) -> isize {
    // SAFETY: `status` is a live reference to an i32.
    unsafe { syscall(nr::SYS_WAITPID, pid as usize, status as *mut i32 as usize, 0) }
}

/// Wait for any child.
pub fn wait(status: &mut i32) -> isize {
    waitpid(-1, status)
}

/// Move the program break. Returns the previous break.
pub fn sbrk(delta: isize) -> isize {
    // SAFETY: no pointer arguments.
    unsafe { syscall(nr::SYS_SBRK, delta as usize, 0, 0) }
}

/// Milliseconds since boot.
pub fn uptime_ms() -> usize {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_UPTIME, 0, 0, 0) as usize }
}

/// Ask the kernel to print the task table.
pub fn ps() {
    // SAFETY: no arguments.
    unsafe { syscall(nr::SYS_PS, 0, 0, 0) };
}

/// Read one line from standard input into `buf`, echoing as it goes.
///
/// Returns the number of bytes in the line, excluding the terminator. The
/// kernel hands over raw characters, so line editing -- echo and backspace --
/// belongs here, the same division of labour as a Unix tty in raw mode.
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let mut byte = [0u8; 1];
        if read(nr::STDIN, &mut byte) <= 0 {
            continue;
        }
        match byte[0] {
            b'\r' | b'\n' => {
                write(nr::STDOUT, b"\n");
                return len;
            }
            0x7f | 0x08 => {
                if len > 0 {
                    len -= 1;
                    // Back up, overwrite with a space, back up again: the only
                    // way to erase a character on a dumb terminal.
                    write(nr::STDOUT, b"\x08 \x08");
                }
            }
            c if (c.is_ascii_graphic() || c == b' ') && len < buf.len() => {
                buf[len] = c;
                len += 1;
                write(nr::STDOUT, &byte);
            }
            // Ignore anything else: control characters and escape sequences
            // would otherwise end up in the command line.
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Formatted output
// ---------------------------------------------------------------------------

struct Stdout;

impl core::fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        write(nr::STDOUT, s.as_bytes());
        Ok(())
    }
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    let _ = Stdout.write_fmt(args);
}

/// Print to standard output.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::_print(format_args!($($arg)*)));
}

/// Print to standard output, followed by a newline.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::_print(format_args!("{}\n", format_args!($($arg)*))));
}

// ---------------------------------------------------------------------------
// Runtime entry
// ---------------------------------------------------------------------------

unsafe extern "Rust" {
    /// Defined by each program through the `entry!` macro.
    fn __alexos_main(argc: usize, argv: *const *const u8) -> i32;
}

/// Process entry point. The kernel starts every program here with `a0` = argc
/// and `a1` = argv, matching the C convention.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
// The pointer is not dereferenced here, only forwarded; `Args::new` is the
// unsafe boundary that vouches for it.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: `__alexos_main` is defined by the program's `entry!` invocation,
    // and the kernel set up argc/argv before the first instruction ran.
    let code = unsafe { __alexos_main(argc, argv) };
    exit(code)
}

/// Declare a program's entry point.
///
/// ```ignore
/// alexos_user::entry!(main);
/// fn main(args: Args) -> i32 { 0 }
/// ```
#[macro_export]
macro_rules! entry {
    ($main:ident) => {
        #[unsafe(no_mangle)]
        fn __alexos_main(argc: usize, argv: *const *const u8) -> i32 {
            // SAFETY: argc and argv come from the kernel, which built the
            // vector on this process's own stack.
            $main(unsafe { $crate::Args::new(argc, argv) })
        }
    };
}

/// The argument vector, as an iterator of byte slices.
///
/// Slices rather than `&str` because a program name arriving from a filesystem
/// is not guaranteed to be UTF-8, and refusing to start over that would be
/// worse than passing the bytes through.
#[derive(Clone, Copy)]
pub struct Args {
    argc: usize,
    argv: *const *const u8,
    index: usize,
}

impl Args {
    /// Wrap the raw vector the kernel supplied.
    ///
    /// # Safety
    /// `argv` must point at `argc` NUL-terminated strings.
    pub unsafe fn new(argc: usize, argv: *const *const u8) -> Self {
        Self { argc, argv, index: 0 }
    }

    /// Number of arguments, including the program name.
    pub fn len(self) -> usize {
        self.argc
    }

    /// Were no arguments passed at all?
    pub fn is_empty(self) -> bool {
        self.argc == 0
    }

    /// Argument `n`, or `None` if out of range.
    pub fn get(self, n: usize) -> Option<&'static [u8]> {
        if n >= self.argc {
            return None;
        }
        // SAFETY: `n < argc`, and the constructor's contract says every entry
        // up to argc is a valid NUL-terminated string.
        unsafe {
            let p = *self.argv.add(n);
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            Some(core::slice::from_raw_parts(p, len))
        }
    }
}

impl Iterator for Args {
    type Item = &'static [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.get(self.index)?;
        self.index += 1;
        Some(item)
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    match info.location() {
        Some(loc) => {
            println!("panic at {}:{}: {}", loc.file(), loc.line(), info.message())
        }
        None => println!("panic: {}", info.message()),
    }
    exit(101)
}
