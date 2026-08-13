# AlexOS boot trampoline.
#
# OpenSBI drops us here in S-mode, MMU off, with:
#   a0 = hart id
#   a1 = physical address of the device tree blob
#
# The image is linked for 0xffffffc080200000 but loaded at 0x80200000, so until
# satp is live every symbol reference has to be PC-relative. `lla` is the only
# pseudo-instruction guaranteed to expand to auipc+addi, so it is used
# exclusively here: at boot it yields a *physical* address (pc is physical and
# the link-time delta between two symbols is offset-invariant), and after the
# jump to the high half the very same instruction yields a virtual one.

.equ VIRT_OFFSET,     0xffffffc000000000
.equ BOOT_STACK_SIZE, 65536
.equ MAX_HARTS,       4

    .section .text.entry
    .globl _entry
_entry:
    # tp holds the hart id for the entire lifetime of the hart. Kernel code
    # reads it via `cpu::id()` instead of paying for an SBI call.
    mv      tp, a0

    # Per-hart boot stack: sp = __boot_stack_top - hartid * BOOT_STACK_SIZE.
    # Secondary harts park below, but they need a stack to run `wfi` from once
    # smp::start_others() wakes them through the SBI HSM extension.
    lla     sp, __boot_stack_top
    li      t0, BOOT_STACK_SIZE
    mul     t1, a0, t0
    sub     sp, sp, t1

    # Only hart 0 brings the machine up; the rest spin until told otherwise.
    bnez    a0, secondary_park

    # ---- Turn on Sv39 using the statically initialised boot table ----------
    # boot_page_table maps two windows with 1 GiB leaf entries:
    #   identity  0x00000000+2G  -- keeps the next few instructions fetchable
    #   high half VIRT_OFFSET+2G -- where the kernel is actually linked
    lla     t0, boot_page_table
    srli    t0, t0, 12                  # PPN of the root table
    li      t1, 8 << 60                 # satp.MODE = 8 (Sv39)
    or      t0, t0, t1
    sfence.vma
    csrw    satp, t0
    sfence.vma zero, zero

    # Still executing out of the identity window. Relocate sp and pc upward.
    li      t1, VIRT_OFFSET
    add     sp, sp, t1
    lla     t0, _entry_high
    add     t0, t0, t1
    jr      t0

_entry_high:
    # From here on `lla` resolves to true virtual addresses.
    # Clear .bss before any Rust static is touched. The section is 4 KiB
    # aligned at both ends, so an 8-byte stride can never overshoot.
    lla     t0, __bss_start
    lla     t1, __bss_end
    bgeu    t0, t1, 2f
1:  sd      zero, (t0)
    addi    t0, t0, 8
    bltu    t0, t1, 1b
2:
    # Drop the frame pointer so backtraces terminate cleanly at the boot frame.
    mv      fp, zero
    mv      ra, zero

    mv      a0, tp                      # a0 = hart id
                                        # a1 = dtb pointer, still untouched
    call    kmain

    # kmain is `-> !`; if it ever returns, fall through to a halt.
secondary_park:
    wfi
    j       secondary_park

# ---------------------------------------------------------------------------
# Boot page table: one Sv39 root, 512 entries, 1 GiB leaves.
#
# A leaf PTE is (PPN << 10) | flags, where the flag bits are
#   V=1 R=2 W=4 X=8 U=16 G=32 A=64 D=128.
# Device memory gets V|R|W|G|A|D          = 0xe7 (no execute)
# Normal memory gets V|R|W|X|G|A|D        = 0xef
# and the PPN of a 1 GiB frame at P is P >> 12.
# ---------------------------------------------------------------------------
    .section .data
    .align 12
    .globl boot_page_table
boot_page_table:
    .quad 0x00000000000000e7            # [  0] 0x00000000 : MMIO (UART, PLIC, virtio)
    .quad 0x0000000000000000            # [  1]
    .quad 0x00000000200000ef            # [  2] 0x80000000 : DRAM, where we are running
    .quad 0x00000000300000ef            # [  3] 0xc0000000 : DRAM continued
    .zero 8 * 252                       # [  4 .. 255]
    .quad 0x00000000000000e7            # [256] high-half mirror of MMIO
    .quad 0x0000000000000000            # [257]
    .quad 0x00000000200000ef            # [258] high-half mirror of DRAM
    .quad 0x00000000300000ef            # [259] high-half mirror of DRAM
    .zero 8 * 252                       # [260 .. 511]

    .section .bss.stack
    .align 12
    .globl __boot_stack
__boot_stack:
    .space BOOT_STACK_SIZE * MAX_HARTS
    .globl __boot_stack_top
__boot_stack_top:
