//! Physical frame allocator: a binary buddy system.
//!
//! Why buddy rather than a free-list stack, which would be simpler and O(1)?
//! Because virtio queues, per-task kernel stacks and the heap's slab refills
//! all need *physically contiguous* runs of pages, and a plain stack of free
//! frames cannot serve those without scanning. Buddy gives contiguous
//! allocation up to 4 MiB with O(log n) split/merge and no external
//! fragmentation beyond internal rounding.
//!
//! Two pieces of bookkeeping:
//!
//! * **Intrusive doubly-linked free lists.** A free frame's own contents are
//!   dead, so the list nodes live *inside* the frames, reached through the
//!   direct map. Zero external metadata, and removal from the middle of a list
//!   -- which merging needs -- is O(1).
//! * **A free bitmap, one bit per block per order.** Merging has to ask "is my
//!   buddy free *at this order*?", which the list alone cannot answer. The
//!   bitmap costs about two bits per frame and is carved off the front of the
//!   managed region at init, so there is no compile-time cap on RAM.
//!
//! Buddy arithmetic runs on frame indices *relative* to the start of the
//! region, so the region itself needs no particular alignment: a block of
//! order `k` is simply one whose relative index is a multiple of `2^k`, an
//! invariant `init` establishes and split/merge preserve.

use core::fmt;

use crate::config::PAGE_SIZE;
use crate::mm::addr::{PhysAddr, PhysPageNum};
use crate::sync::SpinLock;

/// Largest block the allocator will serve: `2^MAX_ORDER` frames = 4 MiB.
pub const MAX_ORDER: usize = 10;

/// Sentinel for "no frame", since index 0 is a legitimate block.
const NIL: usize = usize::MAX;

/// The kernel's physical frame allocator.
pub static FRAME_ALLOCATOR: SpinLock<BuddyAllocator> = SpinLock::new(BuddyAllocator::empty());

/// Intrusive list node, written into the first 16 bytes of a free block.
#[repr(C)]
struct FreeNode {
    /// Relative index of the next free block of this order, or [`NIL`].
    next: usize,
    /// Relative index of the previous free block, or [`NIL`].
    prev: usize,
}

/// A binary buddy allocator over a contiguous range of physical frames.
pub struct BuddyAllocator {
    /// Frame number of relative index 0.
    base: PhysPageNum,
    /// Number of frames under management.
    frames: usize,
    /// Head of each order's free list, as a relative index.
    free_lists: [usize; MAX_ORDER + 1],
    /// Bit *i* of order *k*'s region is set when the block at relative index
    /// `i << k` is free and linked into `free_lists[k]`.
    bitmap: *mut u64,
    /// Bit offset at which each order's region starts within `bitmap`.
    order_bit_base: [usize; MAX_ORDER + 1],
    /// Frames currently handed out.
    allocated: usize,
    /// High-water mark of `allocated`, for the `meminfo` syscall.
    peak: usize,
}

// SAFETY: every access goes through the `SpinLock` wrapping the allocator, and
// `bitmap` points into the direct map, which is valid on every hart.
unsafe impl Send for BuddyAllocator {}

impl BuddyAllocator {
    /// An allocator managing nothing; replaced wholesale by [`init`].
    pub const fn empty() -> Self {
        Self {
            base: PhysPageNum(0),
            frames: 0,
            free_lists: [NIL; MAX_ORDER + 1],
            bitmap: core::ptr::null_mut(),
            order_bit_base: [0; MAX_ORDER + 1],
            allocated: 0,
            peak: 0,
        }
    }

    /// Take ownership of the physical frames in `[start, end)`.
    ///
    /// # Safety
    /// The range must be real, currently unused DRAM, reachable through the
    /// direct map, and not overlap the kernel image or firmware.
    unsafe fn init(&mut self, start: PhysAddr, end: PhysAddr) {
        let first = start.align_up().ppn();
        let last = end.align_down().ppn();
        assert!(last.0 > first.0, "frame allocator given an empty range");

        let total = last.0 - first.0;

        // Reserve the bitmap off the front of the region. Sizing it needs the
        // frame count, which shrinks once the bitmap is deducted, so compute
        // from `total` and accept over-allocating by a few bytes.
        let bits: usize = (0..=MAX_ORDER).map(|k| total.div_ceil(1 << k)).sum();
        let bitmap_bytes = bits.div_ceil(8).next_multiple_of(8);
        let bitmap_frames = bitmap_bytes.div_ceil(PAGE_SIZE);

        self.bitmap = first.as_ptr() as *mut u64;
        // SAFETY: the bitmap frames are inside the range the caller vouched
        // for, and the direct map makes them writable.
        unsafe { core::ptr::write_bytes(self.bitmap as *mut u8, 0, bitmap_frames * PAGE_SIZE) };

        self.base = PhysPageNum(first.0 + bitmap_frames);
        self.frames = total - bitmap_frames;
        self.free_lists = [NIL; MAX_ORDER + 1];
        self.allocated = 0;
        self.peak = 0;

        let mut bit = 0;
        for k in 0..=MAX_ORDER {
            self.order_bit_base[k] = bit;
            bit += self.frames.div_ceil(1 << k);
        }

        // Seed the free lists. At each step take the largest order whose block
        // both starts at a correctly aligned relative index and fits in what
        // remains -- this is what establishes the alignment invariant that
        // split and merge then maintain.
        let mut idx = 0;
        while idx < self.frames {
            let mut order = MAX_ORDER;
            while order > 0 && (idx & ((1 << order) - 1) != 0 || idx + (1 << order) > self.frames) {
                order -= 1;
            }
            // SAFETY: the block is inside the managed region and not yet listed.
            unsafe { self.push(order, idx) };
            idx += 1 << order;
        }
    }

    // -- bitmap ------------------------------------------------------------

    #[inline]
    fn bit_index(&self, order: usize, idx: usize) -> usize {
        self.order_bit_base[order] + (idx >> order)
    }

    #[inline]
    fn is_free(&self, order: usize, idx: usize) -> bool {
        let bit = self.bit_index(order, idx);
        // SAFETY: `bit` is inside the bitmap by construction of order_bit_base.
        unsafe { (*self.bitmap.add(bit / 64) >> (bit % 64)) & 1 != 0 }
    }

    #[inline]
    fn set_free(&mut self, order: usize, idx: usize, free: bool) {
        let bit = self.bit_index(order, idx);
        // SAFETY: as above; the allocator lock gives exclusive access.
        unsafe {
            let word = self.bitmap.add(bit / 64);
            if free {
                *word |= 1 << (bit % 64);
            } else {
                *word &= !(1 << (bit % 64));
            }
        }
    }

    // -- intrusive list ----------------------------------------------------

    /// Borrow the list node stored inside the free block at `idx`.
    ///
    /// # Safety
    /// The block must be within the region and not currently allocated; its
    /// contents are undefined otherwise.
    #[inline]
    unsafe fn node(&self, idx: usize) -> &'static mut FreeNode {
        let ppn = PhysPageNum(self.base.0 + idx);
        // SAFETY: contract delegated to the caller. The direct map covers every
        // managed frame, and a free block's contents are ours to scribble on.
        unsafe { &mut *(ppn.as_ptr() as *mut FreeNode) }
    }

    /// Link a block into its order's free list and mark it free.
    ///
    /// # Safety
    /// The block must be aligned for `order`, inside the region, and absent
    /// from every free list.
    unsafe fn push(&mut self, order: usize, idx: usize) {
        let head = self.free_lists[order];
        // SAFETY: `idx` is a free block per the caller's contract.
        unsafe {
            let node = self.node(idx);
            node.next = head;
            node.prev = NIL;
            if head != NIL {
                self.node(head).prev = idx;
            }
        }
        self.free_lists[order] = idx;
        self.set_free(order, idx, true);
    }

    /// Unlink a block from its order's free list and mark it used.
    ///
    /// # Safety
    /// The block must currently be linked into `free_lists[order]`.
    unsafe fn remove(&mut self, order: usize, idx: usize) {
        // SAFETY: the caller guarantees the block is on this list, so its node
        // is initialised.
        unsafe {
            let (next, prev) = {
                let node = self.node(idx);
                (node.next, node.prev)
            };
            if prev != NIL {
                self.node(prev).next = next;
            } else {
                self.free_lists[order] = next;
            }
            if next != NIL {
                self.node(next).prev = prev;
            }
        }
        self.set_free(order, idx, false);
    }

    // -- allocation --------------------------------------------------------

    /// Allocate `2^order` contiguous frames.
    pub fn alloc(&mut self, order: usize) -> Option<PhysPageNum> {
        if order > MAX_ORDER {
            return None;
        }

        // Smallest available order that can satisfy the request.
        let mut donor = order;
        while donor <= MAX_ORDER && self.free_lists[donor] == NIL {
            donor += 1;
        }
        if donor > MAX_ORDER {
            return None;
        }

        let idx = self.free_lists[donor];
        // SAFETY: `idx` is the head of a non-empty list, so it is linked.
        unsafe { self.remove(donor, idx) };

        // Split down, returning the lower half and freeing each upper half.
        let mut level = donor;
        while level > order {
            level -= 1;
            let buddy = idx + (1 << level);
            // SAFETY: the upper half of a block we exclusively own is itself a
            // correctly aligned, unlisted block of `level`.
            unsafe { self.push(level, buddy) };
        }

        self.allocated += 1 << order;
        self.peak = self.peak.max(self.allocated);
        Some(PhysPageNum(self.base.0 + idx))
    }

    /// Return `2^order` frames starting at `ppn`, merging with free buddies.
    ///
    /// # Safety
    /// `ppn`/`order` must exactly match a live allocation from [`alloc`], and
    /// no reference into those frames may outlive this call.
    pub unsafe fn free(&mut self, ppn: PhysPageNum, order: usize) {
        let mut idx = ppn.0.checked_sub(self.base.0).expect("frame below the managed region");
        assert!(idx + (1 << order) <= self.frames, "frame above the managed region");
        assert!(idx & ((1 << order) - 1) == 0, "misaligned free for order {order}");
        debug_assert!(!self.is_free(order, idx), "double free of frame {ppn:?}");

        self.allocated -= 1 << order;

        // Coalesce upward while the buddy is entirely free at this order.
        let mut level = order;
        while level < MAX_ORDER {
            let buddy = idx ^ (1 << level);
            if buddy + (1 << level) > self.frames || !self.is_free(level, buddy) {
                break;
            }
            // SAFETY: the buddy's free bit is set, so it is on this free list.
            unsafe { self.remove(level, buddy) };
            idx = idx.min(buddy);
            level += 1;
        }

        // SAFETY: `idx` is aligned for `level`, in range, and unlisted -- the
        // merge loop removed every block it absorbed.
        unsafe { self.push(level, idx) };
    }

    /// Frames currently allocated.
    pub fn used_frames(&self) -> usize {
        self.allocated
    }

    /// Frames still available.
    pub fn free_frames(&self) -> usize {
        self.frames - self.allocated
    }

    /// Total frames under management.
    pub fn total_frames(&self) -> usize {
        self.frames
    }

    /// Largest number of frames ever simultaneously allocated.
    pub fn peak_frames(&self) -> usize {
        self.peak
    }
}

impl fmt::Debug for BuddyAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BuddyAllocator {{ {}/{} frames used, peak {} }}",
            self.allocated, self.frames, self.peak
        )
    }
}

/// Smallest order whose block holds at least `count` frames.
#[inline]
pub const fn order_for(count: usize) -> usize {
    if count <= 1 { 0 } else { (usize::BITS - (count - 1).leading_zeros()) as usize }
}

/// Hand every frame between the end of the kernel image and the top of DRAM to
/// the allocator.
///
/// # Safety
/// Must be called once, before any frame is allocated, and `dram_end` must be
/// the true end of usable physical memory.
pub unsafe fn init(dram_end: PhysAddr) {
    let start = PhysAddr(crate::mm::kernel_end_phys());
    // SAFETY: everything above the kernel image is free DRAM; the caller
    // vouches for `dram_end`, and the direct map covers all of it.
    unsafe { FRAME_ALLOCATOR.lock().init(start, dram_end) };
}

/// Allocate one zeroed frame.
pub fn alloc_frame() -> Option<PhysPageNum> {
    let ppn = FRAME_ALLOCATOR.lock().alloc(0)?;
    // Freed frames still hold the previous owner's data -- and, in the case of
    // a frame that was on a free list, our own list pointers. Handing either
    // to a user process would leak kernel state, so zero unconditionally.
    // SAFETY: we exclusively own the frame until it is freed.
    unsafe { ppn.zero() };
    Some(ppn)
}

/// Allocate `2^order` contiguous frames, zeroed.
pub fn alloc_frames(order: usize) -> Option<PhysPageNum> {
    let ppn = FRAME_ALLOCATOR.lock().alloc(order)?;
    // SAFETY: we own the whole run; zeroing it is in bounds.
    unsafe { core::ptr::write_bytes(ppn.as_ptr(), 0, PAGE_SIZE << order) };
    Some(ppn)
}

/// Release frames obtained from [`alloc_frames`].
///
/// # Safety
/// `ppn` and `order` must match a live allocation exactly.
pub unsafe fn free_frames(ppn: PhysPageNum, order: usize) {
    // SAFETY: contract delegated to the caller.
    unsafe { FRAME_ALLOCATOR.lock().free(ppn, order) };
}

/// An owned run of `2^order` physical frames, released on drop.
///
/// Page tables, kernel stacks and user pages all hold their storage this way so
/// that tearing down an address space is a matter of dropping a collection.
#[derive(Debug)]
pub struct Frame {
    ppn: PhysPageNum,
    order: usize,
}

impl Frame {
    /// Allocate a single zeroed frame.
    pub fn alloc() -> Option<Self> {
        Some(Self { ppn: alloc_frame()?, order: 0 })
    }

    /// Allocate `2^order` contiguous zeroed frames.
    pub fn alloc_order(order: usize) -> Option<Self> {
        Some(Self { ppn: alloc_frames(order)?, order })
    }

    /// Frame number of the first frame in the run.
    pub fn ppn(&self) -> PhysPageNum {
        self.ppn
    }

    /// Number of frames in the run.
    pub fn count(&self) -> usize {
        1 << self.order
    }

    /// The run's contents, through the direct map.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `self` owns the frames for as long as it lives.
        unsafe { core::slice::from_raw_parts(self.ppn.as_ptr(), PAGE_SIZE << self.order) }
    }

    /// The run's contents, mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `&mut self` proves exclusive ownership.
        unsafe { core::slice::from_raw_parts_mut(self.ppn.as_ptr(), PAGE_SIZE << self.order) }
    }

    /// Give up ownership, leaving the frames allocated.
    ///
    /// Used where the hardware, not Rust, owns the lifetime -- the root page
    /// table of a live address space, for instance.
    pub fn into_raw(self) -> PhysPageNum {
        let ppn = self.ppn;
        core::mem::forget(self);
        ppn
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // SAFETY: `Frame` is only ever constructed from a matching allocation,
        // and ownership means no one else holds a reference to the storage.
        unsafe { free_frames(self.ppn, self.order) };
    }
}
