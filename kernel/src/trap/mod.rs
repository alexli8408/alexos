//! Trap handling: the single door through which every interrupt and exception
//! enters the kernel.
//!
//! RISC-V funnels everything through `stvec` and reports the reason in
//! `scause`, whose top bit separates interrupts from exceptions. The kernel
//! runs in Direct mode -- one entry point that dispatches in software -- rather
//! than Vectored, because the dispatch is a handful of instructions and one
//! entry point is far easier to reason about.
//!
//! Exceptions taken in supervisor mode are, with one exception, bugs: the
//! kernel does not fault on its own memory by design. So the handler prints
//! the full machine state and panics rather than trying to recover. The
//! exception is a page fault on a lazily-backed or copy-on-write page, which
//! is a normal event and handled in `mm`.

pub mod context;

use core::arch::global_asm;

use crate::arch::{self, int_bits, sstatus_bits};
use crate::timer;

pub use context::TrapFrame;

global_asm!(include_str!("trap.S"));

unsafe extern "C" {
    /// Assembly trap entry; installed in `stvec`.
    fn kernel_trap_entry();
}

/// `scause` values, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Another hart sent an IPI.
    SoftwareInterrupt,
    /// The timer fired: a scheduling quantum expired.
    TimerInterrupt,
    /// A device raised an interrupt through the PLIC.
    ExternalInterrupt,
    /// A `ecall` from user mode: a system call.
    UserEcall,
    /// Instruction fetch, load, or store hit an invalid or forbidden mapping.
    PageFault(FaultKind),
    /// The instruction is not legal at this privilege level.
    IllegalInstruction,
    /// `ebreak`.
    Breakpoint,
    /// Misaligned or out-of-range access that the MMU rejected outright.
    AccessFault(FaultKind),
    /// Anything the kernel does not decode.
    Unknown(usize),
}

/// Which kind of access caused a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// Instruction fetch.
    Fetch,
    /// Load.
    Load,
    /// Store or AMO.
    Store,
}

impl Cause {
    /// Decode `scause`.
    ///
    /// The top bit distinguishes interrupts from exceptions; the remaining bits
    /// are a small dense code whose meaning differs between the two.
    pub fn decode(scause: usize) -> Self {
        let is_interrupt = scause >> (usize::BITS - 1) != 0;
        let code = scause & !(1 << (usize::BITS - 1));

        if is_interrupt {
            match code {
                1 => Self::SoftwareInterrupt,
                5 => Self::TimerInterrupt,
                9 => Self::ExternalInterrupt,
                _ => Self::Unknown(scause),
            }
        } else {
            match code {
                2 => Self::IllegalInstruction,
                3 => Self::Breakpoint,
                5 => Self::AccessFault(FaultKind::Load),
                7 => Self::AccessFault(FaultKind::Store),
                8 => Self::UserEcall,
                12 => Self::PageFault(FaultKind::Fetch),
                13 => Self::PageFault(FaultKind::Load),
                15 => Self::PageFault(FaultKind::Store),
                _ => Self::Unknown(scause),
            }
        }
    }
}

/// Point `stvec` at the assembly entry and verify the frame layout.
///
/// Direct mode is selected by the low two bits of `stvec` being zero, which is
/// why the entry point is 4-byte aligned in the assembly.
pub fn init() {
    context::assert_layout();
    // SAFETY: `kernel_trap_entry` is 4-byte aligned, so the low bits that
    // select Direct mode are clear, and it never returns to its caller.
    unsafe { arch::stvec::write(kernel_trap_entry as *const () as usize) };
}

/// Unmask the interrupt sources the kernel handles.
pub fn enable_interrupts() {
    // SAFETY: every source enabled here has a handler in `dispatch`.
    unsafe {
        arch::sie::set(int_bits::STIE | int_bits::SEIE | int_bits::SSIE);
        arch::intr_enable();
    }
}

/// Rust trap handler, called from `trap.S` with the saved frame.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_trap_handler(frame: &mut TrapFrame) {
    let scause = arch::scause::read();
    let stval = arch::stval::read();
    let cause = Cause::decode(scause);

    // A supervisor trap must not have arrived with interrupts enabled -- the
    // hardware clears SIE on entry. If it did, something re-enabled them
    // between the trap and here, and nested traps would corrupt the frame.
    debug_assert!(!arch::intr_enabled(), "interrupts enabled inside the trap handler");

    match cause {
        Cause::TimerInterrupt => {
            timer::on_tick();
        }
        Cause::ExternalInterrupt => {
            crate::drivers::plic::handle_external_interrupt();
        }
        Cause::SoftwareInterrupt => {
            // Clear the pending bit; leaving it set would re-trap immediately.
            // SAFETY: acknowledging our own IPI.
            unsafe { arch::sip::clear(int_bits::SSIE) };
        }
        _ => fatal(frame, cause, stval),
    }

    // Act on an expired quantum here rather than inside the timer handler. By
    // now the trap frame is fully built on this task's kernel stack, so
    // switching away and coming back later resumes exactly where we left off.
    crate::task::scheduler::resched_if_needed();
}

/// Report an unrecoverable supervisor trap and stop.
fn fatal(frame: &TrapFrame, cause: Cause, stval: usize) -> ! {
    crate::error!("fatal trap in supervisor mode: {cause:?}");
    crate::error!("  sepc  {:#018x}", frame.sepc);
    crate::error!("  stval {stval:#018x}");
    if matches!(cause, Cause::PageFault(_) | Cause::AccessFault(_)) {
        crate::error!("  faulting address {stval:#018x}");
        describe_address(stval);
    }
    crate::println!("{frame:?}");
    panic!("unhandled supervisor trap: {cause:?}");
}

/// Say something useful about where a faulting address landed. A kernel bug is
/// much easier to place when the message distinguishes "wrote to .text" from
/// "dereferenced null".
fn describe_address(addr: usize) {
    use crate::config::{USER_MAX_ADDR, VIRT_OFFSET};

    let note = if addr < crate::config::PAGE_SIZE {
        "null dereference"
    } else if addr < USER_MAX_ADDR {
        "user address touched from the kernel without a checked copy"
    } else if addr < VIRT_OFFSET {
        "non-canonical address: bits 63:39 do not replicate bit 38"
    } else {
        "kernel address, but the mapping does not permit this access"
    };
    crate::error!("  -> {note}");
}

/// Is the given `sstatus` value from a trap that interrupted user mode?
pub fn was_user(sstatus: usize) -> bool {
    sstatus & sstatus_bits::SPP == 0
}
