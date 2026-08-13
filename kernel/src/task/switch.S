# Context switch.
#
#   switch_context(old: *mut TaskContext, new: *const TaskContext)
#
# Only the callee-saved registers need saving. Everything else -- argument
# registers, temporaries -- is already dead by the C ABI's rules at a function
# call boundary, and this *is* a function call from the compiler's point of
# view. That is the whole trick: a context switch costs 14 stores and 14 loads,
# not the 34 a trap frame needs.
#
# `ra` is saved and restored as the resume point. A task that has never run has
# a hand-built context whose ra points at task_entry, so the first `ret` after
# loading it lands there instead of returning to a caller that never existed.
#
# Offsets match TaskContext in context.rs: ra at 0, sp at 8, s0..s11 at 16..104.

    .section .text
    .align 2
    .globl switch_context
switch_context:
    # --- save the outgoing task ---
    sd      ra,  0(a0)
    sd      sp,  8(a0)
    sd      s0,  16(a0)
    sd      s1,  24(a0)
    sd      s2,  32(a0)
    sd      s3,  40(a0)
    sd      s4,  48(a0)
    sd      s5,  56(a0)
    sd      s6,  64(a0)
    sd      s7,  72(a0)
    sd      s8,  80(a0)
    sd      s9,  88(a0)
    sd      s10, 96(a0)
    sd      s11, 104(a0)

    # --- load the incoming task ---
    ld      ra,  0(a1)
    ld      sp,  8(a1)
    ld      s0,  16(a1)
    ld      s1,  24(a1)
    ld      s2,  32(a1)
    ld      s3,  40(a1)
    ld      s4,  48(a1)
    ld      s5,  56(a1)
    ld      s6,  64(a1)
    ld      s7,  72(a1)
    ld      s8,  80(a1)
    ld      s9,  88(a1)
    ld      s10, 96(a1)
    ld      s11, 104(a1)

    # Returns into the *incoming* task's ra, not the caller's.
    ret
