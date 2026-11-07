#!/usr/bin/env python3
"""Run a server inside a pty; feed lines from a FIFO into its console, log its output.

Usage: ptyrun.py FIFO LOG CWD -- cmd args...
"""
import os, pty, sys, select, re

fifo, log, cwd = sys.argv[1], sys.argv[2], sys.argv[3]
cmd = sys.argv[sys.argv.index("--") + 1:]
if os.path.exists(fifo):
    os.remove(fifo)
os.mkfifo(fifo)
pid, fd = pty.fork()
if pid == 0:
    os.chdir(cwd)
    os.environ["TERM"] = "dumb"
    os.execvp(cmd[0], cmd)
ffd = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
keep = os.open(fifo, os.O_WRONLY)  # keep the FIFO open so reads never see EOF
out = open(log, "ab", buffering=0)
ansi = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")
buf = b""
while True:
    r, _, _ = select.select([fd, ffd], [], [], 1.0)
    if fd in r:
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        out.write(ansi.sub(b"", data).replace(b"\r", b""))
    if ffd in r:
        buf += os.read(ffd, 65536)
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            for ch in line:
                os.write(fd, bytes([ch]))
            os.write(fd, b"\r")
_, status = os.waitpid(pid, 0)
out.write(b"\n[ptyrun] exited %d\n" % status)
