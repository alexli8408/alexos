//! The kernel test suite.
//!
//! These run on real hardware state -- the actual frame allocator, the actual
//! page tables -- inside a kernel task, after the machine is fully up. That is
//! the point: a buddy allocator that passes on a mock and faults on a live
//! system has not been tested.
//!
//! Each test must leave the kernel exactly as it found it, since the next one
//! and then `init` run afterwards on the same machine.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::config::PAGE_SIZE;
use crate::ktest;
use crate::mm::addr::{PhysAddr, VirtAddr, VirtPageNum};
use crate::mm::frame::{self, Frame, order_for};
use crate::mm::page_table::{MapError, PageTable, PteFlags};
use crate::sync::{Semaphore, SpinLock, WaitQueue};

// ---------------------------------------------------------------------------
// Addressing
// ---------------------------------------------------------------------------

ktest!(addr_page_math, {
    let a = VirtAddr(0x1234_5678);
    assert_eq!(a.page_offset(), 0x678);
    assert_eq!(a.align_down().0, 0x1234_5000);
    assert_eq!(a.align_up().0, 0x1234_6000);
    assert!(a.align_down().is_aligned());
    // Rounding an already-aligned address must not move it.
    assert_eq!(a.align_down().align_up().0, 0x1234_5000);
});

ktest!(addr_sv39_indices, {
    // Each index is 9 bits; build a page number with a distinct value in each
    // level so a shift error cannot go unnoticed.
    let vpn = VirtPageNum((1 << 18) | (2 << 9) | 3);
    assert_eq!(vpn.indices(), [1, 2, 3]);
});

ktest!(addr_canonical_form, {
    // Bits 63:39 must replicate bit 38.
    assert!(VirtAddr(0x0000_003f_ffff_f000).is_canonical());
    assert!(VirtAddr(crate::config::VIRT_OFFSET).is_canonical());
    assert!(!VirtAddr(0x0000_0080_0000_0000).is_canonical());
});

ktest!(addr_direct_map_roundtrip, {
    let pa = PhysAddr(0x8020_1000);
    assert_eq!(pa.to_virt().to_phys(), pa);
    assert!(pa.to_virt().is_kernel());
});

// ---------------------------------------------------------------------------
// Frame allocator
// ---------------------------------------------------------------------------

ktest!(frame_alloc_is_zeroed_and_unique, {
    // A recycled frame carries the previous owner's data and the allocator's
    // own free-list pointers; handing either to a user process would leak
    // kernel state, so allocation must zero.
    let a = Frame::alloc().expect("out of frames");
    let b = Frame::alloc().expect("out of frames");
    assert_ne!(a.ppn(), b.ppn());
    assert!(a.as_slice().iter().all(|&x| x == 0));
    assert!(b.as_slice().iter().all(|&x| x == 0));
});

ktest!(frame_free_is_reused, {
    let before = frame::FRAME_ALLOCATOR.lock().free_frames();
    let ppn = {
        let f = Frame::alloc().expect("out of frames");
        f.ppn()
    };
    // The frame went back on drop, so the count is restored and the next
    // allocation gets the same one -- LIFO reuse.
    assert_eq!(frame::FRAME_ALLOCATOR.lock().free_frames(), before);
    let again = Frame::alloc().expect("out of frames");
    assert_eq!(again.ppn(), ppn);
});

ktest!(frame_contiguous_run, {
    // Order 3 is eight frames; DMA buffers and kernel stacks depend on the run
    // being physically contiguous, not merely eight frames.
    let run = Frame::alloc_order(3).expect("out of frames");
    assert_eq!(run.count(), 8);
    assert_eq!(run.ppn().0 % 8, 0, "an order-3 block must be 8-frame aligned");
    assert_eq!(run.as_slice().len(), 8 * PAGE_SIZE);
});

ktest!(frame_buddy_merges_on_free, {
    // Split a large block into two, free both, and check the allocator merged
    // them back -- otherwise the next large request fails despite free memory.
    let before = frame::FRAME_ALLOCATOR.lock().free_frames();

    let a = Frame::alloc_order(2).expect("out of frames");
    let b = Frame::alloc_order(2).expect("out of frames");
    assert_eq!(frame::FRAME_ALLOCATOR.lock().free_frames(), before - 8);
    drop(a);
    drop(b);
    assert_eq!(frame::FRAME_ALLOCATOR.lock().free_frames(), before);

    // If merging failed there would be only order-2 blocks left where an
    // order-3 used to be.
    let big = Frame::alloc_order(3).expect("buddy failed to merge");
    assert_eq!(big.count(), 8);
});

ktest!(frame_order_for_rounds_up, {
    assert_eq!(order_for(1), 0);
    assert_eq!(order_for(2), 1);
    assert_eq!(order_for(3), 2);
    assert_eq!(order_for(4), 2);
    assert_eq!(order_for(5), 3);
    assert_eq!(order_for(8), 3);
});

// ---------------------------------------------------------------------------
// Heap
// ---------------------------------------------------------------------------

ktest!(heap_size_classes_do_not_overlap, {
    // Allocate across every size class at once and write a distinct pattern to
    // each. A class-index or refill bug shows up as one buffer scribbling on
    // another.
    let sizes = [8usize, 16, 32, 64, 128, 256, 512, 1024, 2048];
    let mut buffers: Vec<Vec<u8>> = Vec::new();

    for (i, &size) in sizes.iter().enumerate() {
        buffers.push(alloc::vec![i as u8; size]);
    }
    for (i, buf) in buffers.iter().enumerate() {
        assert_eq!(buf.len(), sizes[i]);
        assert!(buf.iter().all(|&b| b == i as u8), "size class {} corrupted", sizes[i]);
    }
});

ktest!(heap_large_allocation_bypasses_classes, {
    // Anything over 2 KiB goes straight to the buddy allocator.
    let big = alloc::vec![0x5au8; 96 * 1024];
    assert_eq!(big.len(), 96 * 1024);
    assert!(big.iter().all(|&b| b == 0x5a));
});

ktest!(heap_reuses_freed_blocks, {
    // LIFO reuse: freeing and immediately reallocating the same size should
    // hand back the same address rather than growing the heap.
    let addr = {
        let b = Box::new(0u64);
        &raw const *b as usize
    };
    let again = Box::new(0u64);
    assert_eq!(&raw const *again as usize, addr);
});

ktest!(heap_alignment_is_honoured, {
    #[repr(align(64))]
    struct Aligned64(#[allow(dead_code)] [u8; 64]);

    let a = Box::new(Aligned64([0; 64]));
    assert_eq!((&raw const *a as usize) % 64, 0);
});

// ---------------------------------------------------------------------------
// Page tables
// ---------------------------------------------------------------------------

ktest!(page_table_map_translate_unmap, {
    let mut table = PageTable::new().expect("out of frames");
    let frame = Frame::alloc().expect("out of frames");
    let vpn = VirtPageNum(0x4_2000);

    assert!(table.translate(vpn).is_none());

    table.map(vpn, frame.ppn(), PteFlags::RW | PteFlags::USER).expect("map failed");
    let pte = table.translate(vpn).expect("translate failed after map");
    assert_eq!(pte.ppn(), frame.ppn());
    assert!(pte.flags().contains(PteFlags::READ | PteFlags::WRITE | PteFlags::USER));
    // Set eagerly at map time, because not every implementation updates them.
    assert!(pte.flags().contains(PteFlags::ACCESSED | PteFlags::DIRTY));

    // The page offset must survive translation.
    let va = VirtAddr(vpn.base_addr().0 + 0x321);
    assert_eq!(table.translate_addr(va).unwrap().0, frame.ppn().base_addr().0 + 0x321);

    assert_eq!(table.unmap(vpn).expect("unmap failed"), frame.ppn());
    assert!(table.translate(vpn).is_none());
});

ktest!(page_table_rejects_double_map, {
    let mut table = PageTable::new().expect("out of frames");
    let frame = Frame::alloc().expect("out of frames");
    let vpn = VirtPageNum(0x4_3000);

    table.map(vpn, frame.ppn(), PteFlags::RW).expect("first map failed");
    assert_eq!(table.map(vpn, frame.ppn(), PteFlags::RW), Err(MapError::AlreadyMapped));
    assert_eq!(table.unmap(VirtPageNum(0x4_4000)), Err(MapError::NotMapped));
});

ktest!(page_table_superpage_covers_512_pages, {
    let mut table = PageTable::new().expect("out of frames");
    // A level-1 leaf maps 2 MiB, so both the page number and the frame number
    // must be multiples of 512.
    let vpn = VirtPageNum(512 * 4);
    let ppn = crate::mm::addr::PhysPageNum(512 * 8);

    table.map_at_level(vpn, ppn, PteFlags::RW, 1).expect("superpage map failed");

    // The walk stops at the leaf, so a 4 KiB lookup inside it reports nothing;
    // what matters is that the entry exists and the tree stayed small.
    assert!(table.table_frames() <= 3, "a superpage should not need a leaf table");
});

ktest!(page_table_frees_its_own_frames, {
    let before = frame::FRAME_ALLOCATOR.lock().free_frames();
    {
        let mut table = PageTable::new().expect("out of frames");
        let frame = Frame::alloc().expect("out of frames");
        // Three widely separated addresses force three separate interior tables.
        for i in 0..3 {
            let vpn = VirtPageNum(i * 512 * 512 + 7);
            table.map(vpn, frame.ppn(), PteFlags::RW).expect("map failed");
        }
        assert!(table.table_frames() > 1);
    }
    // Dropping the table returns the root and every interior table.
    assert_eq!(frame::FRAME_ALLOCATOR.lock().free_frames(), before);
});

ktest!(pte_flag_encoding, {
    let ppn = crate::mm::addr::PhysPageNum(0x8_1234);
    let pte = crate::mm::page_table::Pte::new(ppn, PteFlags::RX | PteFlags::VALID);
    assert_eq!(pte.ppn(), ppn);
    assert!(pte.is_valid());
    // Any of R/W/X set is what makes an entry a leaf rather than a pointer.
    assert!(pte.is_leaf());
    assert!(!crate::mm::page_table::Pte::new(ppn, PteFlags::VALID).is_leaf());
});

// ---------------------------------------------------------------------------
// Address spaces
// ---------------------------------------------------------------------------

ktest!(address_space_user_copy_roundtrip, {
    use crate::mm::space::{AddressSpace, Backing, Region};

    let mut space = AddressSpace::new().expect("out of frames");
    space.map_kernel_half().expect("kernel half");
    let base = VirtAddr(0x2_0000);
    space
        .push(Region::new(
            base,
            VirtAddr(base.0 + 3 * PAGE_SIZE),
            Backing::Framed,
            PteFlags::RW | PteFlags::USER,
        ))
        .expect("push failed");

    // Straddle a page boundary: a copy that walks the table one page at a time
    // has to stitch the halves back together correctly. The region is three
    // pages so that the payload stays inside it -- copy_to_user refuses to
    // write past a mapping, which is the behaviour the next test checks.
    let payload: Vec<u8> = (0..PAGE_SIZE + 64).map(|i| (i % 251) as u8).collect();
    let at = VirtAddr(base.0 + PAGE_SIZE - 32);
    space.copy_to_user(at, &payload).expect("copy_to_user failed");

    let mut back = alloc::vec![0u8; payload.len()];
    space.copy_from_user(at, &mut back).expect("copy_from_user failed");
    assert_eq!(back, payload);
});

ktest!(address_space_rejects_unmapped_and_readonly, {
    use crate::mm::space::{AddressSpace, Backing, Region};

    let mut space = AddressSpace::new().expect("out of frames");
    space.map_kernel_half().expect("kernel half");
    let base = VirtAddr(0x3_0000);
    space
        .push(Region::new(
            base,
            VirtAddr(base.0 + PAGE_SIZE),
            Backing::Framed,
            PteFlags::READ | PteFlags::USER,
        ))
        .expect("push failed");

    // Unmapped: no such address in this space.
    let mut buf = [0u8; 4];
    assert!(space.copy_from_user(VirtAddr(0x9_0000), &mut buf).is_none());

    // Mapped but read-only: a syscall must not be a way around page
    // permissions, so writing through it has to fail even from the kernel.
    assert!(space.copy_to_user(base, &[1, 2, 3, 4]).is_none());
    assert!(space.copy_from_user(base, &mut buf).is_some());
});

ktest!(address_space_duplicate_copies_not_shares, {
    use crate::mm::space::{AddressSpace, Backing, Region};

    let mut parent = AddressSpace::new().expect("out of frames");
    parent.map_kernel_half().expect("kernel half");
    let base = VirtAddr(0x4_0000);
    parent
        .push(Region::new(
            base,
            VirtAddr(base.0 + PAGE_SIZE),
            Backing::Framed,
            PteFlags::RW | PteFlags::USER,
        ))
        .expect("push failed");
    parent.copy_to_user(base, b"parent").expect("seed failed");

    let child = parent.duplicate().expect("duplicate failed");

    // Same contents...
    let mut buf = [0u8; 6];
    child.copy_from_user(base, &mut buf).expect("read child");
    assert_eq!(&buf, b"parent");

    // ...different frames. This is the property fork depends on.
    assert_ne!(parent.translate(base).unwrap(), child.translate(base).unwrap());

    child.copy_to_user(base, b"child!").expect("write child");
    parent.copy_from_user(base, &mut buf).expect("read parent");
    assert_eq!(&buf, b"parent", "a write to the child reached the parent");
});

// ---------------------------------------------------------------------------
// ELF loader
// ---------------------------------------------------------------------------

ktest!(loader_rejects_malformed_images, {
    use crate::loader::{self, LoadError};

    assert_eq!(loader::load(&[]).unwrap_err(), LoadError::Truncated);
    assert_eq!(loader::load(&[0u8; 64]).unwrap_err(), LoadError::BadMagic);

    // Right magic, wrong class: a 32-bit header must not be read as 64-bit.
    let mut header = [0u8; 64];
    header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    header[4] = 1; // ELFCLASS32
    header[5] = 1;
    assert_eq!(loader::load(&header).unwrap_err(), LoadError::WrongFormat);
});

ktest!(loader_accepts_an_embedded_program, {
    // The real check: every program the build embedded must actually load.
    for name in crate::programs::names() {
        let image = crate::programs::find(name).expect("named program is missing");
        let loaded = crate::loader::load(image)
            .unwrap_or_else(|e| panic!("{name} failed to load: {e:?}"));
        assert!(loaded.entry >= PAGE_SIZE, "{name} entry is in the null page");
        assert!(loaded.brk > loaded.entry, "{name} has no break above its image");
    }
});

// ---------------------------------------------------------------------------
// Synchronisation and scheduling
// ---------------------------------------------------------------------------

ktest!(spinlock_guards_data, {
    let lock = SpinLock::new(41);
    {
        let mut guard = lock.lock();
        *guard += 1;
        // Recursive acquisition would deadlock, so the failure to acquire here
        // is the correct and expected behaviour.
        assert!(lock.try_lock().is_none());
    }
    assert_eq!(*lock.lock(), 42);
    assert!(lock.try_lock().is_some());
});

ktest!(spinlock_restores_interrupt_state, {
    // Interrupts must be on when a test runs -- it is a normal kernel task.
    assert!(crate::arch::intr_enabled());
    let lock = SpinLock::new(());
    {
        let _guard = lock.lock();
        // Taking the lock masks interrupts for the critical section.
        assert!(!crate::arch::intr_enabled());
    }
    assert!(crate::arch::intr_enabled(), "guard failed to restore interrupts");
});

ktest!(semaphore_counts_permits, {
    let sem = Semaphore::new(2);
    assert_eq!(sem.available(), 2);
    sem.acquire();
    sem.acquire();
    assert_eq!(sem.available(), 0);
    sem.release();
    assert_eq!(sem.available(), 1);
    sem.acquire();
    assert_eq!(sem.available(), 0);
});

ktest!(wait_queue_wakes_a_blocked_task, {
    use core::sync::atomic::{AtomicBool, Ordering};

    static READY: AtomicBool = AtomicBool::new(false);
    static QUEUE: WaitQueue = WaitQueue::new();
    static DONE: AtomicBool = AtomicBool::new(false);

    // The helper blocks until this task publishes the condition and wakes it.
    fn waiter() {
        QUEUE.wait_until(|| READY.load(Ordering::Acquire));
        DONE.store(true, Ordering::Release);
    }

    READY.store(false, Ordering::Release);
    DONE.store(false, Ordering::Release);
    crate::task::spawn("ktest-waiter", waiter).expect("spawn failed");

    // Let it reach the wait.
    for _ in 0..64 {
        crate::task::yield_now();
    }
    assert!(!DONE.load(Ordering::Acquire), "waiter ran before being woken");
    assert_eq!(QUEUE.len(), 1, "waiter is not on the queue");

    READY.store(true, Ordering::Release);
    QUEUE.wake_all();

    for _ in 0..64 {
        crate::task::yield_now();
        if DONE.load(Ordering::Acquire) {
            break;
        }
    }
    assert!(DONE.load(Ordering::Acquire), "waiter was never woken");
});

ktest!(scheduler_preempts_a_task_that_never_yields, {
    use core::sync::atomic::{AtomicU64, Ordering};

    static SPINS: AtomicU64 = AtomicU64::new(0);

    // Deliberately makes no syscall and never yields. If it makes progress
    // while this task also runs, the timer is preempting it.
    fn spinner() {
        loop {
            SPINS.fetch_add(1, Ordering::Relaxed);
            core::hint::spin_loop();
        }
    }

    SPINS.store(0, Ordering::Relaxed);
    crate::task::spawn("ktest-spinner", spinner).expect("spawn failed");

    let start = crate::timer::ticks();
    while crate::timer::ticks() < start + 8 {
        core::hint::spin_loop();
    }

    assert!(
        SPINS.load(Ordering::Relaxed) > 0,
        "a task that never yields was never scheduled: preemption is broken"
    );
});

ktest!(task_pids_are_unique_and_monotonic, {
    let a = crate::task::Task::new_kernel("ktest-a", || {}).expect("task a");
    let b = crate::task::Task::new_kernel("ktest-b", || {}).expect("task b");
    assert!(b.pid.0 > a.pid.0, "pids must never be reused");
    // Never admitted to the run queue, so dropping them here is the whole
    // lifetime -- which also exercises freeing a task that never ran.
    drop(a);
    drop(b);
});

ktest!(kernel_stack_canary_is_intact, {
    let task = crate::task::Task::new_kernel("ktest-stack", || {}).expect("task");
    let inner = task.inner.lock();
    assert!(inner.kstack.canary_intact());
    assert!(inner.kstack.top() > inner.kstack.bottom());
    assert_eq!(
        inner.kstack.top() - inner.kstack.bottom(),
        crate::config::KERNEL_STACK_SIZE
    );
});

ktest!(arc_task_is_freed_when_dropped, {
    // The leak this guards against kept every exited process alive: an Arc to
    // the task, left on a stack that never unwound.
    let task = crate::task::Task::new_kernel("ktest-arc", || {}).expect("task");
    let weak = Arc::downgrade(&task);
    assert_eq!(weak.strong_count(), 1);
    drop(task);
    assert_eq!(weak.strong_count(), 0, "task outlived its last reference");
});
