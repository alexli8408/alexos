#!/bin/sh
# Run a kernel image under QEMU with a wall-clock limit.
#
# QEMU installs its own SIGALRM handler, so `perl -e 'alarm N'` and similar
# tricks do not stop it; and macOS ships no coreutils `timeout`. This forks a
# watchdog that escalates SIGTERM to SIGKILL instead, which works on both
# Linux CI and a developer's Mac.
#
# Usage: scripts/run-qemu.sh [-t seconds] [-i stdin-text] [-w seconds] -- [qemu args...]
#
# -w delays the input so it lands after the guest has booted. Without it the
# bytes reach the UART's receive FIFO before the driver initialises and are
# discarded by the FIFO reset, which looks exactly like a broken interrupt.
#
# Exits with QEMU's status, or 124 if the watchdog had to intervene, matching
# what `timeout` would have returned.

set -eu

TIMEOUT=30
STDIN_TEXT=""
INPUT_DELAY=0

while [ $# -gt 0 ]; do
    case "$1" in
        -t) TIMEOUT="$2"; shift 2 ;;
        -i) STDIN_TEXT="$2"; shift 2 ;;
        -w) INPUT_DELAY="$2"; shift 2 ;;
        --) shift; break ;;
        *)  break ;;
    esac
done

if [ $# -eq 0 ]; then
    echo "usage: $0 [-t seconds] [-i stdin-text] -- qemu-system-riscv64 [args...]" >&2
    exit 2
fi

# Feed the guest a fixed string when asked, otherwise nothing. Either way stdin
# must be closed rather than inherited, or QEMU will sit waiting on the
# terminal when run from a script.
if [ -n "$STDIN_TEXT" ]; then
    # Keep the pipe open for the whole run: closing it would send EOF, and some
    # guests treat that as the console going away.
    { sleep "$INPUT_DELAY"; printf '%s' "$STDIN_TEXT"; sleep "$TIMEOUT"; } | "$@" &
else
    "$@" < /dev/null &
fi
QEMU_PID=$!

(
    # Poll rather than a single long sleep so a guest that exits on its own is
    # noticed promptly and the watchdog does not linger.
    elapsed=0
    while [ "$elapsed" -lt "$TIMEOUT" ]; do
        kill -0 "$QEMU_PID" 2>/dev/null || exit 0
        sleep 1
        elapsed=$((elapsed + 1))
    done
    # Still alive past the limit: ask nicely, then insist.
    kill -TERM "$QEMU_PID" 2>/dev/null || exit 0
    sleep 1
    kill -KILL "$QEMU_PID" 2>/dev/null || true
) &
WATCHDOG_PID=$!

STATUS=0
wait "$QEMU_PID" 2>/dev/null || STATUS=$?

kill "$WATCHDOG_PID" 2>/dev/null || true
wait "$WATCHDOG_PID" 2>/dev/null || true

# 143 = SIGTERM, 137 = SIGKILL: both mean the watchdog fired.
if [ "$STATUS" -eq 143 ] || [ "$STATUS" -eq 137 ]; then
    echo "[run-qemu] timed out after ${TIMEOUT}s" >&2
    exit 124
fi

exit "$STATUS"
