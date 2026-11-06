# MineHouse parity specs

Flint specs for the mechanics our survival server needs. Each spec mirrors a
GameTest in [minehouse-pumpkin](https://github.com/P5ina/minehouse-pumpkin/tree/master/tools/minehouse-parity)
that passes on the vanilla 26.3 server, so a failure here is a gap in SteelMC,
not in the test.

- `gen_blocks.py` writes `flint/blocks/*.json`, `gen_mobs.py` writes one spec per mob to
  `flint/mobs/*.json`. Edit the specs there and rerun the script.
- `known-gaps.txt` lists specs that fail on SteelMC today.
- `check.py` compares a run with the list; the `MineHouse parity` workflow runs it.

Run locally:

```sh
python3 minehouse-tests/gen_blocks.py
TEST_PATH=$PWD/minehouse-tests/flint cargo test -p steel-flint --profile cicd --lib \
  test_run_flint_selected -- --ignored --nocapture
python3 minehouse-tests/check.py steel-flint/log/flint_summary.json
```

Flint checks a state at a fixed tick. GameTests wait until a condition holds,
so block specs check at the GameTest's tick limit, after the state settles.
Mob behavior happens at a random tick, so mob specs use `within: N` on the
assert, which our flint-core fork (P5ina/flint-core) adds: the assert is
retried every tick until it passes or N ticks have run.

Flint has no `kill` command and no "outside this area" check, so slime,
magma cube and enderman only get the landing spec, not split / teleport.
