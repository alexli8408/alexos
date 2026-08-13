#!/bin/sh
# End-to-end test: boot the kernel, drive the shell over the serial port, and
# check what comes back.
#
# This covers what the in-kernel suite structurally cannot. Those tests run in
# supervisor mode and can never exercise the ELF loader, the user trap path, or
# fork/exec/wait as a user process actually experiences them. Here the kernel is
# a black box and the only interface is the console, which is exactly how a real
# user meets it.

set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

KERNEL=target/riscv64gc-unknown-none-elf/release/alexos.bin
LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

# Build explicitly rather than trusting whatever is on disk. `make test`
# produces a kernel with the ktest feature on, which runs the suite and exits
# instead of starting init -- reusing that image here would silently test
# nothing.
echo "integration: building"
make build > /dev/null

# Newline-separated commands fed to the shell once it is up.
SCRIPT='hello
forktest
echo integration ok
ps
'

echo "integration: booting"
scripts/run-qemu.sh -t 45 -w 3 -i "$SCRIPT" -- \
    qemu-system-riscv64 \
    -machine virt -cpu rv64 -smp 1 -m 128M \
    -nographic -bios default -kernel "$KERNEL" > "$LOG" 2>&1 || true

failed=0

expect() {
    if grep -qF "$1" "$LOG"; then
        echo "  ok      $2"
    else
        echo "  FAILED  $2 (expected to find: $1)"
        failed=1
    fi
}

reject() {
    if grep -qF "$1" "$LOG"; then
        echo "  FAILED  $2 (found: $1)"
        failed=1
    else
        echo "  ok      $2"
    fi
}

echo "integration: checking output"
expect "init: starting"              "init runs as pid 1"
expect "AlexOS shell"                "init forked and exec'd the shell"
expect "hello from userspace!"       "a user program loaded and ran"
expect "forktest: ok"                "fork copied memory instead of sharing it"
expect "integration ok"              "arguments reached the program"
expect "PID   PPID  NAME"             "ps listed the task table"
reject "PANIC"                       "the kernel did not panic"
reject "fatal trap"                  "no unhandled supervisor trap"
reject "Zombie"                      "every child was reaped"

if [ "$failed" -ne 0 ]; then
    echo
    echo "integration: FAILED -- full log follows"
    echo "-----------------------------------------------------------"
    cat "$LOG"
    exit 1
fi

echo "integration: all checks passed"
