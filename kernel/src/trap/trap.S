# Trap entry and exit.
#
# Register offsets match `TrapFrame` in context.rs, which asserts them at boot:
# x[i] at i*8, sstatus at 256, sepc at 264. The frame is 304 bytes; only the
# first 34 slots are touched here, the rest belong to the user-mode path.
#
# Registers are saved before anything else runs, so t0/t1 can be reused freely
# afterwards -- their originals are already in the frame. On the way out the
# CSRs are restored first, while t0/t1 are still scratch, and the general
# registers last.

.equ FRAME_SIZE, 304

    .section .text
    .align 2
    .globl kernel_trap_entry
kernel_trap_entry:
    addi    sp, sp, -FRAME_SIZE

    sd      x1,  1*8(sp)
    # x2 is sp itself: store the value it had before the frame was pushed.
    addi    t0, sp, FRAME_SIZE
    sd      t0,  2*8(sp)
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

    csrr    t0, sstatus
    sd      t0, 32*8(sp)
    csrr    t1, sepc
    sd      t1, 33*8(sp)

    # The handler takes &mut TrapFrame, which is the frame we just built.
    mv      a0, sp
    call    kernel_trap_handler

kernel_trap_exit:
    ld      t0, 32*8(sp)
    csrw    sstatus, t0
    ld      t1, 33*8(sp)
    csrw    sepc, t1

    ld      x1,  1*8(sp)
    ld      x3,  3*8(sp)
    ld      x4,  4*8(sp)
    ld      x5,  5*8(sp)
    ld      x6,  6*8(sp)
    ld      x7,  7*8(sp)
    ld      x8,  8*8(sp)
    ld      x9,  9*8(sp)
    ld      x10, 10*8(sp)
    ld      x11, 11*8(sp)
    ld      x12, 12*8(sp)
    ld      x13, 13*8(sp)
    ld      x14, 14*8(sp)
    ld      x15, 15*8(sp)
    ld      x16, 16*8(sp)
    ld      x17, 17*8(sp)
    ld      x18, 18*8(sp)
    ld      x19, 19*8(sp)
    ld      x20, 20*8(sp)
    ld      x21, 21*8(sp)
    ld      x22, 22*8(sp)
    ld      x23, 23*8(sp)
    ld      x24, 24*8(sp)
    ld      x25, 25*8(sp)
    ld      x26, 26*8(sp)
    ld      x27, 27*8(sp)
    ld      x28, 28*8(sp)
    ld      x29, 29*8(sp)
    ld      x30, 30*8(sp)
    ld      x31, 31*8(sp)

    # Pop the frame rather than reloading x2: sp has been the base pointer for
    # every load above, so it has to be restored last.
    addi    sp, sp, FRAME_SIZE
    sret
