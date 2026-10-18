#!/usr/bin/env python3
"""A pty between a terminal and a program that holds some of the terminal's cursor position
reports (CPR, `ESC [ row ; col R`) for a while: every EVERY-th one (the session's first never)
is held DELAY seconds; a report behind a held one waits for it, so the answers stay in order, as
a terminal's do; every other byte passes at once. The window size follows the outer terminal
(SIGWINCH). Each held report is logged to LOG.

OPT-IN, MANUAL, NEVER IN CI. It reproduces on a real terminal the late cursor answer of
TUI-VERIFY §4.5 / DIVERGENCES X-81 without starving the process: a DELAY over crossterm's 2 s
makes that query time out and its answer come late. Wrap the binary and point the tmux suite at
the wrapper:

    printf '#!/bin/bash\nexec python3 -I %s 20 2.5 /tmp/held.log -- %s "$@"\n' \
        "$PWD/scripts/dsr-delay.py" "$PWD/target/debug/iota" > /tmp/iota-dsr-delay
    chmod +x /tmp/iota-dsr-delay
    IOTA_TMUX=1 IOTA_TMUX_BIN=/tmp/iota-dsr-delay cargo test --test ui_tmux tmux_resize_mid_stream_and_idle

usage: dsr-delay.py EVERY DELAY LOG -- program args...
"""
import fcntl
import os
import re
import select
import signal
import sys
import termios
import time
import tty

every, delay, log = int(sys.argv[1]), float(sys.argv[2]), sys.argv[3]
argv = sys.argv[sys.argv.index("--") + 1:]

pid, master = os.forkpty()
if pid == 0:
    os.execvp(argv[0], argv)


def winsize():
    try:
        ws = fcntl.ioctl(0, termios.TIOCGWINSZ, b"\0" * 8)
        fcntl.ioctl(master, termios.TIOCSWINSZ, ws)
        os.kill(pid, signal.SIGWINCH)
    except OSError:
        pass


winsize()
signal.signal(signal.SIGWINCH, lambda *_: winsize())
old = termios.tcgetattr(0)
tty.setraw(0)
CPR = re.compile(rb"\x1b\[\d+;\d+R")
buf = b""
held = []  # (release time, bytes)
seen = 0
logf = open(log, "a")
try:
    while True:
        now = time.monotonic()
        while held and held[0][0] <= now:
            os.write(master, held.pop(0)[1])
        timeout = max(0.0, held[0][0] - now) if held else None
        try:
            r, _, _ = select.select([0, master], [], [], timeout)
        except InterruptedError:
            continue
        if master in r:
            try:
                data = os.read(master, 65536)
            except OSError:
                break
            if not data:
                break
            os.write(1, data)
        if 0 in r:
            data = os.read(0, 65536)
            if not data:
                break
            buf += data
            out = b""
            while True:
                m = CPR.search(buf)
                esc = buf.find(b"\x1b")
                if m is None:
                    # keep a possible partial CPR tail, pass the rest
                    if esc >= 0 and re.fullmatch(rb"\x1b(\[[\d;]*)?", buf[esc:]):
                        out += buf[:esc]
                        buf = buf[esc:]
                    else:
                        out += buf
                        buf = b""
                    break
                out += buf[: m.start()]
                seen += 1
                if seen > 1 and seen % every == 0:
                    if out:
                        os.write(master, out)
                        out = b""
                    held.append((time.monotonic() + delay, m.group()))
                    logf.write(f"held CPR #{seen} {m.group()!r} for {delay}s\n")
                    logf.flush()
                elif held:
                    # answers stay in order: one behind a held one waits for it
                    if out:
                        os.write(master, out)
                        out = b""
                    held.append((held[-1][0], m.group()))
                else:
                    out += m.group()
                buf = buf[m.end():]
            if out:
                os.write(master, out)
finally:
    termios.tcsetattr(0, termios.TCSADRAIN, old)
    try:
        _, status = os.waitpid(pid, 0)
        code = os.waitstatus_to_exitcode(status)
    except ChildProcessError:
        code = 0
    sys.exit(code if code >= 0 else 1)
