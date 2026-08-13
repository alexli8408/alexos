//! AlexOS -- a preemptive multitasking kernel for RISC-V 64, written from
//! scratch in stable Rust.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

use core::arch::asm;
use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("entry.S"));

pub mod config;
pub mod mm;
pub mod sbi;

/// Rust entry point, called from `entry.S` once Sv39 is live, the kernel is
/// executing out of the high half, and `.bss` has been cleared.
///
/// `hart_id` is the boot hart; `dtb` is the *physical* address of the device
/// tree QEMU handed to OpenSBI.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(hart_id: usize, dtb: usize) -> ! {
    let _ = (hart_id, dtb);
    sbi::console_write(b"AlexOS: reached kmain in the high half\n");
    sbi::shutdown(sbi::ResetType::Shutdown)
}

/// Park the hart forever.
fn halt() -> ! {
    loop {
        // SAFETY: `wfi` is a hint instruction, always legal in S-mode.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    halt()
}
