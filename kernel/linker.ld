/*
 * AlexOS kernel link script.
 *
 * The kernel runs in the upper half of the Sv39 address space. Physical memory
 * is direct-mapped at VIRT_OFFSET, so the kernel image is linked at
 * VIRT_OFFSET + 0x80200000 but must be *loaded* at 0x80200000, where OpenSBI
 * hands off control. Every output section therefore sets an explicit load
 * address (AT) of `vma - VIRT_OFFSET`, which keeps `llvm-objcopy -O binary`
 * emitting a flat image whose offsets match physical memory exactly.
 *
 * Section boundaries are exported as symbols and 4 KiB aligned so that
 * mm::remap can install fine-grained permissions (.text is R-X, .rodata is R--,
 * everything else is RW-) once the real kernel page table is built.
 */

OUTPUT_ARCH(riscv)
ENTRY(_entry)

VIRT_OFFSET  = 0xffffffc000000000;
KERNEL_PHYS  = 0x80200000;
BASE_ADDRESS = 0xffffffc080200000;

SECTIONS
{
    . = BASE_ADDRESS;
    __kernel_start = .;

    .text : AT(ADDR(.text) - VIRT_OFFSET) {
        __text_start = .;
        /* The boot trampoline must be the very first thing in the image: the
           flat binary is loaded at 0x80200000 and OpenSBI jumps straight there. */
        KEEP(*(.text.entry))
        *(.text .text.*)
        . = ALIGN(4K);
        __text_end = .;
    }

    . = ALIGN(4K);
    .rodata : AT(ADDR(.rodata) - VIRT_OFFSET) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)

        /* Kernel unit tests register themselves here at link time; see ktest.rs. */
        . = ALIGN(8);
        __ktest_start = .;
        KEEP(*(.ktest))
        __ktest_end = .;

        . = ALIGN(4K);
        __rodata_end = .;
    }

    . = ALIGN(4K);
    .data : AT(ADDR(.data) - VIRT_OFFSET) {
        __data_start = .;
        *(.data .data.*)
        *(.sdata .sdata.*)
        . = ALIGN(4K);
        __data_end = .;
    }

    . = ALIGN(4K);
    .bss (NOLOAD) : AT(ADDR(.bss) - VIRT_OFFSET) {
        __bss_start = .;
        /* Boot stacks live at the front of .bss so a stack overflow runs into
           the guard region below __bss_start rather than into kernel data. */
        KEEP(*(.bss.stack))
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4K);
        __bss_end = .;
    }

    . = ALIGN(4K);
    __kernel_end = .;

    /DISCARD/ : {
        *(.eh_frame) *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
