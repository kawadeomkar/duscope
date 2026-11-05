"""Runs a command on a pseudo-terminal that answers it as a terminal would
(P2-BENCH-01: D34's dua).

    python3 bench/ptyrun.py COMMAND [ARG...]

dua exports and imports a snapshot only in its interactive mode, which starts
by asking the terminal where the cursor is (`ESC [ 6 n`) and gives up when no
answer comes ("The cursor position could not be read within a normal
duration"). `script` gives it a pseudo-terminal, but on a bench runner nothing
is on the other side to answer. This gives the command an 80-by-24
pseudo-terminal, answers each cursor-position query with row 1, column 1, reads
and drops what the command writes, and exits with its status; when the command
fails, the end of what it wrote goes to stderr, its escape sequences removed.

Standard library only; runs on Python 3.9 and later, on Linux and macOS.
"""

from __future__ import annotations

import fcntl
import os
import pty
import re
import struct
import sys
import termios
from typing import List, Sequence, Tuple

# What the command may ask and what a terminal answers. dua 2.46 asks only
# where the cursor is.
QUERIES: Tuple[Tuple[bytes, bytes], ...] = ((b"\x1b[6n", b"\x1b[1;1R"),)
ROWS, COLUMNS = 24, 80
# How much of the command's output a failure reports, before its escape
# sequences are removed.
TAIL = 4096
ESCAPE = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")


def answers(data: bytes) -> Tuple[List[bytes], bytes]:
    """The answers to the queries in `data`, in order, and the end of `data`
    that may begin a query the next read completes."""
    out: List[bytes] = []
    at = 0
    while True:
        esc = data.find(b"\x1b", at)
        if esc < 0:
            return out, b""
        rest = data[esc:]
        for query, answer in QUERIES:
            if rest.startswith(query):
                out.append(answer)
                at = esc + len(query)
                break
        else:
            if any(query.startswith(rest) for query, _ in QUERIES):
                return out, rest
            at = esc + 1


def run(argv: Sequence[str]) -> int:
    """Runs `argv` on the pseudo-terminal: its exit status, or 128 plus the
    signal that ended it."""
    pid, fd = pty.fork()
    if pid == 0:
        try:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
            os.execvp(argv[0], list(argv))
        except OSError as error:
            os.write(2, f"ptyrun: {argv[0]}: {error.strerror}\n".encode())
        os._exit(127)
    pending = tail = b""
    while True:
        try:
            chunk = os.read(fd, 65536)
        except OSError:  # EIO, on Linux, once the command has closed the terminal
            break
        if not chunk:
            break
        found, pending = answers(pending + chunk)
        for answer in found:
            os.write(fd, answer)
        tail = (tail + chunk)[-TAIL:]
    os.close(fd)
    _, status = os.waitpid(pid, 0)
    code = os.waitstatus_to_exitcode(status)
    if code < 0:
        code = 128 - code
    if code != 0:
        sys.stderr.write(ESCAPE.sub(b"", tail).decode("utf-8", "replace").strip() + "\n")
    return code


def main(argv: List[str]) -> int:
    if not argv:
        sys.stderr.write("usage: ptyrun.py COMMAND [ARG...]\n")
        return 2
    return run(argv)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
