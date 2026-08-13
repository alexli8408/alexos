# User trap entry and return.
#
# The kernel is mapped into the upper half of every address space, with the
# GLOBAL bit set, so a trap from user mode does *not* need to switch satp before
# it can run kernel code. That is what lets this path be twenty instructions
# instead of xv6's trampoline-page dance. The cost is that the kernel is visible
# (though not accessible) from a user page table, which is the pre-KPTI Linux
# tradeoff and the right one for a kernel with no untrusted tenants.
#
# The contract is carried in sscratch: while user code runs it holds the kernel
# virtual address of that task's TrapFrame. The very first instruction swaps it
# with sp, which simultaneously gets us a pointer to save state through and
# stashes the user stack pointer somewhere recoverable -- with no free register
# to work with, that swap is the only way in.

    .section .text
    .align 2
    .globl user_trap_entry
user_trap_entry:
    # sp -> TrapFrame, sscratch -> user sp
    csrrw   sp, sscratch, sp

    sd      x1,  1*8(sp)
    # x2 (sp) is in sscratch; recovered below once t0 is free.
    sd      x3,  3*8(sp)
    sd      x4,  4*8(sp)
    sd      x5,  5*8(sp)
    sd      x6,  6*8(sp)
    sd      x7,  7*8(sp)
    sd      x8,  8*8(sp)
    sd      x9,  9*8(sp)
    sd      x10, 10*8(sp)
    sd      x11, 11*8(sp)
    sd      x12, 12*8(sp)
    sd      x13, 13*8(sp)
    sd      x14, 14*8(sp)
    sd      x15, 15*8(sp)
    sd      x16, 16*8(sp)
    sd      x17, 17*8(sp)
    sd      x18, 18*8(sp)
    sd      x19, 19*8(sp)
    sd      x20, 20*8(sp)
    sd      x21, 21*8(sp)
    sd      x22, 22*8(sp)
    sd      x23, 23*8(sp)
    sd      x24, 24*8(sp)
    sd      x25, 25*8(sp)
    sd      x26, 26*8(sp)
    sd      x27, 27*8(sp)
    sd      x28, 28*8(sp)
    sd      x29, 29*8(sp)
    sd      x30, 30*8(sp)
    sd      x31, 31*8(sp)

    # Now that t0 is saved, use it to recover the user sp and the CSRs.
    csrr    t0, sscratch
    sd      t0,  2*8(sp)
    csrr    t0, sstatus
    sd      t0, 32*8(sp)
    csrr    t0, sepc
    sd      t0, 33*8(sp)

    # Hand the frame to the handler, then move onto the kernel stack. tp is
    # reloaded because user code may have scribbled on it and the kernel reads
    # it for the hart id.
    mv      a0, sp
    ld      tp, 37*8(sp)
    ld      sp, 34*8(sp)

    call    user_trap_handler
    # user_trap_handler returns the frame to resume in a0 and falls through.

    .globl user_trap_return
user_trap_return:
    # a0 -> TrapFrame to resume.
    # Park it in sscratch so the next trap from user mode can find it.
    csrw    sscratch, a0

    ld      t0, 32*8(a0)
    csrw    sstatus, t0
    ld      t0, 33*8(a0)
    csrw    sepc, t0

    ld      x1,  1*8(a0)
    ld      x2,  2*8(a0)
    ld      x3,  3*8(a0)
    ld      x4,  4*8(a0)
    ld      x5,  5*8(a0)
    ld      x6,  6*8(a0)
    ld      x7,  7*8(a0)
    ld      x8,  8*8(a0)
    ld      x9,  9*8(a0)
    ld      x11, 11*8(a0)
    ld      x12, 12*8(a0)
    ld      x13, 13*8(a0)
    ld      x14, 14*8(a0)
    ld      x15, 15*8(a0)
    ld      x16, 16*8(a0)
    ld      x17, 17*8(a0)
    ld      x18, 18*8(a0)
    ld      x19, 19*8(a0)
    ld      x20, 20*8(a0)
    ld      x21, 21*8(a0)
    ld      x22, 22*8(a0)
    ld      x23, 23*8(a0)
    ld      x24, 24*8(a0)
    ld      x25, 25*8(a0)
    ld      x26, 26*8(a0)
    ld      x27, 27*8(a0)
    ld      x28, 28*8(a0)
    ld      x29, 29*8(a0)
    ld      x30, 30*8(a0)
    ld      x31, 31*8(a0)
    # a0 last: it has been the base pointer for every load above.
    ld      x10, 10*8(a0)

    sret
