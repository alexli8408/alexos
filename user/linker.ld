/*
 * User programs are static, non-relocatable executables linked at a fixed
 * base. The base is deliberately above zero so that page zero can stay
 * unmapped and a null dereference faults instead of silently working.
 */

OUTPUT_ARCH(riscv)
ENTRY(_start)

BASE_ADDRESS = 0x10000;

SECTIONS
{
    . = BASE_ADDRESS;

    .text : {
        *(.text.entry)
        *(.text .text.*)
    }

    . = ALIGN(4K);
    .rodata : {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    }

    . = ALIGN(4K);
    .data : {
        *(.data .data.*)
        *(.sdata .sdata.*)
    }

    .bss : {
        *(.sbss .sbss.*)
        *(.bss .bss.*)
        *(COMMON)
    }

    /DISCARD/ : { *(.eh_frame) *(.note.*) *(.comment) }
}
