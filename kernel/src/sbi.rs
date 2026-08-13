//! Supervisor Binary Interface bindings.
//!
//! Everything the kernel cannot do from S-mode -- setting the machine timer,
//! sending IPIs, starting secondary harts, powering the board off -- goes
//! through OpenSBI via `ecall`. The calling convention is: extension id in a7,
//! function id in a6, arguments in a0..a5, and a two-register return of
//! `(error, value)` in a0/a1.

use core::arch::asm;

// Modern (SBI >= 0.2) extension ids, spelled as their ASCII names.
const EID_TIME: usize = 0x5449_4D45; // "TIME"
const EID_IPI: usize = 0x0073_5049; // "sPI"
const EID_RFNC: usize = 0x5246_4E43; // "RFNC"
const EID_HSM: usize = 0x0048_534D; // "HSM"
const EID_SRST: usize = 0x5352_5354; // "SRST"
const EID_DBCN: usize = 0x4442_434E; // "DBCN"

// Legacy (SBI 0.1) extension ids. Deprecated by the spec but universally
// implemented, and the only console available before the UART driver binds.
const EID_LEGACY_PUTCHAR: usize = 0x01;
const EID_LEGACY_GETCHAR: usize = 0x02;

/// Return value of an SBI call: a negative error code plus a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbiRet {
    /// `0` on success, a negative SBI error code otherwise.
    pub error: isize,
    /// Call-specific payload, meaningful only when `error == 0`.
    pub value: usize,
}

impl SbiRet {
    /// `true` when the firmware reported success (`SBI_SUCCESS == 0`).
    pub fn is_ok(self) -> bool {
        self.error == 0
    }
}

#[inline(always)]
fn ecall(eid: usize, fid: usize, arg0: usize, arg1: usize, arg2: usize) -> SbiRet {
    let (error, value);
    // SAFETY: `ecall` traps to firmware, which per the SBI spec preserves every
    // register except a0/a1. The clobbers below are the ones it may write.
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") arg0 => error,
            inlateout("a1") arg1 => value,
            in("a2") arg2,
            in("a6") fid,
            in("a7") eid,
            options(nostack),
        );
    }
    SbiRet { error, value }
}

/// Program the next supervisor timer interrupt for absolute `mtime` value
/// `stime`. Writing a time in the past fires immediately; there is no way to
/// cancel other than scheduling one far in the future.
pub fn set_timer(stime: u64) -> SbiRet {
    ecall(EID_TIME, 0, stime as usize, 0, 0)
}

/// Post a software interrupt to every hart selected by `hart_mask`, which is a
/// bitmap whose bit *i* refers to hart `hart_mask_base + i`.
pub fn send_ipi(hart_mask: usize, hart_mask_base: usize) -> SbiRet {
    ecall(EID_IPI, 0, hart_mask, hart_mask_base, 0)
}

/// Broadcast a remote `sfence.vma` so other harts drop stale TLB entries after
/// this hart edits a shared page table.
pub fn remote_sfence_vma(hart_mask: usize, hart_mask_base: usize) -> SbiRet {
    ecall(EID_RFNC, 1, hart_mask, hart_mask_base, 0)
}

/// Boot `hartid` at physical address `start_addr` with `opaque` left in a1.
/// Used by `smp::start_others` to release the harts parked in entry.S.
pub fn hart_start(hartid: usize, start_addr: usize, opaque: usize) -> SbiRet {
    ecall(EID_HSM, 0, hartid, start_addr, opaque)
}

/// Query the HSM state of `hartid` (0 = started, 1 = stopped, ...).
pub fn hart_status(hartid: usize) -> SbiRet {
    ecall(EID_HSM, 2, hartid, 0, 0)
}

/// Write one byte to the firmware console. This is the slow path -- one trap
/// per character -- but it works from the first instruction of the kernel and
/// keeps working when the UART driver has faulted, which makes it the right
/// primitive for panics.
pub fn console_putchar(c: u8) {
    ecall(EID_LEGACY_PUTCHAR, 0, c as usize, 0, 0);
}

/// Read one byte from the firmware console, or `None` if nothing is buffered.
pub fn console_getchar() -> Option<u8> {
    let ret = ecall(EID_LEGACY_GETCHAR, 0, 0, 0, 0);
    // The legacy call returns the character in a0 and -1 when the queue is dry.
    if ret.error == -1 { None } else { Some(ret.error as u8) }
}

/// Write `bytes` through the SBI debug console extension in one call. Falls
/// back to the byte-at-a-time legacy path on firmware that predates DBCN.
pub fn console_write(bytes: &[u8]) {
    let phys = crate::mm::virt_to_phys(bytes.as_ptr() as usize);
    let ret = ecall(EID_DBCN, 0, bytes.len(), phys, 0);
    if !ret.is_ok() {
        for &b in bytes {
            console_putchar(b);
        }
    }
}

/// Reset type argument to `system_reset`.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum ResetType {
    /// Power off.
    Shutdown = 0,
    /// Full power cycle.
    ColdReboot = 1,
    /// Restart without cycling power.
    WarmReboot = 2,
}

/// Power the board down. Returns only if the firmware refuses the request, in
/// which case the caller is expected to spin.
pub fn shutdown(reason: ResetType) -> ! {
    ecall(EID_SRST, 0, reason as usize, 0, 0);
    // SRST is optional; fall back to the legacy shutdown call, then wedge.
    ecall(0x08, 0, 0, 0, 0);
    loop {
        // SAFETY: `wfi` is a hint instruction, always legal in S-mode.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}
