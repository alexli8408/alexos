//! Raw RISC-V supervisor-mode machinery: control/status registers, the
//! interrupt-enable dance, and the handful of instructions that have no
//! portable spelling in Rust.

use core::arch::asm;

/// Generate typed accessors for a CSR.
macro_rules! csr {
    ($name:ident, $csr:literal) => {
        #[doc = concat!("Accessors for the `", $csr, "` control/status register.")]
        pub mod $name {
            use core::arch::asm;

            /// Read the current value of the CSR.
            #[inline(always)]
            pub fn read() -> usize {
                let v: usize;
                // SAFETY: reading a supervisor CSR from S-mode has no side effects.
                unsafe { asm!(concat!("csrr {0}, ", $csr), out(reg) v, options(nomem, nostack)) };
                v
            }

            /// Overwrite the CSR.
            ///
            /// # Safety
            /// The caller must uphold whatever invariant the register controls;
            /// most of these registers can trivially wedge the machine.
            #[inline(always)]
            pub unsafe fn write(v: usize) {
                unsafe {
                    asm!(concat!("csrw ", $csr, ", {0}"), in(reg) v, options(nomem, nostack))
                };
            }

            /// Atomically set every bit present in `mask`.
            ///
            /// # Safety
            /// See [`write`].
            #[inline(always)]
            pub unsafe fn set(mask: usize) {
                unsafe {
                    asm!(concat!("csrs ", $csr, ", {0}"), in(reg) mask, options(nomem, nostack))
                };
            }

            /// Atomically clear every bit present in `mask`.
            ///
            /// # Safety
            /// See [`write`].
            #[inline(always)]
            pub unsafe fn clear(mask: usize) {
                unsafe {
                    asm!(concat!("csrc ", $csr, ", {0}"), in(reg) mask, options(nomem, nostack))
                };
            }
        }
    };
}

csr!(sstatus, "sstatus"); // supervisor status: interrupt enables, previous mode
csr!(sie, "sie"); // per-source interrupt enables
csr!(sip, "sip"); // pending interrupts
csr!(stvec, "stvec"); // trap vector base
csr!(sepc, "sepc"); // PC to resume at after a trap
csr!(scause, "scause"); // why we trapped
csr!(stval, "stval"); // faulting address / instruction
csr!(sscratch, "sscratch"); // scratch word, holds the per-hart TrapFrame pointer
csr!(satp, "satp"); // address translation root

/// `sstatus` bit positions the kernel cares about.
pub mod sstatus_bits {
    /// Supervisor Interrupt Enable.
    pub const SIE: usize = 1 << 1;
    /// Interrupt enable state before the trap.
    pub const SPIE: usize = 1 << 5;
    /// Privilege level before the trap: 0 = user, 1 = supervisor.
    pub const SPP: usize = 1 << 8;
    /// Permit supervisor loads/stores to user pages.
    pub const SUM: usize = 1 << 18;
    /// Make supervisor loads from execute-only pages legal.
    pub const MXR: usize = 1 << 19;
}

/// `sie`/`sip` bit positions.
pub mod int_bits {
    /// Supervisor software interrupt (IPI).
    pub const SSIE: usize = 1 << 1;
    /// Supervisor timer interrupt.
    pub const STIE: usize = 1 << 5;
    /// Supervisor external interrupt (PLIC).
    pub const SEIE: usize = 1 << 9;
}

/// Id of the hart executing this code.
///
/// `tp` is loaded once in entry.S and never touched again, so this compiles to
/// a single register move -- cheap enough to call from a spin lock.
#[inline(always)]
pub fn hart_id() -> usize {
    let id: usize;
    // SAFETY: reading tp is always sound; the kernel reserves it for the hart id.
    unsafe { asm!("mv {0}, tp", out(reg) id, options(nomem, nostack)) };
    id
}

/// Are supervisor interrupts currently enabled on this hart?
#[inline(always)]
pub fn intr_enabled() -> bool {
    sstatus::read() & sstatus_bits::SIE != 0
}

/// Unmask supervisor interrupts.
///
/// # Safety
/// Enabling interrupts inside a critical section can deadlock against a lock
/// the interrupt handler takes. Prefer [`without_interrupts`].
#[inline(always)]
pub unsafe fn intr_enable() {
    unsafe { sstatus::set(sstatus_bits::SIE) };
}

/// Mask supervisor interrupts.
///
/// # Safety
/// Callers must re-enable eventually or the hart stops responding to timers.
#[inline(always)]
pub unsafe fn intr_disable() {
    unsafe { sstatus::clear(sstatus_bits::SIE) };
}

/// Run `f` with interrupts masked, restoring the previous state afterwards.
///
/// Nesting is safe: an inner call observes SIE already clear and leaves it that
/// way on exit, so only the outermost guard re-enables.
#[inline]
pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let was_enabled = intr_enabled();
    if was_enabled {
        // SAFETY: restored below on every path (`f` cannot unwind: panic = abort).
        unsafe { intr_disable() };
    }
    let result = f();
    if was_enabled {
        // SAFETY: we are merely restoring the caller's own interrupt state.
        unsafe { intr_enable() };
    }
    result
}

/// Park the hart until the next interrupt arrives.
#[inline(always)]
pub fn wait_for_interrupt() {
    // SAFETY: `wfi` is a hint; the worst case is that it returns immediately.
    unsafe { asm!("wfi", options(nomem, nostack)) };
}

/// Invalidate the entire TLB for the current address space.
#[inline(always)]
pub fn sfence_vma_all() {
    // SAFETY: a full fence is always semantically safe -- it only costs misses.
    unsafe { asm!("sfence.vma zero, zero", options(nostack)) };
}

/// Invalidate the TLB entry covering a single virtual address.
#[inline(always)]
pub fn sfence_vma_addr(vaddr: usize) {
    // SAFETY: as above; a narrower fence is a pure optimisation.
    unsafe { asm!("sfence.vma {0}, zero", in(reg) vaddr, options(nostack)) };
}

/// Cycle counter shared by all harts, driven at a fixed frequency by the CLINT.
#[inline(always)]
pub fn read_time() -> u64 {
    let t: usize;
    // SAFETY: `time` is a read-only user-accessible counter.
    unsafe { asm!("rdtime {0}", out(reg) t, options(nomem, nostack)) };
    t as u64
}
