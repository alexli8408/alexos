//! NS16550A UART driver.
//!
//! This replaces the SBI console once memory management is up. Transmit is
//! polled -- the FIFO drains in well under a scheduling quantum and blocking
//! the writer is simpler than maintaining a tx ring. Receive is interrupt
//! driven through the PLIC, because the alternative is spinning a whole hart
//! waiting for a keystroke.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::config::UART_BASE;
use crate::mm::phys_to_virt;
use crate::sync::SpinLock;

/// Register offsets. The `virt` board wires the 16550 with a one-byte stride.
mod reg {
    /// Receive buffer (read) / transmit holding (write), or divisor low if DLAB.
    pub const RBR_THR_DLL: usize = 0;
    /// Interrupt enable, or divisor high if DLAB.
    pub const IER_DLM: usize = 1;
    /// Interrupt identification (read) / FIFO control (write).
    pub const IIR_FCR: usize = 2;
    /// Line control.
    pub const LCR: usize = 3;
    /// Modem control.
    pub const MCR: usize = 4;
    /// Line status.
    pub const LSR: usize = 5;
}

/// Line status bits.
mod lsr {
    /// A byte is waiting in the receive buffer.
    pub const DATA_READY: u8 = 1 << 0;
    /// The transmit holding register will accept a byte.
    pub const THR_EMPTY: u8 = 1 << 5;
}

/// Line control bits.
mod lcr {
    /// 8 data bits.
    pub const WORD_LEN_8: u8 = 0b11;
    /// Divisor latch access: remaps registers 0 and 1 onto the baud divisor.
    pub const DLAB: u8 = 1 << 7;
}

/// Interrupt enable bits.
mod ier {
    /// Raise an interrupt when a byte arrives.
    pub const RX_AVAILABLE: u8 = 1 << 0;
}

/// FIFO control bits.
mod fcr {
    pub const ENABLE: u8 = 1 << 0;
    pub const CLEAR_RX: u8 = 1 << 1;
    pub const CLEAR_TX: u8 = 1 << 2;
}

/// Serialises access to the device registers.
static UART: SpinLock<Uart> = SpinLock::new(Uart { base: UART_BASE });
static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Bytes received by the interrupt handler, waiting to be read.
static RX_QUEUE: SpinLock<RxRing> = SpinLock::new(RxRing::new());

struct Uart {
    /// Physical base; converted through the direct map on each access so this
    /// stays correct no matter when the kernel page table is switched.
    base: usize,
}

impl Uart {
    #[inline(always)]
    fn reg(&self, offset: usize) -> *mut u8 {
        phys_to_virt(self.base + offset) as *mut u8
    }

    #[inline(always)]
    fn read(&self, offset: usize) -> u8 {
        // SAFETY: `base + offset` is a device register inside the MMIO window
        // that the boot page table maps for the lifetime of the kernel.
        unsafe { self.reg(offset).read_volatile() }
    }

    #[inline(always)]
    fn write(&self, offset: usize, value: u8) {
        // SAFETY: as above.
        unsafe { self.reg(offset).write_volatile(value) };
    }

    /// Configure 8N1 at 38400 baud with FIFOs on and receive interrupts armed.
    fn init(&self) {
        // Mask interrupts while the divisor latch is exposed.
        self.write(reg::IER_DLM, 0x00);

        // QEMU ignores the actual baud rate, but a real 16550 needs a sane
        // divisor and the sequence is the same either way.
        self.write(reg::LCR, lcr::DLAB);
        self.write(reg::RBR_THR_DLL, 0x03); // 38400 baud from a 1.8432 MHz clock
        self.write(reg::IER_DLM, 0x00);

        // Leave DLAB, select 8 bits / no parity / 1 stop bit.
        self.write(reg::LCR, lcr::WORD_LEN_8);

        // Enable and flush both FIFOs.
        self.write(reg::IIR_FCR, fcr::ENABLE | fcr::CLEAR_RX | fcr::CLEAR_TX);

        // DTR + RTS, so anything on the other end sees us as ready.
        self.write(reg::MCR, 0x03);

        self.write(reg::IER_DLM, ier::RX_AVAILABLE);
    }

    /// Block until the transmit register is free, then push one byte.
    fn put(&self, byte: u8) {
        while self.read(reg::LSR) & lsr::THR_EMPTY == 0 {
            core::hint::spin_loop();
        }
        self.write(reg::RBR_THR_DLL, byte);
    }

    /// Take one byte if the receiver has one.
    fn get(&self) -> Option<u8> {
        if self.read(reg::LSR) & lsr::DATA_READY != 0 {
            Some(self.read(reg::RBR_THR_DLL))
        } else {
            None
        }
    }
}

/// Fixed-capacity byte ring for received characters.
///
/// Overflow drops the newest byte rather than overwriting the oldest: a user
/// who types faster than the shell reads would otherwise see their line
/// silently rewritten from the middle.
struct RxRing {
    buf: [u8; Self::CAP],
    head: usize,
    tail: usize,
}

impl RxRing {
    const CAP: usize = 256;

    const fn new() -> Self {
        Self { buf: [0; Self::CAP], head: 0, tail: 0 }
    }

    fn push(&mut self, byte: u8) -> bool {
        let next = (self.tail + 1) % Self::CAP;
        if next == self.head {
            return false;
        }
        self.buf[self.tail] = byte;
        self.tail = next;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            return None;
        }
        let byte = self.buf[self.head];
        self.head = (self.head + 1) % Self::CAP;
        Some(byte)
    }

    fn is_empty(&self) -> bool {
        self.head == self.tail
    }
}

/// Bring the UART up and route console output through it.
pub fn init() {
    UART.lock().init();
    INITIALISED.store(true, Ordering::Release);
    crate::console::use_uart_backend();
}

/// Write a byte slice, translating LF to CRLF so terminals do not stair-step.
pub fn write_bytes(bytes: &[u8]) {
    let uart = UART.lock();
    for &b in bytes {
        if b == b'\n' {
            uart.put(b'\r');
        }
        uart.put(b);
    }
}

/// Pop one received byte, or `None` if nothing has arrived.
pub fn read_byte() -> Option<u8> {
    RX_QUEUE.lock().pop()
}

/// Is at least one byte available to read?
pub fn has_input() -> bool {
    !RX_QUEUE.lock().is_empty()
}

/// PLIC interrupt handler: drain the receive FIFO into the ring.
///
/// Called with interrupts masked. It must not block, so a full ring drops
/// bytes rather than waiting for a reader. Waking blocked readers is the
/// caller's job -- this module deliberately knows nothing about tasks.
///
/// Returns the number of bytes queued, so the dispatcher can skip the wakeup
/// when the interrupt turned out to be spurious.
pub fn handle_interrupt() -> usize {
    let mut queued = 0;
    loop {
        let byte = match UART.lock().get() {
            Some(b) => b,
            None => break,
        };
        if !RX_QUEUE.lock().push(byte) {
            // Ring is full; the reader is not keeping up. Nothing useful to do
            // from interrupt context except drop the byte.
            break;
        }
        queued += 1;
    }
    queued
}

/// Has `init` run? The console backend switch depends on it.
pub fn is_initialised() -> bool {
    INITIALISED.load(Ordering::Acquire)
}
