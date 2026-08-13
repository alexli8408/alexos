//! Kernel heap: a segregated free-list (slab-style) allocator.
//!
//! Backing `alloc` needs a general allocator, but kernel allocation is not
//! general-purpose traffic. It is overwhelmingly small, short-lived, and
//! repetitive -- a `Vec` node here, a `Box<Inode>` there, thousands of times.
//! So instead of a first-fit list with a header on every block:
//!
//! * Requests up to 2 KiB round up to one of nine power-of-two size classes.
//!   Each class keeps a LIFO free list threaded through the free blocks
//!   themselves, making both `alloc` and `dealloc` a couple of pointer moves
//!   with no per-block header and no search.
//! * A class that runs dry claims one frame from the buddy allocator and
//!   carves it into blocks. Since a frame is 4 KiB aligned and every class is
//!   a power of two no larger than that, block addresses are automatically
//!   aligned to their own size, which is what lets `alloc` satisfy alignment
//!   requests without any extra arithmetic.
//! * Anything larger goes straight to the buddy allocator, rounded up to a
//!   whole number of frames.
//!
//! LIFO reuse is deliberate: the most recently freed block of a class is the
//! one most likely to still be in cache.
//!
//! The tradeoff is internal fragmentation -- a 33-byte request occupies 64
//! bytes -- and that partially used frames are never returned to the buddy
//! allocator. Both are acceptable for a kernel whose allocation mix is stable
//! after boot; neither would be for a general-purpose userland malloc.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;

use crate::config::PAGE_SIZE;
use crate::mm::frame::{alloc_frames, free_frames, order_for};
use crate::sync::SpinLock;

/// Size classes, in bytes: 8 through 2048.
const CLASS_SIZES: [usize; 9] = [8, 16, 32, 64, 128, 256, 512, 1024, 2048];
const NUM_CLASSES: usize = CLASS_SIZES.len();

/// Largest request served from a size class; above this we go to the buddy.
const MAX_CLASS_SIZE: usize = CLASS_SIZES[NUM_CLASSES - 1];

/// The allocator backing every `Box`, `Vec` and `String` in the kernel.
#[global_allocator]
static KERNEL_HEAP: Heap = Heap { inner: SpinLock::new(HeapInner::new()) };

struct Heap {
    inner: SpinLock<HeapInner>,
}

struct HeapInner {
    /// Head of each class's free list. Each free block's first word points at
    /// the next free block of the same class.
    free: [*mut u8; NUM_CLASSES],
    /// Bytes handed out, for `meminfo`.
    allocated: usize,
    /// Bytes claimed from the buddy allocator.
    reserved: usize,
}

// SAFETY: the `SpinLock` serialises every access, and the raw pointers refer
// to direct-mapped frames that are valid on every hart.
unsafe impl Send for HeapInner {}

impl HeapInner {
    const fn new() -> Self {
        Self { free: [ptr::null_mut(); NUM_CLASSES], allocated: 0, reserved: 0 }
    }

    /// Smallest class index that satisfies `layout`, or `None` if the request
    /// belongs to the large path.
    ///
    /// Alignment folds into the size: because a class of size `s` only ever
    /// yields `s`-aligned addresses, asking for a class at least as large as
    /// the requested alignment satisfies it for free.
    fn class_for(layout: Layout) -> Option<usize> {
        let need = layout.size().max(layout.align());
        if need > MAX_CLASS_SIZE {
            return None;
        }
        CLASS_SIZES.iter().position(|&s| s >= need)
    }

    /// Claim a frame and thread it onto class `class`'s free list.
    fn refill(&mut self, class: usize) -> bool {
        let size = CLASS_SIZES[class];
        let ppn = match alloc_frames(0) {
            Some(ppn) => ppn,
            None => return false,
        };
        self.reserved += PAGE_SIZE;

        // Build the list back-to-front so the lowest address ends up at the
        // head; allocation order then walks the frame upward, which is kinder
        // to the prefetcher than scattering.
        let base = ppn.as_ptr();
        let count = PAGE_SIZE / size;
        let mut head = self.free[class];
        for i in (0..count).rev() {
            // SAFETY: `i < count`, so the block lies inside the frame we own,
            // and every class size is at least 8 bytes -- enough for the link.
            unsafe {
                let block = base.add(i * size);
                ptr::write(block as *mut *mut u8, head);
                head = block;
            }
        }
        self.free[class] = head;
        true
    }
}

// SAFETY: `alloc` returns either null or a pointer to an owned, correctly
// sized and aligned block, and `dealloc` is only valid for pointers this
// allocator produced with the same layout -- the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut heap = self.inner.lock();

        let Some(class) = HeapInner::class_for(layout) else {
            // Large path: whole frames straight from the buddy allocator.
            // Alignments above a page are not reachable from kernel code and
            // would need the buddy region itself to be aligned, so reject them
            // rather than return something subtly wrong.
            if layout.align() > PAGE_SIZE {
                return ptr::null_mut();
            }
            let order = order_for(layout.size().div_ceil(PAGE_SIZE));
            return match alloc_frames(order) {
                Some(ppn) => {
                    heap.allocated += PAGE_SIZE << order;
                    heap.reserved += PAGE_SIZE << order;
                    ppn.as_ptr()
                }
                None => ptr::null_mut(),
            };
        };

        if heap.free[class].is_null() && !heap.refill(class) {
            return ptr::null_mut();
        }

        let block = heap.free[class];
        // SAFETY: `block` came off this class's free list, so its first word is
        // the link to the next free block.
        heap.free[class] = unsafe { ptr::read(block as *mut *mut u8) };
        heap.allocated += CLASS_SIZES[class];
        block
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut heap = self.inner.lock();

        let Some(class) = HeapInner::class_for(layout) else {
            let order = order_for(layout.size().div_ceil(PAGE_SIZE));
            heap.allocated -= PAGE_SIZE << order;
            heap.reserved -= PAGE_SIZE << order;
            drop(heap);
            // SAFETY: the same layout produced this pointer, so it names the
            // start of a run of `2^order` frames from the buddy allocator.
            unsafe {
                free_frames(crate::mm::addr::VirtAddr(ptr as usize).to_phys().ppn(), order)
            };
            return;
        };

        // Push onto the class's free list. LIFO, so the next allocation of this
        // size reuses a block that is probably still cached.
        // SAFETY: the block is at least 8 bytes and is no longer in use.
        unsafe { ptr::write(ptr as *mut *mut u8, heap.free[class]) };
        heap.free[class] = ptr;
        heap.allocated -= CLASS_SIZES[class];
    }
}

/// Bytes currently allocated and bytes claimed from the frame allocator.
pub fn stats() -> (usize, usize) {
    let heap = KERNEL_HEAP.inner.lock();
    (heap.allocated, heap.reserved)
}

/// Prove the heap works before anything depends on it.
///
/// Runs at boot rather than as a unit test because a broken heap makes every
/// later failure unintelligible -- and because the whole point is to exercise
/// the path from `Box` down through the buddy allocator on real hardware.
pub fn self_test() {
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    let a = Box::new(0xdead_beef_u64);
    let b = Box::new([7u8; 300]);
    assert_eq!(*a, 0xdead_beef);
    assert_eq!(b[299], 7);

    // Force several refills and a large allocation, then check the values
    // survived: a size-class mix-up would corrupt one of these.
    let mut v: Vec<usize> = Vec::new();
    for i in 0..4096 {
        v.push(i);
    }
    assert_eq!(v.iter().sum::<usize>(), 4096 * 4095 / 2);

    let big: Vec<u8> = alloc::vec![0xab; 64 * 1024];
    assert_eq!(big.len(), 64 * 1024);
    assert!(big.iter().all(|&x| x == 0xab));

    drop(v);
    drop(big);
    drop(a);
    drop(b);
}
