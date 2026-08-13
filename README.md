# AlexOS

A preemptive multitasking kernel for RISC-V 64, written from scratch in stable
Rust. No `std`, no external crates, no nightly features.

[![CI](https://github.com/alexli8408/alexos/actions/workflows/ci.yml/badge.svg)](https://github.com/alexli8408/alexos/actions/workflows/ci.yml)

It boots on QEMU's `virt` board, brings up Sv39 paging and a higher-half kernel,
schedules preemptively across a multi-level feedback queue, and drops to user
mode to run `init`, a shell, and programs the shell forks and execs.

```
   _    _           ___  ____
  / \  | | _____  __/ _ \/ ___|
 / _ \ | |/ _ \ \/ / | | \___ \
/ ___ \| |  __/>  <| |_| |___) |
\_/  \_\_|\___/_/\_\\___/|____/

  riscv64 · sv39 · stable rust
  boot hart 0 · dtb 0x87e00000

[I] frames: 31744 usable (124 MiB) from 0x802c2000 to 0x88000000
[I] heap: self-test passed, 0 B live / 32768 B reserved
[D] map .text    0xffffffc080200000..0xffffffc080214000 -R-X-G----
[D] map .rodata  0xffffffc080214000..0xffffffc08027f000 -R---G----
[D] map .data    0xffffffc08027f000..0xffffffc080281000 -RW--G----
[D] map .bss     0xffffffc080281000..0xffffffc0802c2000 -RW--G----
[I] kernel space active, 7 page-table frames, boot table retired
[I] plic: hart 0 context 1 listening on uart
[I] timer: 10 ms quantum (100 Hz)
[I] programs: 5 embedded (echo forktest hello init sh)
[I] init is pid 1
[I] boot complete -- scheduler taking over
init: starting

AlexOS shell. Type `help` for the command list.
$ forktest
forktest: parent is pid 3, sentinel 0xaa
  forked child 0 as pid 4
  child 0 (pid 4) set sentinel 0xbb
  reaped pid 4 with status 1
forktest: parent sentinel is still 0xaa
forktest: ok
$ ps
  PID   PPID  NAME        STATE      LVL  KSTACK
    1      0  init        Blocked      0  31904 B free
    2      1  sh          Running      0  31904 B free
$
```

## Running it

Needs a Rust toolchain and `qemu-system-riscv64`.

```sh
rustup target add riscv64gc-unknown-none-elf
rustup component add llvm-tools
cargo install cargo-binutils

make run     # boot into the shell (ctrl-a x to quit)
make test    # run the 31 in-kernel tests, exit non-zero on failure
make debug   # boot halted, waiting for gdb on :1234
make size    # section sizes and flat image size
```

`scripts/integration-test.sh` boots the kernel and drives the shell over the
serial port, checking the output — the parts a unit test structurally cannot
reach.

## What it does

**Boot.** OpenSBI hands off at `0x80200000`. `entry.S` turns on Sv39 with a
statically built page table, jumps to the high half at `0xffffffc080200000`,
clears `.bss`, and enters Rust.

**Physical memory.** A binary buddy allocator with intrusive free lists — the
list nodes live inside the free frames themselves, so there is no external
metadata — plus a bitmap that answers the one question merging asks and the
lists cannot: *is my buddy free at this order?* Serves contiguous runs up to
4 MiB.

**Virtual memory.** Sv39 page tables with 4 KiB pages and 2 MiB superpages. The
boot table maps everything RWX with 1 GiB leaves, which is fine for the dozen
instructions before Rust starts and unacceptable afterwards; the real kernel
space gives `.text` R-X, `.rodata` R--, data RW-, and MMIO RW- with no execute.
The linear map uses superpages, so the whole kernel address space costs 7
page-table frames instead of ~250.

**Kernel heap.** A segregated size-class allocator: nine power-of-two classes up
to 2 KiB with LIFO free lists threaded through the free blocks, refilled a frame
at a time from the buddy allocator. Alignment falls out for free, since a class
of size *s* only ever yields *s*-aligned addresses.

**Traps.** One entry point in Direct mode, dispatching in software. Timer
interrupts drive preemption; the PLIC multiplexes device interrupts and the
dispatcher loops until the claim register reads zero, because the PLIC coalesces.

**Scheduling.** Preemptive, with a three-level feedback queue: burn a full
quantum and you drop a level, block before it expires and you rise. Tasks switch
through a per-hart idle context rather than directly to each other, which is what
makes the design race-free without holding a lock across a context switch.

**Userspace.** ELF64 loader, per-process address spaces, a 12-call syscall ABI,
and `fork`/`exec`/`wait`. User pointers are never dereferenced directly — every
syscall argument is walked through the page table and copied via the linear map,
so a bad pointer is an error return rather than a kernel fault.

**Userland.** `init`, a shell with builtins and fork/exec, and `hello`, `echo`,
`forktest`.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  user mode          init · sh · hello · echo · forktest      │
│                     alexos_user runtime, no heap             │
└───────────────────────────┬──────────────────────────────────┘
                            │  ecall  (a7 = number, a0..a5 = args)
┌───────────────────────────▼──────────────────────────────────┐
│  trap        user.S ──► user_trap_handler ──► syscall table   │
│              trap.S ──► kernel_trap_handler                   │
│              timer · PLIC · page faults · fatal traps         │
├───────────────────────────────────────────────────────────────┤
│  task        MLFQ scheduler · per-hart idle context           │
│              context switch (14 regs) · kernel stacks         │
│              fork · exec · wait · pid allocation              │
├───────────────────────────────────────────────────────────────┤
│  sync        SpinLock (masks interrupts, safe in handlers)    │
│              WaitQueue · Mutex · Semaphore (park the task)    │
├───────────────────────────────────────────────────────────────┤
│  mm          AddressSpace ──► Region ──► PageTable (Sv39)     │
│              heap: size classes ──► buddy allocator ──► DRAM  │
├───────────────────────────────────────────────────────────────┤
│  drivers     NS16550A UART (polled tx, interrupt rx) · PLIC   │
├───────────────────────────────────────────────────────────────┤
│  sbi         timer · IPI · hart start · console · shutdown    │
└───────────────────────────────────────────────────────────────┘
                            │
                     OpenSBI (M-mode firmware)
```

### Address space

Sv39 gives 39-bit addresses; the upper half starts at the first address with
bit 38 set. Physical memory is direct-mapped there, so a page table or a DMA
buffer is reachable without a temporary mapping.

```
0xffff_ffff_ffff_ffff ┐
                      │  kernel: image, linear map of DRAM, MMIO
0xffff_ffc0_0000_0000 ┘  VIRT_OFFSET — direct map base, 256 GiB window

0x0000_003f_ffff_f000 ┐  user stack, grows down
                      │
0x0000_0000_1000_0000 │  user heap, moved by sbrk
0x0000_0000_0001_0000 │  ELF image
0x0000_0000_0000_0000 ┘  unmapped, so a null dereference faults
```

The kernel's upper-half root entries are shared into every user page table, so
a trap from user mode runs kernel code without first switching `satp` — the
pre-KPTI Linux tradeoff, and the right one for a kernel with no untrusted
tenants.

### System calls

| # | Call | Notes |
|---|------|-------|
| 0 | `exit(code)` | releases the address space immediately, not at reap time |
| 1 | `write(fd, buf, len)` | |
| 2 | `read(fd, buf, len)` | blocks on a wait queue; may return short |
| 3 | `yield()` | |
| 4 | `getpid()` | |
| 5 | `getppid()` | |
| 6 | `fork()` | eager copy; returns 0 in the child |
| 7 | `exec(path, argv)` | resolves against programs embedded at build time |
| 8 | `waitpid(pid, status)` | `-1` for any child |
| 9 | `sbrk(delta)` | |
| 10 | `uptime()` | |
| 11 | `ps()` | |

`abi/syscall.rs` is included verbatim by both the kernel and the user runtime.
Two copies of a number list stay correct right up until someone inserts a call
in the middle of one of them.

## Design decisions worth defending

**RISC-V rather than x86_64.** Sv39 is three clean levels of 512 entries; there
is no GDT, no A20 line, no real-mode trampoline, and no 30 years of
compatibility to encode. The complexity that remains is the complexity that is
actually interesting.

**Stable Rust, no nightly.** The usual blockers are naked functions (stabilised
1.88) and `custom_test_frameworks`. Rather than put the whole kernel on nightly
for the test suite, tests register themselves at *link* time: `ktest!` emits a
descriptor into a `.ktest` section and the runner walks the range the linker
script marks out. Four lines of linker script instead of a toolchain
constraint.

**Zero dependencies.** The buddy allocator, the slab heap, the page tables, the
bitflags, the spin locks, and the wait queues are all in-tree. Pulling in
`bitflags` and `buddy_system_allocator` would have removed most of the parts
worth writing.

**Tasks switch through an idle context.** A task that enqueued itself and then
switched would leave a window where another hart could pop it and run it on
registers that had not been saved yet. Parking the outgoing task in
`Cpu::previous` and letting the idle loop requeue it means a task is only
runnable once it has finished switching off its own stack — race-free without
holding a lock across a context switch.

**A feedback queue rather than round-robin.** Round-robin gives a compute loop
the same share as a shell waiting on a keystroke. Demoting tasks that burn a
full quantum and promoting ones that block sorts interactive work to the top
without anyone having to declare it interactive.

**No guard pages on kernel stacks.** The linear map is built from 2 MiB
superpages, and punching a 4 KiB hole in one would mean splitting it and losing
the TLB benefit for every stack. A canary at the low end, checked on every
context switch, catches an overflow one quantum later than a guard page would —
but with a clear message rather than silent corruption.

## Testing

31 tests run inside a kernel task on the real allocator and real page tables,
after the machine is fully up. A buddy allocator that passes against a mock and
faults on a live system has not been tested.

```
$ make test
[I] running 31 kernel tests
  alexos::tests::frame_buddy_merges_on_free ... ok
  alexos::tests::address_space_duplicate_copies_not_shares ... ok
  alexos::tests::scheduler_preempts_a_task_that_never_yields ... ok
  ...
[I] all 31 tests passed in 80 ms
```

They report through QEMU's SiFive test finisher, so a failing assertion sets the
emulator's exit status and fails CI rather than merely printing.

Two tests earned their keep on the first run. One was a bug in the test itself —
a payload that ran 32 bytes past its region, which `copy_to_user` correctly
refused. The other found that buddy blocks were only *relatively* aligned:
contiguous, but not physically aligned to their own size, which is what a DMA
engine or a superpage mapping needs. That would have surfaced later as a
mysterious device failure rather than an allocator bug.

## Layout

```
abi/syscall.rs          the syscall ABI, shared by kernel and userland
kernel/
  linker.ld             higher-half layout, section symbols, .ktest section
  src/entry.S           boot trampoline: Sv39 on, jump to the high half
  src/arch.rs           CSR accessors, interrupt control
  src/mm/               addr · frame (buddy) · heap (slab) · page_table · space
  src/trap/             trap.S · user.S · dispatch · TrapFrame
  src/task/             switch.S · scheduler (MLFQ) · process (fork/exec)
  src/sync/             spin locks · wait queues · mutex · semaphore
  src/drivers/          UART · PLIC
  src/syscall/          dispatch and argument validation
  src/ktest.rs          link-section test registration
  src/tests.rs          the test suite
user/
  src/lib.rs            runtime: _start, syscalls, print, panic
  src/bin/              init · sh · hello · echo · forktest
scripts/
  run-qemu.sh           QEMU with a working watchdog
  integration-test.sh   boot and drive the shell over serial
```

Roughly 6,100 lines of kernel Rust, 380 lines of assembly, and 620 lines of
userland. 97 `unsafe` blocks, every one with a `SAFETY` comment stating what
makes it sound.

## Limitations

Deliberate, and named rather than hidden:

- **Single hart.** Secondary harts are parked in `entry.S`. The scheduler and
  locks are structured for SMP, but bringing up more harts needs the wait-queue
  handoff audited under real concurrency first.
- **No filesystem.** `exec` resolves names against ELF images embedded at build
  time — an initramfs without the archive format. A virtio-blk driver and an
  inode filesystem are the natural next step; `BLOCK_SIZE` and the virtio MMIO
  window are already reserved.
- **`fork` copies eagerly.** The `COW` PTE bit is defined and the fault handler
  has a place for it, but the optimisation is not wired up.
- **`sbrk` backs pages immediately.** The `LAZY` PTE bit is reserved for demand
  paging.
- **Device addresses are compile-time constants.** They are correct for QEMU's
  `virt` board; real hardware needs the device tree parsed instead.

## Reference

Written against the [RISC-V Privileged Specification](https://riscv.org/technical/specifications/),
the [SBI specification](https://github.com/riscv-non-isa/riscv-sbi-doc), and the
`virt` board layout in QEMU's `hw/riscv/virt.c`.

## License

MIT.
