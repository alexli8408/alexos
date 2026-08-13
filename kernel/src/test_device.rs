//! SiFive test finisher.
//!
//! QEMU's `virt` board exposes a register that terminates the emulator with a
//! chosen exit status. That is the channel the kernel test harness uses to
//! report pass or fail to `make test` -- without it, a CI job would have to
//! scrape serial output and guess.

use crate::config::TEST_DEVICE_BASE;
use crate::mm::phys_to_virt;

/// Magic values the finisher recognises.
const FINISH_PASS: u32 = 0x5555;
const FINISH_FAIL: u32 = 0x3333;

/// Terminate the emulator with the given status.
///
/// The failure encoding puts the exit code in the upper 16 bits; QEMU exits
/// with `(code << 1) | 1`, so code 1 becomes shell status 3.
fn finish(value: u32) -> ! {
    let reg = phys_to_virt(TEST_DEVICE_BASE) as *mut u32;
    // SAFETY: the finisher is a device register inside the MMIO window mapped
    // by the boot page table. The write does not return.
    unsafe { reg.write_volatile(value) };

    // Reached only if the device is absent (i.e. not running under QEMU).
    crate::sbi::shutdown(crate::sbi::ResetType::Shutdown)
}

/// Exit QEMU with status 0.
pub fn exit_success() -> ! {
    finish(FINISH_PASS)
}

/// Exit QEMU with a non-zero status.
pub fn exit_failure() -> ! {
    finish(FINISH_FAIL | (1 << 16))
}
