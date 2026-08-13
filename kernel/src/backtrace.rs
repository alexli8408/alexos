//! Frame-pointer backtraces.
//!
//! The kernel is built with `-C force-frame-pointers=yes`, so every frame keeps
//! the standard RISC-V layout: `s0`/`fp` points just past the frame, with the
//! return address at `fp - 8` and the caller's frame pointer at `fp - 16`.
//! Walking that chain gives a stack trace without needing DWARF unwinding
//! tables in the kernel image.
//!
//! Addresses are printed raw. `make sym ADDR=0x...` resolves them against the
//! ELF, which keeps symbol tables out of the kernel's memory footprint.

use core::arch::asm;

use crate::config::VIRT_OFFSET;

/// Frames to print before giving up, in case the chain is circular.
const MAX_FRAMES: usize = 32;

/// Print a backtrace starting from the caller.
pub fn print() {
    let mut fp: usize;
    // SAFETY: reading the frame pointer has no side effects.
    unsafe { asm!("mv {0}, s0", out(reg) fp, options(nomem, nostack)) };

    println!("backtrace:");
    let mut frame = 0;
    while frame < MAX_FRAMES {
        // A frame pointer that is not in the kernel half, is misaligned, or has
        // not moved upward means the chain has run off the end of the stack.
        if fp < VIRT_OFFSET || fp & 0x7 != 0 {
            break;
        }

        // SAFETY: `fp` has been range- and alignment-checked, and the kernel
        // stack it points into is mapped. A wild pointer that passes both
        // checks would fault, which the trap handler reports rather than
        // silently corrupting anything.
        let (ra, prev_fp) = unsafe {
            let ra = ((fp - 8) as *const usize).read_volatile();
            let prev_fp = ((fp - 16) as *const usize).read_volatile();
            (ra, prev_fp)
        };

        if ra == 0 {
            break;
        }
        println!("  #{frame:<2} ra={ra:#018x} fp={fp:#018x}");

        // Frame pointers march upward; anything else is a corrupt chain.
        if prev_fp <= fp {
            break;
        }
        fp = prev_fp;
        frame += 1;
    }
    if frame == MAX_FRAMES {
        println!("  ... truncated at {MAX_FRAMES} frames");
    }
}
