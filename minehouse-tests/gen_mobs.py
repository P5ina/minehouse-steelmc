#!/usr/bin/env python3
"""Writes one Flint spec per mob (every spawn-egg mob, plus giant and illusioner).

Same scenarios as tools/minehouse-parity/mobs.py in P5ina/minehouse-pumpkin,
which pass on vanilla 26.3. Mob behavior happens at a random tick, so the
checks use `within` (our flint-core fork): retried every tick until they pass.

Flint has no `kill` command and no "outside this area" check, so slime,
magma cube and enderman get the landing test instead of split / teleport.
"""
import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve()
OUT = ROOT.with_name("flint") / "mobs"
ENTITIES = {e["name"]: e for e in json.loads(
    (ROOT.parents[1] / "steel-registry/build_assets/entities.json").read_text())}
SCHEMA = "https://raw.githubusercontent.com/FlintTestMC/flint-core/refs/heads/main/flint-content/test_spec_schema.json"


def b(id, **props):
    return {"id": f"minecraft:{id}", **props}


def items(item):
    return f'[{{Slot: 0b, count: 1, id: "minecraft:{item}"}}]'


class Spec:
    """A glass pen with interior x/z in 1..w, y from 1; stone floor at y=0."""

    def __init__(self, mob, desc, inner, walls=3, roof=False, extra_x=0):
        self.mob, self.desc, self.w, self.walls, self.roof = mob, desc, inner, walls, roof
        self.sx = inner + 2 + extra_x
        self.sz = inner + 2
        self.blocks, self.timeline = [], []
        for x in range(self.sx):
            for z in range(self.sz):
                edge = x in (0, inner + 1) or z in (0, inner + 1)
                if x > inner + 1:
                    continue
                if edge:
                    for y in range(1, walls + 1):
                        self.set((x, y, z), b("glass"))
                if roof:
                    self.set((x, walls + 1, z), b("glass"))

    @property
    def center(self):
        return [1 + self.w / 2, 1.0, 1 + self.w / 2]

    def set(self, pos, block):
        self.blocks.append({"pos": list(pos), "block": block})

    def fill(self, block, y_to):
        for x in range(1, self.w + 1):
            for z in range(1, self.w + 1):
                for y in range(1, y_to + 1):
                    self.set((x, y, z), block)

    def summon(self, alias, mob, pos, at=1, **nbt):
        self.timeline.append({"at": at, "do": "summon", "entity_alias": alias,
                              "entity_type": f"minecraft:{mob}", "pos": pos,
                              "PersistenceRequired": "1b", **nbt})

    def place(self, pos, block, at=2):
        self.timeline.append({"at": at, "do": "place", "pos": list(pos), "block": block})

    def expect(self, checks, ticks, at=2):
        self.timeline.append({"at": at, "do": "assert", "checks": checks, "within": ticks})

    def write(self):
        top = self.walls + (2 if self.roof else 1) + 4
        timeline = [
            {"at": 0, "do": "fill", "region": [[0, 0, 0], [self.sx - 1, 0, self.sz - 1]], "with": b("stone")},
            {"at": 0, "do": "place_each", "blocks": self.blocks},
            *sorted(self.timeline, key=lambda e: e["at"]),
        ]
        spec = {
            "$schema": SCHEMA, "flintVersion": "1.0.0",
            "name": f"minehouse_mob_{self.mob}", "description": self.desc,
            "tags": ["minehouse", "mob"], "minecraftIds": [f"minecraft:{self.mob}"], "dependencies": [],
            "setup": {"cleanup": {"region": [[0, 0, 0], [self.sx - 1, top, self.sz - 1]]}},
            "timeline": timeline, "breakpoints": [],
        }
        (OUT / f"mob_{self.mob}.json").write_text(json.dumps(spec, indent=2, ensure_ascii=False) + "\n")


def pen(mob, desc, walls=3, roof=False, extra=0):
    inner = max(3, math.ceil(ENTITIES[mob]["width"]) + 2) + extra
    return Spec(mob, desc, inner, walls=max(walls, 2), roof=roof)


def specs():
    # fall: summoned 4 blocks up, ends up standing on the floor
    for mob in [
        "dolphin", "turtle", "polar_bear", "sniffer", "mule", "skeleton_horse", "zombie_horse",
        "camel_husk", "wandering_trader", "witch", "wither_skeleton", "zombified_piglin",
        "illusioner", "giant", "spider", "cave_spider", "endermite", "silverfish", "warden",
        "breeze", "creaking", "parched", "sulfur_cube", "nautilus", "zombie_nautilus",
        "elder_guardian", "goat", "slime", "magma_cube", "enderman",
    ]:
        s = pen(mob, f"A {mob} summoned 4 blocks up falls and lands on the floor.", walls=2)
        cx, _, cz = s.center
        s.summon("mob", mob, [cx, 5.0, cz], NoAI="0b")
        s.expect([{"entity_alias": "mob", "pos": [cx, 1.0, cz], "position_tolerance": 0.8}], 80)
        yield s

    # exists: flying or huge mobs stay after summoning
    for mob in ["parrot", "bat", "allay", "vex", "blaze", "ghast", "happy_ghast", "phantom", "shulker",
                "ender_dragon", "wither"]:
        s = pen(mob, f"A {mob} can be summoned and stays in the world.", walls=2)
        s.summon("mob", mob, s.center, NoAI="1b")
        s.timeline.append({"at": 40, "do": "assert", "checks": [{"entity_alias": "mob", "exists": True}]})
        yield s

    # breed: two adults in love make a baby
    breeders = {
        "cow": {}, "chicken": {}, "rabbit": {}, "fox": {}, "bee": {}, "strider": {}, "camel": {},
        "armadillo": {}, "axolotl": {}, "ocelot": {}, "panda": {},
        "horse": {"Tame": "1b"}, "donkey": {"Tame": "1b"}, "llama": {"Tame": "1b"}, "trader_llama": {"Tame": "1b"},
    }
    for mob, nbt in breeders.items():
        s = pen(mob, f"Two {mob}s in love make a baby.", roof=mob == "bee", extra=2 if mob == "panda" else 0)
        cx, _, cz = s.center
        s.summon("a", mob, [cx - 0.6, 1.0, cz], InLove="600", Age="0", **nbt)
        s.summon("b", mob, [cx + 0.6, 1.0, cz], InLove="600", Age="0", **nbt)
        if mob == "panda":
            # pandas only breed with bamboo nearby: a row along the back wall
            for x in range(1, s.w + 1):
                s.set((x, 0, s.w), b("dirt"))
                s.set((x, 1, s.w), b("bamboo", age=0, leaves="none", stage=0))
        s.expect([{"is": f"minecraft:{mob}", "count": 3}], 300)
        yield s

    # lightning conversions
    for mob, check in [
        ("pig", {"is": "minecraft:zombified_piglin"}),
        ("villager", {"is": "minecraft:witch"}),
        ("creeper", {"is": "minecraft:creeper", "powered": "1b"}),
    ]:
        s = pen(mob, f"Lightning converts a {mob}.")
        s.summon("mob", mob, s.center, NoAI="1b")
        s.summon("bolt", "lightning_bolt", s.center, at=2)
        s.expect([check], 60, at=3)
        yield s

    # Nether mobs convert in the Overworld
    for mob, into in [("piglin", "zombified_piglin"), ("piglin_brute", "zombified_piglin"), ("hoglin", "zoglin")]:
        s = pen(mob, f"A {mob} in the Overworld turns into a {into}.")
        s.summon("mob", mob, s.center)
        s.expect([{"is": f"minecraft:{into}"}], 500)
        yield s

    # conversions in water / powder snow, tadpole growth
    for mob, into, block, ticks in [
        ("zombie", "drowned", "water", 1000), ("husk", "zombie", "water", 1000),
        ("skeleton", "stray", "powder_snow", 800),
    ]:
        s = pen(mob, f"A {mob} in {block.replace('_', ' ')} turns into a {into}.", walls=4)
        s.fill(b(block), 3)
        s.summon("mob", mob, s.center)
        s.expect([{"is": f"minecraft:{into}"}], ticks)
        yield s

    s = pen("tadpole", "A tadpole about to grow up turns into a frog.")
    s.fill(b("water"), 2)
    s.summon("mob", "tadpole", s.center, Age="23980")
    s.expect([{"is": "minecraft:frog"}], 80)
    yield s

    # shear with a dispenser
    for mob, check in [
        ("sheep", {"is": "minecraft:item", "Item.id": "minecraft:white_wool"}),
        ("mooshroom", {"is": "minecraft:cow"}),
        ("bogged", {"is": "minecraft:item"}),
        ("snow_golem", {"is": "minecraft:item", "Item.id": "minecraft:carved_pumpkin"}),
    ]:
        s = pen(mob, f"A dispenser with shears shears a {mob}.")
        mx = 1 + s.w // 2
        s.set((mx, 1, 0), b("dispenser", facing="south", triggered=False, nbt={"Items": items("shears")}))
        s.set((mx, 2, 0), b("glass"))
        s.summon("mob", mob, [mx + 0.5, 1.0, 1.5], NoAI="1b")
        s.place((mx, 2, 0), b("redstone_block"))
        s.expect([check], 60, at=3)
        yield s

    # burns to death in daylight (worlds start at day, clear weather)
    for mob in ["drowned", "stray", "zombie_villager"]:
        s = pen(mob, f"A {mob} burns to death in daylight.", roof=True)
        s.summon("mob", mob, s.center)
        s.expect([{"entity_alias": "mob", "exists": False}], 1200)
        yield s

    # attacks: the attacker kills the target
    for mob, target, attacker_nbt, target_nbt, water in [
        ("iron_golem", "zombie", {}, {}, False),
        ("zoglin", "cow", {}, {}, False),
        ("vindicator", "villager", {"equipment": '{mainhand: {id: "minecraft:iron_axe"}}'}, {"NoAI": "1b"}, False),
        ("pillager", "villager", {"equipment": '{mainhand: {id: "minecraft:crossbow"}}'}, {"NoAI": "1b"}, False),
        ("ravager", "villager", {}, {"NoAI": "1b"}, False),
        ("guardian", "squid", {}, {}, True),
        ("frog", "slime", {}, {"Size": "0"}, False),
        ("wolf", "sheep", {}, {}, False),
        ("cat", "rabbit", {}, {}, False),
    ]:
        big = max(ENTITIES[mob]["width"], ENTITIES[target]["width"])
        s = Spec(mob, f"A {mob} kills a {target}.", max(4, math.ceil(big) + 3) + 1, walls=4, roof=True)
        if water:
            s.fill(b("water"), 4)
        s.summon("attacker", mob, [1.5, 1.0, 1.5], **attacker_nbt)
        s.summon("target", target, [s.w + 0.5, 1.0, s.w + 0.5], **target_nbt)
        s.expect([{"entity_alias": "target", "exists": False}], 1500)
        yield s

    # fish and squid die out of water
    for mob in ["cod", "salmon", "tropical_fish", "pufferfish", "squid", "glow_squid"]:
        s = pen(mob, f"A {mob} out of water dies.", walls=2, roof=True)
        s.summon("mob", mob, s.center)
        s.expect([{"entity_alias": "mob", "exists": False}], 900)
        yield s

    s = Spec("evoker", "An evoker near a villager summons vexes.", 6, walls=4, roof=True)
    s.summon("mob", "evoker", [1.5, 1.0, 1.5])
    s.summon("villager", "villager", [6.5, 1.0, 6.5], NoAI="1b")
    s.expect([{"is": "minecraft:vex"}], 400)
    yield s

    s = Spec("copper_golem", "A dispenser puts a carved pumpkin on a copper block and a copper golem appears.", 3, walls=2)
    s.set((2, 1, 2), b("copper_block"))
    s.set((2, 3, 2), b("dispenser", facing="down", triggered=False, nbt={"Items": items("carved_pumpkin")}))
    s.place((2, 4, 2), b("redstone_block"), at=1)
    s.expect([{"is": "minecraft:copper_golem"}], 60)
    yield s


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    names = []
    for s in specs():
        s.write()
        names.append(s.mob)
    assert len(names) == len(set(names)), "duplicate mob"
    print(f"wrote {len(names)} specs to {OUT}")
