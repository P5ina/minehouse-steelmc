#!/usr/bin/env python3
"""Drive one benchmark run: N bots, spread on a grid, sample tick time.

Usage: bench.py FIFO PORT COUNT MINUTES TAG
"""
import subprocess, sys, time, os

fifo, port, count, minutes, tag = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), float(sys.argv[4]), sys.argv[5]
here = os.path.dirname(os.path.abspath(__file__))


def cmd(c):
    with open(fifo, "w") as f:
        f.write(c + "\n")
    print(time.strftime("%H:%M:%S"), ">", c, flush=True)
    time.sleep(0.3)


bots = subprocess.Popen([sys.executable, os.path.join(here, "bot.py"), "127.0.0.1", str(port), str(count), "b"],
                        stdout=open(os.path.join(here, f"bots-{tag}.log"), "w"), stderr=subprocess.STDOUT)
time.sleep(count * 1.0 + 10)
side = 5
for i in range(count):
    x = (i % side - side // 2) * 600
    z = (i // side - side // 2) * 600
    cmd(f"gamemode creative b{i:02d}")
    cmd(f"tp b{i:02d} {x} 100 {z}")
start = time.time()
while time.time() - start < minutes * 60:
    time.sleep(30)
    print(time.strftime("%H:%M:%S"), "t+%ds" % (time.time() - start), flush=True)
    cmd("tick query")
    cmd("execute if entity @e[type=!player]")
    cmd("list")
bots.terminate()
