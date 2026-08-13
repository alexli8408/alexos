# AlexOS build and run targets.
#
#   make run      boot the kernel in QEMU (ctrl-a x to quit)
#   make debug    boot halted, waiting for gdb on :1234
#   make gdb      attach gdb to a `make debug` session
#   make test     run the in-kernel test suite, exit non-zero on failure
#   make sym ADDR=0x...   resolve a backtrace address to a symbol

TARGET      := riscv64gc-unknown-none-elf
PROFILE     := release
KERNEL_ELF  := target/$(TARGET)/$(PROFILE)/alexos
KERNEL_BIN  := $(KERNEL_ELF).bin
FS_IMG      := fs.img

CPUS        ?= 1
MEM         ?= 128M

OBJCOPY     := rust-objcopy --binary-architecture=riscv64
OBJDUMP     := rust-objdump --arch-name=riscv64
NM          := rust-nm

# -bios default loads the OpenSBI build shipped with QEMU, which hands off to
# our image at 0x80200000. The kernel is passed as a flat binary because that
# address is fixed by the firmware, not read from ELF headers.
QEMU_ARGS := \
	-machine virt \
	-cpu rv64 \
	-smp $(CPUS) \
	-m $(MEM) \
	-nographic \
	-bios default \
	-kernel $(KERNEL_BIN)

.PHONY: all build run debug gdb test clean fmt clippy check size dump sym help

all: build

build: $(KERNEL_BIN)

# User programs must be built and staged before the kernel, whose build script
# embeds whatever it finds in user/build/.
$(KERNEL_BIN): user FORCE
	cargo build -p alexos-kernel --$(PROFILE)
	$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $@

.PHONY: user
user:
	@cargo build -p alexos-user --$(PROFILE) 2>&1 | grep -v '^$$' || true
	@mkdir -p user/build
	@rm -f user/build/*
	@for prog in $$(ls user/src/bin | sed 's/\.rs$$//'); do \
		cp target/$(TARGET)/$(PROFILE)/$$prog user/build/$$prog; \
	done
	@echo "staged $$(ls user/build | wc -l | tr -d ' ') user programs"

FORCE:

run: build
	@echo "== AlexOS on qemu-system-riscv64 (ctrl-a x to exit) =="
	qemu-system-riscv64 $(QEMU_ARGS)

debug: build
	@echo "== halted, waiting for gdb on :1234 =="
	qemu-system-riscv64 $(QEMU_ARGS) -s -S

gdb:
	riscv64-elf-gdb $(KERNEL_ELF) -ex 'target remote :1234'

# The kernel writes to the SiFive test finisher to set QEMU's exit code, so a
# failing assertion fails the build rather than just printing.
test:
	cargo build -p alexos-kernel --$(PROFILE) --features exit-on-panic
	$(OBJCOPY) $(KERNEL_ELF) --strip-all -O binary $(KERNEL_BIN)
	qemu-system-riscv64 $(QEMU_ARGS) -append ktest

check:
	cargo check -p alexos-kernel --$(PROFILE)

fmt:
	cargo fmt --all

clippy:
	cargo clippy -p alexos-kernel --$(PROFILE) -- -D warnings

size: build
	@rust-size $(KERNEL_ELF)
	@echo "flat image: $$(wc -c < $(KERNEL_BIN)) bytes"

dump: build
	$(OBJDUMP) -d $(KERNEL_ELF) | less

# Resolve an address from a kernel backtrace to the nearest preceding symbol.
sym:
	@test -n "$(ADDR)" || { echo "usage: make sym ADDR=0xffffffc0802...."; exit 1; }
	@$(NM) -n $(KERNEL_ELF) | awk -v a=$$(printf '%d' $(ADDR)) \
		'{ addr = strtonum("0x" $$1); if (addr <= a) { last=$$3; lastaddr=addr } } \
		 END { printf "%s + %#x\n", last, a - lastaddr }'

clean:
	cargo clean
	rm -rf $(KERNEL_BIN) $(FS_IMG) user/build

help:
	@grep -E '^#   ' Makefile | sed 's/^#   //'
