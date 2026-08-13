//! In-kernel test harness.
//!
//! `#[test]` needs a host process, a test runner and `std`; none of those exist
//! here, and the standard workaround -- `#![feature(custom_test_frameworks)]`
//! -- would put the whole kernel on nightly for the sake of the test suite.
//!
//! So tests register themselves at *link* time instead. `ktest!` emits a
//! descriptor into a dedicated `.ktest` section, the linker script gathers them
//! between `__ktest_start` and `__ktest_end`, and the runner walks that range as
//! a slice. Adding a test anywhere in the kernel requires no central list to
//! update, and the whole mechanism is stable Rust plus four lines of linker
//! script.
//!
//! Tests run inside a kernel task, so the ones that block on a wait queue or
//! yield behave exactly as they would in production. The suite reports its
//! result through the SiFive test finisher, which sets QEMU's exit status, so
//! `make test` fails a CI job rather than merely printing.

use core::sync::atomic::{AtomicUsize, Ordering};

/// One registered test.
///
/// `repr(C)` because the linker lays these out as a raw array and the runner
/// reinterprets that range as `&[KTest]`; the field order has to be the
/// declaration order.
#[repr(C)]
pub struct KTest {
    /// Test name, as written in the source.
    pub name: &'static str,
    /// The test body. Signals failure by panicking, usually via `assert!`.
    pub func: fn(),
}

// SAFETY: `KTest` holds a `&'static str` and a plain function pointer, both of
// which are freely shareable. The impl is needed only because the descriptors
// live in a `static`.
unsafe impl Sync for KTest {}

/// Register a test.
///
/// ```ignore
/// ktest!(two_plus_two, {
///     assert_eq!(2 + 2, 4);
/// });
/// ```
#[macro_export]
macro_rules! ktest {
    ($name:ident, $body:block) => {
        // `used` is essential: nothing references these statics, so without it
        // the compiler discards them long before the linker could collect them.
        #[used]
        // Named like a function because that is what it reads as at the call
        // site; the static is an implementation detail of the registration.
        #[allow(non_upper_case_globals)]
        #[unsafe(link_section = ".ktest")]
        static $name: $crate::ktest::KTest = $crate::ktest::KTest {
            name: concat!(module_path!(), "::", stringify!($name)),
            func: || $body,
        };
    };
}

unsafe extern "C" {
    safe static __ktest_start: [u8; 0];
    safe static __ktest_end: [u8; 0];
}

/// Failures observed so far. A test signals failure by panicking, and the panic
/// handler stops the kernel, so in practice this is 0 or the run ended early --
/// but it keeps the reporting honest if that ever changes.
static FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Every registered test, gathered by the linker.
fn tests() -> &'static [KTest] {
    let start = (&raw const __ktest_start) as usize;
    let end = (&raw const __ktest_end) as usize;
    let count = (end - start) / core::mem::size_of::<KTest>();

    // SAFETY: the linker script places nothing but `KTest` descriptors between
    // these two symbols, and the region is 8-byte aligned by the `. = ALIGN(8)`
    // that precedes it.
    unsafe { core::slice::from_raw_parts(start as *const KTest, count) }
}

/// Entry point for the test task. `spawn` wants a `fn()`, and `run_all`
/// diverges, so this adapts between them.
pub fn run_all_task() {
    run_all()
}

/// Run every registered test, then exit QEMU with a status that reflects the
/// result. Never returns.
pub fn run_all() -> ! {
    let tests = tests();
    crate::println!();
    crate::info!("running {} kernel tests", tests.len());

    let started = crate::timer::uptime_ms();

    for test in tests {
        crate::print!("  {} ... ", test.name);
        (test.func)();
        // Reaching here means no panic, which is the only failure channel.
        crate::println!("\x1b[32mok\x1b[0m");
    }

    let elapsed = crate::timer::uptime_ms() - started;
    let failures = FAILURES.load(Ordering::Relaxed);

    crate::println!();
    if failures == 0 {
        crate::info!("all {} tests passed in {} ms", tests.len(), elapsed);
        crate::test_device::exit_success()
    } else {
        crate::error!("{failures} of {} tests failed", tests.len());
        crate::test_device::exit_failure()
    }
}
