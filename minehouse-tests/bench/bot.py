#!/usr/bin/env python3
"""Minimal offline 26.3 (protocol 777) bot swarm for load testing.

Usage: bot.py HOST PORT COUNT [PREFIX]
Bots log in, answer keep-alives/pings/teleports/chunk batches, and send a
position packet every tick (like a real idle client).
"""
import asyncio, json, struct, sys, zlib, uuid, os, time

PK = json.load(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "steel-registry", "build_assets", "packets.json")))
SB = {st: {n: i for i, n in enumerate(v)} for st, v in PK["serverbound"].items()}
CB = {st: {i: n for i, n in enumerate(v)} for st, v in PK["clientbound"].items()}
PROTO = PK["version"]


def varint(v):
    v &= 0xFFFFFFFF
    out = bytearray()
    while True:
        b = v & 0x7F
        v >>= 7
        if v:
            out.append(b | 0x80)
        else:
            out.append(b)
            return bytes(out)


def mcstr(s):
    b = s.encode()
    return varint(len(b)) + b


def read_varint_buf(buf, i):
    n = s = 0
    while True:
        b = buf[i]
        i += 1
        n |= (b & 0x7F) << s
        s += 7
        if not b & 0x80:
            break
    if n & (1 << 31):
        n -= 1 << 32
    return n, i


class Bot:
    def __init__(self, host, port, name, stats):
        self.host, self.port, self.name, self.stats = host, port, name, stats
        self.threshold = -1
        self.state = "login"
        self.pos = None
        self.alive = False

    async def read_varint(self):
        n = s = 0
        while True:
            b = (await self.r.readexactly(1))[0]
            n |= (b & 0x7F) << s
            s += 7
            if not b & 0x80:
                return n

    async def read_packet(self):
        ln = await self.read_varint()
        data = await self.r.readexactly(ln)
        if self.threshold >= 0:
            dl, i = read_varint_buf(data, 0)
            data = zlib.decompress(data[i:]) if dl else data[i:]
        pid, i = read_varint_buf(data, 0)
        return pid, data[i:]

    def send(self, name, payload=b""):
        body = varint(SB[self.state][name]) + payload
        if self.threshold >= 0:
            if len(body) >= self.threshold:
                body = varint(len(body)) + zlib.compress(body)
            else:
                body = varint(0) + body
        self.w.write(varint(len(body)) + body)

    def client_info(self):
        return (mcstr("en_us") + struct.pack(">b", 10) + varint(0) + b"\x01" + b"\x7f"
                + varint(1) + b"\x00" + b"\x01" + varint(0))

    async def run(self):
        self.r, self.w = await asyncio.open_connection(self.host, self.port)
        self.state = "handshake"
        self.send("intention", varint(PROTO) + mcstr(self.host) + struct.pack(">H", self.port) + varint(2))
        self.state = "login"
        u = uuid.uuid3(uuid.NAMESPACE_OID, "OfflinePlayer:" + self.name).bytes
        self.send("hello", mcstr(self.name) + u)
        await self.w.drain()
        ticker = None
        try:
            while True:
                pid, d = await self.read_packet()
                name = CB[self.state].get(pid)
                if self.state == "login":
                    if name == "login_compression":
                        self.threshold, _ = read_varint_buf(d, 0)
                    elif name == "login_finished":
                        self.send("login_acknowledged")
                        self.state = "config"
                        self.send("client_information", self.client_info())
                    elif name == "login_disconnect":
                        print(self.name, "login disconnect", d[:200]); return
                elif self.state == "config":
                    if name == "select_known_packs":
                        self.send("select_known_packs", d)
                    elif name == "keep_alive":
                        self.send("keep_alive", d)
                    elif name == "ping":
                        self.send("pong", d)
                    elif name == "code_of_conduct":
                        self.send("accept_code_of_conduct")
                    elif name == "finish_configuration":
                        self.send("finish_configuration")
                        self.state = "play"
                        self.alive = True
                        self.stats["joined"] += 1
                        ticker = asyncio.create_task(self.tick())
                    elif name == "disconnect":
                        print(self.name, "config disconnect", d[:200]); return
                else:
                    if name == "keep_alive":
                        self.send("keep_alive", d)
                    elif name == "ping":
                        self.send("pong", d)
                    elif name == "player_position":
                        tid, i = read_varint_buf(d, 0)
                        x, y, z = struct.unpack(">ddd", d[i:i + 24])
                        yaw, pitch = struct.unpack(">ff", d[i + 48:i + 56])
                        flags = struct.unpack(">i", d[i + 56:i + 60])[0]
                        if flags & 1 and self.pos: x += self.pos[0]
                        if flags & 2 and self.pos: y += self.pos[1]
                        if flags & 4 and self.pos: z += self.pos[2]
                        self.pos = [x, y, z]
                        self.send("accept_teleportation", varint(tid) + struct.pack(">dddff", x, y, z, yaw, pitch))
                        if not self.stats.get("loaded_" + self.name):
                            self.stats["loaded_" + self.name] = 1
                            self.send("player_loaded")
                    elif name == "chunk_batch_finished":
                        self.send("chunk_batch_received", struct.pack(">f", 20.0))
                    elif name == "set_health":
                        hp = struct.unpack(">f", d[:4])[0]
                        if hp <= 0:
                            self.stats["deaths"] += 1
                            self.send("client_command", varint(0))
                    elif name == "disconnect":
                        print(self.name, "kicked", d[:300]); return
                await self.w.drain()
        except (asyncio.IncompleteReadError, ConnectionError) as e:
            print(self.name, "connection lost", e)
        finally:
            self.alive = False
            self.stats["joined"] -= 1 if self.state == "play" else 0
            if ticker:
                ticker.cancel()

    async def tick(self):
        while True:
            await asyncio.sleep(0.05)
            if self.pos:
                self.send("move_player_pos", struct.pack(">dddB", *self.pos, 0))
            self.send("client_tick_end")
            try:
                await self.w.drain()
            except ConnectionError:
                return


async def main():
    host, port, count = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    prefix = sys.argv[4] if len(sys.argv) > 4 else "bot"
    stats = {"joined": 0, "deaths": 0}
    tasks = []
    for i in range(count):
        tasks.append(asyncio.create_task(Bot(host, port, f"{prefix}{i:02d}", stats).run()))
        await asyncio.sleep(1.0)
    while True:
        print(time.strftime("%H:%M:%S"), "online", stats["joined"], "deaths", stats["deaths"], flush=True)
        await asyncio.sleep(30)


asyncio.run(main())
