//! Runtime items the compiler expects a `no_std` binary to provide.

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::arch;
use crate::sbi::{self, ResetType};

/// Guards against a panic raised while already panicking, which would
/// otherwise recurse until the stack runs out and turn a legible message into
/// a silent hang.
static PANIC_DEPTH: AtomicUsize = AtomicUsize::new(0);

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    // SAFETY: the machine is going down; no further scheduling should occur.
    unsafe { arch::intr_disable() };

    let depth = PANIC_DEPTH.fetch_add(1, Ordering::Relaxed);
    if depth > 0 {
        // Second panic: say the minimum and stop, using the rawest output path.
        crate::console::_print_unlocked(format_args!("\n[!] double panic, halting\n"));
        halt();
    }

    // Deliberately the unlocked printer: the panic may have come from inside a
    // console critical section, and a deadlock here would cost us the message.
    crate::console::_print_unlocked(format_args!(
        "\n\x1b[1;31m[PANIC]\x1b[0m hart {} ",
        arch::hart_id()
    ));
    match info.location() {
        Some(loc) => crate::console::_print_unlocked(format_args!(
            "at {}:{}:{}\n",
            loc.file(),
            loc.line(),
            loc.column()
        )),
        None => crate::console::_print_unlocked(format_args!("at <unknown location>\n")),
    }
    crate::console::_print_unlocked(format_args!("        {}\n", info.message()));

    crate::backtrace::print();

    // Under `make test` the harness wants a non-zero exit code, not a hang.
    if cfg!(feature = "exit-on-panic") {
        crate::test_device::exit_failure();
    }

    sbi::shutdown(ResetType::Shutdown)
}

/// Stop this hart forever.
fn halt() -> ! {
    loop {
        arch::wait_for_interrupt();
    }
}
