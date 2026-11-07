#!/usr/bin/env python3
"""Writes the MineHouse block parity tests as Flint specs.

Same setups as the GameTests in P5ina/minehouse-pumpkin
(tools/minehouse-parity/gen_tests.py), which pass on vanilla 26.3. A GameTest
waits until its condition holds; here every check runs at the GameTest's
max_ticks, after the state has settled.
"""
import json
from pathlib import Path

OUT = Path(__file__).with_name("flint") / "blocks"
SCHEMA = "https://raw.githubusercontent.com/FlintTestMC/flint-core/refs/heads/main/flint-content/test_spec_schema.json"
WIRE_EW = {"east": "side", "west": "side", "north": "none", "south": "none", "power": 0}


def b(id, **props):
    return {"id": f"minecraft:{id}", **props}


def items(*stacks):
    return ", ".join(f'{{Slot: {s}b, count: {c}, id: "minecraft:{i}"}}' for s, i, c in stacks).join("[]")


def dispenser(facing, item):
    return b("dispenser", facing=facing, triggered=False, nbt={"Items": items((0, item, 1))})


class Spec:
    def __init__(self, name, desc, size, ticks, tags):
        self.name, self.desc, self.size, self.ticks, self.tags = name, desc, size, ticks, tags
        self.blocks, self.later, self.checks = [], [], []

    def set(self, pos, block):
        self.blocks.append({"pos": list(pos), "block": block})

    def power(self, pos):
        # the GameTest start block pulses here; a redstone block powers the same spot
        self.later.append({"at": 1, "do": "place", "pos": list(pos), "block": b("redstone_block")})

    def use_block(self, at, pos, rot):
        # the GameTest makes a mock player use the block; here the backing player
        # stands at pos, looks at it and uses it with an empty hand
        self.later.append({"at": at, "do": "tp", "entity_alias": "player", "pos": list(pos), "rot": list(rot)})
        self.later.append({"at": at, "do": "interact"})

    def check(self, pos, block):
        self.checks.append({"pos": list(pos), "is": block})

    def entity(self, type):
        self.checks.append({"is": f"minecraft:{type}", "exists": True})

    def write(self):
        sx, sy, sz = self.size
        timeline = [
            {"at": 0, "do": "fill", "region": [[0, 0, 0], [sx - 1, 0, sz - 1]], "with": b("stone")},
            {"at": 0, "do": "place_each", "blocks": self.blocks},
            *self.later,
            {"at": self.ticks, "do": "assert", "checks": self.checks},
        ]
        ids = sorted({x["block"]["id"] for x in self.blocks} | {"minecraft:stone"})
        spec = {
            "$schema": SCHEMA, "flintVersion": "1.0.0",
            "name": f"minehouse_{self.name}", "description": self.desc,
            "tags": ["minehouse", *self.tags], "minecraftIds": ids, "dependencies": [],
            "setup": {"cleanup": {"region": [[0, 0, 0], [sx - 1, sy - 1, sz - 1]]}},
            "timeline": timeline, "breakpoints": [],
        }
        (OUT / f"{self.name}.json").write_text(json.dumps(spec, indent=2, ensure_ascii=False) + "\n")


def specs():
    s = Spec("redstone_wire_15", "A redstone block powers a 15-long wire line: 15 next to it, 1 at the end.",
             (17, 3, 1), 20, ["redstone"])
    for x in range(1, 16):
        s.set((x, 1, 0), b("redstone_wire", **WIRE_EW))
    s.power((0, 1, 0))
    s.check((1, 1, 0), b("redstone_wire", power=15))
    s.check((15, 1, 0), b("redstone_wire", power=1))
    yield s

    s = Spec("comparator_reads_chest", "A hopper fills a chest; the comparator reading the chest turns on.",
             (5, 4, 1), 40, ["redstone", "container"])
    s.set((0, 1, 0), b("chest", facing="north", type="single", waterlogged=False))
    s.set((0, 2, 0), b("hopper", facing="down", enabled=True,
                       nbt={"Items": items((0, "cobblestone", 64)), "TransferCooldown": 0}))
    s.set((1, 1, 0), b("comparator", facing="west", mode="compare", powered=False))
    s.check((1, 1, 0), b("comparator", powered=True))
    yield s

    s = Spec("piston_pushes_block", "A powered piston pushes a gold block one block east.",
             (4, 3, 3), 40, ["redstone", "piston"])
    s.set((1, 1, 1), b("piston", facing="east", extended=False))
    s.set((2, 1, 1), b("gold_block"))
    s.power((0, 1, 1))
    s.check((3, 1, 1), b("gold_block"))
    yield s

    s = Spec("hopper_fills_chest", "A hopper moves its diamond into the chest below.",
             (1, 4, 1), 40, ["container", "hopper"])
    s.set((0, 1, 0), b("chest", facing="north", type="single", waterlogged=False))
    s.set((0, 2, 0), b("hopper", facing="down", enabled=True,
                       nbt={"Items": items((0, "diamond", 1)), "TransferCooldown": 0}))
    s.check((0, 1, 0), b("chest", nbt={"Items[0].id": "minecraft:diamond"}))
    yield s

    s = Spec("hopper_picks_up_item", "A hopper picks up a dropped emerald and passes it to the chest below.",
             (1, 5, 1), 60, ["container", "hopper", "item-entity"])
    s.set((0, 1, 0), b("chest", facing="north", type="single", waterlogged=False))
    s.set((0, 2, 0), b("hopper", facing="down", enabled=True, nbt={"TransferCooldown": 0}))
    s.later.append({"at": 1, "do": "summon", "entity_alias": "emerald", "entity_type": "minecraft:item",
                    "pos": [0.5, 3.7, 0.5], "Item": '{count: 1, id: "minecraft:emerald"}'})
    s.check((0, 1, 0), b("chest", nbt={"Items[0].id": "minecraft:emerald"}))
    yield s

    s = Spec("furnace_smelts_iron", "A furnace with raw iron and coal smelts one iron ingot.",
             (1, 3, 1), 320, ["container", "furnace"])
    s.set((0, 1, 0), b("furnace", facing="north", lit=False,
                       nbt={"Items": items((0, "raw_iron", 1), (1, "coal", 1))}))
    s.check((0, 1, 0), b("furnace", nbt={"Items[0].id": "minecraft:iron_ingot"}))
    yield s

    s = Spec("dispenser_water_flows_7", "A dispenser pours water into a one-wide channel; it flows 7 blocks.",
             (10, 4, 3), 100, ["dispenser", "fluid"])
    for x in range(10):
        s.set((x, 1, 0), b("glass"))
        s.set((x, 1, 2), b("glass"))
    s.set((0, 1, 1), b("glass"))
    s.set((1, 2, 1), dispenser("down", "water_bucket"))
    s.power((2, 2, 1))
    s.check((8, 1, 1), b("water"))
    yield s

    s = Spec("water_meets_lava_obsidian", "Water flows onto a lava source and turns it into obsidian.",
             (5, 4, 3), 100, ["dispenser", "fluid"])
    for x in range(5):
        s.set((x, 1, 0), b("glass"))
        s.set((x, 1, 2), b("glass"))
    s.set((0, 1, 1), b("glass"))
    s.set((4, 1, 1), b("glass"))
    s.set((1, 2, 1), dispenser("down", "lava_bucket"))
    s.set((3, 2, 1), dispenser("down", "water_bucket"))
    s.power((2, 2, 1))
    s.check((1, 1, 1), b("obsidian"))
    yield s

    s = Spec("tnt_breaks_stone", "Wire ignites TNT; the explosion breaks stone two blocks away.",
             (9, 3, 5), 120, ["redstone", "tnt", "explosion"])
    s.set((1, 1, 1), b("tnt", unstable=False))
    for x in range(2, 8):
        s.set((x, 1, 1), b("redstone_wire", **WIRE_EW))
    s.set((1, 1, 3), b("stone"))
    s.power((8, 1, 1))
    s.check((1, 1, 3), b("air"))
    yield s

    s = Spec("iron_golem_from_blocks", "A dispenser puts a carved pumpkin on an iron T and an iron golem appears.",
             (4, 6, 3), 40, ["dispenser", "golem", "entity"])
    for p in [(1, 1, 1), (1, 2, 1), (0, 2, 1), (2, 2, 1)]:
        s.set(p, b("iron_block"))
    s.set((1, 4, 1), dispenser("down", "carved_pumpkin"))
    s.power((2, 4, 1))
    s.entity("iron_golem")
    yield s

    s = Spec("snow_golem_from_blocks", "A dispenser puts a carved pumpkin on two snow blocks and a snow golem appears.",
             (2, 6, 1), 40, ["dispenser", "golem", "entity"])
    s.set((0, 1, 0), b("snow_block"))
    s.set((0, 2, 0), b("snow_block"))
    s.set((0, 4, 0), dispenser("down", "carved_pumpkin"))
    s.power((1, 4, 0))
    s.entity("snow_golem")
    yield s

    s = Spec("nether_portal_lights", "A dispenser with flint and steel lights an obsidian frame.",
             (4, 7, 2), 40, ["dispenser", "portal"])
    for x in range(4):
        s.set((x, 1, 1), b("obsidian"))
        s.set((x, 5, 1), b("obsidian"))
    for y in range(2, 5):
        s.set((0, y, 1), b("obsidian"))
        s.set((3, y, 1), b("obsidian"))
    s.set((1, 2, 0), dispenser("south", "flint_and_steel"))
    s.power((2, 2, 0))
    s.check((1, 3, 1), b("nether_portal"))
    yield s

    # Flint's backing player is not an entity of the test world, so the viewer
    # recheck 5 ticks after opening drops it; these check before that recheck.
    s = Spec("barrel_opens_when_used", "A player opens a barrel; the barrel shows open.",
             (3, 3, 5), 3, ["container", "player"])
    s.set((1, 1, 1), b("barrel", facing="up", open=False))
    s.use_block(1, (1.5, 1, 3.5), (180, 29))
    s.check((1, 1, 1), b("barrel", open=True))
    yield s

    s = Spec("trapped_chest_powers_when_opened", "A player opens a trapped chest; it powers the lamp beside it.",
             (4, 3, 5), 3, ["container", "player", "redstone"])
    s.set((1, 1, 1), b("trapped_chest", facing="south", type="single", waterlogged=False))
    s.set((2, 1, 1), b("redstone_lamp", lit=False))
    s.use_block(1, (1.5, 1, 3.5), (180, 29))
    s.check((2, 1, 1), b("redstone_lamp", lit=True))
    yield s


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    n = 0
    for s in specs():
        s.write()
        n += 1
    print(f"wrote {n} specs to {OUT}")
