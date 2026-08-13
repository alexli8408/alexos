//! Platform-Level Interrupt Controller.
//!
//! The PLIC multiplexes every device interrupt on the board onto the single
//! external-interrupt line each hart has. Its model is:
//!
//! * every source has a priority (0 disables it);
//! * every *context* -- a (hart, privilege level) pair -- has an enable bitmap
//!   and a threshold, and only sees sources above that threshold;
//! * a handler **claims** an interrupt, which returns the highest-priority
//!   pending source and atomically clears its pending bit, and must later
//!   **complete** it by writing the same id back.
//!
//! Forgetting the completion write is the classic PLIC bug: the source is never
//! re-armed, and the device goes silent forever with no other symptom.
//!
//! Context numbering on the `virt` board is `2 * hart + 1` for supervisor mode
//! (`2 * hart` is machine mode, which belongs to OpenSBI).

use crate::config::{PLIC_BASE, UART_IRQ};
use crate::mm::phys_to_virt;

/// Register offsets within the PLIC.
mod off {
    /// Per-source priority words, indexed by source id.
    pub const PRIORITY: usize = 0x0000;
    /// Per-context enable bitmaps.
    pub const ENABLE: usize = 0x2000;
    /// Bytes between one context's enable bitmap and the next.
    pub const ENABLE_STRIDE: usize = 0x80;
    /// Per-context priority threshold.
    pub const THRESHOLD: usize = 0x20_0000;
    /// Per-context claim/complete register.
    pub const CLAIM: usize = 0x20_0004;
    /// Bytes between one context's threshold/claim pair and the next.
    pub const CONTEXT_STRIDE: usize = 0x1000;
}

/// Supervisor-mode context number for `hart`.
#[inline]
fn context(hart: usize) -> usize {
    2 * hart + 1
}

#[inline]
fn reg(offset: usize) -> *mut u32 {
    phys_to_virt(PLIC_BASE + offset) as *mut u32
}

/// Set the priority of `irq`. Zero means "never deliver".
fn set_priority(irq: u32, priority: u32) {
    // SAFETY: the PLIC window is mapped RW by the kernel address space, and
    // `irq` is bounded by the board's source count.
    unsafe { reg(off::PRIORITY + irq as usize * 4).write_volatile(priority) };
}

/// Allow `irq` to reach `hart`'s supervisor context.
///
/// Sources are added here as their drivers arrive; virtio joins in the block
/// device phase.
fn enable(hart: usize, irq: u32) {
    let word = off::ENABLE + context(hart) * off::ENABLE_STRIDE + (irq as usize / 32) * 4;
    // SAFETY: as above. Read-modify-write is safe here because bring-up runs on
    // one hart before secondaries start.
    unsafe {
        let p = reg(word);
        p.write_volatile(p.read_volatile() | (1 << (irq % 32)));
    }
}

/// Set the minimum priority `hart` will accept. Zero accepts everything.
fn set_threshold(hart: usize, threshold: u32) {
    // SAFETY: mapped device register.
    unsafe {
        reg(off::THRESHOLD + context(hart) * off::CONTEXT_STRIDE).write_volatile(threshold)
    };
}

/// Take the highest-priority pending interrupt, or `None` if there is none.
fn claim(hart: usize) -> Option<u32> {
    // SAFETY: mapped device register; the read has the side effect of clearing
    // the source's pending bit, which is exactly what a claim is.
    let irq = unsafe { reg(off::CLAIM + context(hart) * off::CONTEXT_STRIDE).read_volatile() };
    if irq == 0 { None } else { Some(irq) }
}

/// Tell the PLIC the interrupt has been serviced and may fire again.
fn complete(hart: usize, irq: u32) {
    // SAFETY: mapped device register; writing back a claimed id is the
    // completion protocol.
    unsafe {
        reg(off::CLAIM + context(hart) * off::CONTEXT_STRIDE).write_volatile(irq)
    };
}

/// Route the board's device interrupts to this hart.
pub fn init(hart: usize) {
    // Accept everything above priority 0. The kernel has no interrupt
    // prioritisation policy: all its sources are equally urgent, and a
    // threshold would only create ways to starve one.
    set_threshold(hart, 0);

    set_priority(UART_IRQ, 1);
    enable(hart, UART_IRQ);

    crate::info!("plic: hart {hart} context {} listening on uart", context(hart));
}

/// Service every pending external interrupt.
///
/// Loops rather than handling one, because the PLIC coalesces: two devices
/// raising while we are in here produce a single external interrupt, and
/// returning after the first would leave the second pending with nothing
/// scheduled to notice it.
pub fn handle_external_interrupt() {
    let hart = crate::arch::hart_id();

    while let Some(irq) = claim(hart) {
        match irq {
            UART_IRQ => {
                crate::drivers::uart::handle_interrupt();
            }
            other => {
                crate::warn!("plic: interrupt {other} has no handler");
            }
        }
        // Must happen even for the unhandled case, or that source wedges.
        complete(hart, irq);
    }
}
