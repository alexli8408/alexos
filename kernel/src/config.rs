//! Compile-time layout of the machine and of the kernel's address space.
//!
//! The physical numbers describe QEMU's `virt` board. They are checked against
//! the device tree at boot (`dtb::probe`) rather than trusted blindly, so
//! running on real hardware means fixing the probe, not these constants.

/// Bytes per page, and the shift that turns an address into a page number.
pub const PAGE_SIZE: usize = 4096;
/// `log2(PAGE_SIZE)`.
pub const PAGE_SHIFT: usize = 12;

/// Sv39: 39-bit virtual addresses, three levels of 512-entry tables.
pub const SV39_LEVELS: usize = 3;
/// Entries in one page table: 4096 bytes / 8 bytes per PTE.
pub const PTE_PER_TABLE: usize = 512;

/// Direct map base. Physical address `p` is readable at `p + VIRT_OFFSET` for
/// the lifetime of the kernel, which is what makes it legal to touch a page
/// table or a DMA buffer without a temporary mapping.
///
/// This is the lowest address whose bit 38 is set, i.e. the first valid
/// upper-half Sv39 address; the window it opens is 256 GiB wide.
pub const VIRT_OFFSET: usize = 0xffff_ffc0_0000_0000;

/// Where OpenSBI hands off, and therefore where the kernel image is loaded.
pub const KERNEL_PHYS_BASE: usize = 0x8020_0000;

/// Start of DRAM on the `virt` board. The 2 MiB below `KERNEL_PHYS_BASE`
/// belongs to OpenSBI and must never be handed to the frame allocator.
pub const DRAM_BASE: usize = 0x8000_0000;

/// Default DRAM size QEMU gives us (`-m 128M`). Overridden by the device tree.
pub const DRAM_SIZE: usize = 128 * 1024 * 1024;

/// Kernel heap, carved out of `.bss` and handed to the slab allocator.
pub const KERNEL_HEAP_SIZE: usize = 8 * 1024 * 1024;

/// Boot stack per hart, matching the `.equ` in entry.S.
pub const BOOT_STACK_SIZE: usize = 64 * 1024;

/// Kernel stack given to each task. Deep enough for a nested trap plus the
/// filesystem's recursive block walk.
pub const KERNEL_STACK_SIZE: usize = 32 * 1024;

/// Harts the kernel is willing to bring up.
pub const MAX_HARTS: usize = 4;

/// Scheduling quantum in milliseconds.
pub const TICK_MS: u64 = 10;

// ---------------------------------------------------------------------------
// User address space layout (low half, so it never collides with the kernel).
// ---------------------------------------------------------------------------

/// Where `exec` places the ELF image. Page 0 is left unmapped so that a null
/// dereference in user code faults instead of silently succeeding.
pub const USER_IMAGE_BASE: usize = 0x0000_0000_0001_0000;

/// Top of the user stack; it grows down from here.
pub const USER_STACK_TOP: usize = 0x0000_003f_ffff_f000;

/// Initial user stack size, extended on demand by the page fault handler.
pub const USER_STACK_SIZE: usize = 256 * 1024;

/// Base of the user heap managed by `sbrk`.
pub const USER_HEAP_BASE: usize = 0x0000_0000_1000_0000;

/// Highest address a user program may name. Anything above this belongs to the
/// kernel and is rejected by the syscall argument validator.
pub const USER_MAX_ADDR: usize = 0x0000_0040_0000_0000;

// ---------------------------------------------------------------------------
// Memory-mapped devices on the `virt` board.
// ---------------------------------------------------------------------------

/// Core-local interruptor: per-hart timer compare and software interrupts.
pub const CLINT_BASE: usize = 0x0200_0000;

/// Platform-level interrupt controller.
pub const PLIC_BASE: usize = 0x0c00_0000;

/// NS16550A UART.
pub const UART_BASE: usize = 0x1000_0000;
/// PLIC source number the UART is wired to.
pub const UART_IRQ: u32 = 10;

/// VirtIO MMIO transport: eight 4 KiB slots, IRQs 1 through 8.
pub const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
/// Bytes between consecutive transport slots.
pub const VIRTIO_MMIO_STRIDE: usize = 0x1000;
/// Number of slots to probe.
pub const VIRTIO_MMIO_SLOTS: usize = 8;
/// PLIC source number of slot 0; slot *n* is `VIRTIO_IRQ_BASE + n`.
pub const VIRTIO_IRQ_BASE: u32 = 1;

/// SiFive test finisher: writing here exits QEMU, which is how the kernel test
/// harness reports pass/fail to the host.
pub const TEST_DEVICE_BASE: usize = 0x0010_0000;

/// Filesystem block size. Chosen equal to `PAGE_SIZE` so a cached block can be
/// mapped straight into a user address space by `mmap` with no bounce copy.
pub const BLOCK_SIZE: usize = 4096;

/// Blocks held in the buffer cache.
pub const BLOCK_CACHE_CAPACITY: usize = 256;
