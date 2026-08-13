//! The register state saved across a trap.

use core::fmt;

use crate::arch::sstatus_bits;

/// Every register a trap has to preserve.
///
/// The field order is load-bearing: `trap.S` indexes this struct by hand, with
/// `x[i]` at byte offset `i * 8`, `sstatus` at 256 and `sepc` at 264. Changing
/// the layout without changing the assembly produces a kernel that corrupts
/// registers at random, so the two are checked against each other by
/// `assert_layout` at boot.
///
/// `x[0]` is the zero register and is never restored; the slot exists so that
/// register *n* lives at offset *n*, which keeps the assembly readable.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct TrapFrame {
    /// General purpose registers x0 through x31.
    pub x: [usize; 32],
    /// Supervisor status at the moment of the trap. Carries SPP (the privilege
    /// level to return to) and SPIE (the interrupt state to restore).
    pub sstatus: usize,
    /// Address to resume at. For an `ecall` the kernel advances this by 4, or
    /// the trap re-executes the `ecall` forever.
    pub sepc: usize,
    /// Kernel stack pointer for this task, reloaded by `uservec` on entry from
    /// user mode.
    pub kernel_sp: usize,
    /// `satp` of the kernel address space, so the user trap path can switch
    /// out of the user space it arrived in.
    pub kernel_satp: usize,
    /// Where `uservec` jumps after saving state.
    pub kernel_trap: usize,
    /// Hart id, reloaded into `tp` on kernel entry.
    pub kernel_hartid: usize,
}

/// Convenient names for the registers the kernel reads by hand.
impl TrapFrame {
    /// Return address, `x1`.
    pub fn ra(&self) -> usize {
        self.x[1]
    }

    /// Stack pointer, `x2`.
    pub fn sp(&self) -> usize {
        self.x[2]
    }

    /// Set the stack pointer.
    pub fn set_sp(&mut self, sp: usize) {
        self.x[2] = sp;
    }

    /// Syscall argument *n*, from `a0`..`a5` (`x10`..`x15`).
    pub fn arg(&self, n: usize) -> usize {
        debug_assert!(n < 6);
        self.x[10 + n]
    }

    /// Syscall number, from `a7` (`x17`).
    pub fn syscall_id(&self) -> usize {
        self.x[17]
    }

    /// Set the syscall return value in `a0`.
    pub fn set_return(&mut self, value: usize) {
        self.x[10] = value;
    }

    /// Did this trap come from user mode?
    ///
    /// SPP records the privilege level that was interrupted: 0 for user, 1 for
    /// supervisor.
    pub fn from_user(&self) -> bool {
        self.sstatus & sstatus_bits::SPP == 0
    }

    /// Build the frame a brand-new user task starts from.
    ///
    /// SPP is cleared so `sret` drops to user mode, and SPIE is set so that the
    /// same `sret` enables interrupts -- otherwise the task runs with them
    /// masked and can never be preempted.
    pub fn new_user(entry: usize, user_sp: usize, kernel_sp: usize, kernel_satp: usize) -> Self {
        let mut sstatus = crate::arch::sstatus::read();
        sstatus &= !sstatus_bits::SPP;
        sstatus |= sstatus_bits::SPIE;
        // Clear SIE: interrupts stay masked for the handful of instructions
        // between loading this frame and the sret that installs SPIE.
        sstatus &= !sstatus_bits::SIE;

        let mut frame = Self { sstatus, sepc: entry, kernel_sp, kernel_satp, ..Self::default() };
        frame.set_sp(user_sp);
        frame
    }
}

impl fmt::Debug for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const NAMES: [&str; 32] = [
            "zero", "ra", "sp", "gp", "tp", "t0", "t1", "t2", "s0", "s1", "a0", "a1", "a2", "a3",
            "a4", "a5", "a6", "a7", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
            "t3", "t4", "t5", "t6",
        ];
        writeln!(f, "TrapFrame {{")?;
        writeln!(f, "  sepc    {:#018x}", self.sepc)?;
        writeln!(f, "  sstatus {:#018x}", self.sstatus)?;
        // Four registers per line keeps a dump inside an 80-column terminal.
        for chunk in (1..32).collect::<alloc::vec::Vec<_>>().chunks(4) {
            write!(f, " ")?;
            for &i in chunk {
                write!(f, " {:>4} {:#018x}", NAMES[i], self.x[i])?;
            }
            writeln!(f)?;
        }
        write!(f, "}}")
    }
}

/// Byte offsets `trap.S` hard-codes. Verified at boot so a struct change that
/// forgets the assembly fails loudly instead of silently corrupting registers.
pub fn assert_layout() {
    use core::mem::offset_of;
    assert_eq!(offset_of!(TrapFrame, x), 0);
    assert_eq!(offset_of!(TrapFrame, sstatus), 32 * 8);
    assert_eq!(offset_of!(TrapFrame, sepc), 33 * 8);
    assert_eq!(offset_of!(TrapFrame, kernel_sp), 34 * 8);
    assert_eq!(offset_of!(TrapFrame, kernel_satp), 35 * 8);
    assert_eq!(offset_of!(TrapFrame, kernel_trap), 36 * 8);
    assert_eq!(offset_of!(TrapFrame, kernel_hartid), 37 * 8);
    assert_eq!(core::mem::size_of::<TrapFrame>(), 38 * 8);
}
