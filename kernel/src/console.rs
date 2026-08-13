//! Kernel console: `print!`, `println!`, and the log macros.
//!
//! Output has two backends. Before `drivers::uart` binds, writes go through
//! SBI, which costs a trap per byte but is available from the first instruction
//! of the kernel. Afterwards the real NS16550A driver takes over. Panics always
//! use SBI regardless, because the reason for the panic may well be the driver.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sbi;
use crate::sync::SpinLock;

/// Flipped once the UART driver is ready to take writes.
static UART_READY: AtomicBool = AtomicBool::new(false);

/// Serialises writers so concurrent `println!`s do not interleave mid-line.
static CONSOLE_LOCK: SpinLock<()> = SpinLock::new(());

struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if UART_READY.load(Ordering::Relaxed) {
            crate::drivers::uart::write_bytes(s.as_bytes());
        } else {
            sbi::console_write(s.as_bytes());
        }
        Ok(())
    }
}

/// Hand console output to the UART driver. Called once the driver is mapped
/// and its FIFOs are configured.
pub fn use_uart_backend() {
    UART_READY.store(true, Ordering::Release);
}

/// Implementation detail of the `print!` family.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let _guard = CONSOLE_LOCK.lock();
    let _ = Console.write_fmt(args);
}

/// Write to the console without taking the lock.
///
/// The panic handler uses this so that a panic *inside* a console critical
/// section still produces output instead of deadlocking. Interleaved garbage
/// beats silence when the machine is going down.
#[doc(hidden)]
pub fn _print_unlocked(args: fmt::Arguments<'_>) {
    struct Raw;
    impl Write for Raw {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            sbi::console_write(s.as_bytes());
            Ok(())
        }
    }
    let _ = Raw.write_fmt(args);
}

/// Write formatted output to the console with no trailing newline.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::console::_print(format_args!($($arg)*)));
}

/// Write formatted output to the console, followed by a newline.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::console::_print(format_args!("{}\n", format_args!($($arg)*))));
}

// ---------------------------------------------------------------------------
// Levelled logging.
//
// ANSI colour is applied unconditionally: the only consumer is a terminal
// attached to QEMU's serial port, and the escape sequences make a wall of boot
// output scannable.
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub mod color {
    pub const RESET: &str = "\x1b[0m";
    pub const RED: &str = "\x1b[31m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const GREEN: &str = "\x1b[32m";
    pub const BLUE: &str = "\x1b[34m";
    pub const GRAY: &str = "\x1b[90m";
}

/// Something went wrong but the kernel is continuing.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => ($crate::println!("{}[E] {}{}",
        $crate::console::color::RED, format_args!($($arg)*), $crate::console::color::RESET));
}

/// Unexpected but recoverable.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => ($crate::println!("{}[W] {}{}",
        $crate::console::color::YELLOW, format_args!($($arg)*), $crate::console::color::RESET));
}

/// Normal progress reporting: subsystem came up, device found.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => ($crate::println!("{}[I] {}{}",
        $crate::console::color::GREEN, format_args!($($arg)*), $crate::console::color::RESET));
}

/// Detail useful when a subsystem misbehaves.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => ($crate::println!("{}[D] {}{}",
        $crate::console::color::BLUE, format_args!($($arg)*), $crate::console::color::RESET));
}

/// Very high volume; compiled out unless the `trace` feature is on.
#[macro_export]
#[cfg(feature = "trace")]
macro_rules! trace {
    ($($arg:tt)*) => ($crate::println!("{}[T] {}{}",
        $crate::console::color::GRAY, format_args!($($arg)*), $crate::console::color::RESET));
}

/// No-op stand-in for `trace!` when the `trace` feature is off. The arguments
/// are still type-checked so a disabled trace cannot rot.
#[macro_export]
#[cfg(not(feature = "trace"))]
macro_rules! trace {
    ($($arg:tt)*) => {{ let _ = format_args!($($arg)*); }};
}
