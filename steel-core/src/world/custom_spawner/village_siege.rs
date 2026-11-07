//! Vanilla `VillageSiege`: on one night in ten, zombies march into a village.

use std::f32::consts::TAU;
use std::sync::Arc;

use glam::DVec3;
use rand::{Rng, RngExt as _};
use steel_math::trig::{cos, sin};
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_entities::ZOMBIE;
use steel_utils::{BlockPos, Identifier};

use super::CustomSpawner;
use crate::chunk::heightmap::HeightmapType;
use crate::entity::entities::mobs::hostile::ZombieEntity;
use crate::entity::{Entity as _, EntitySpawnReason, Monster, create_entity_instance};
use crate::world::World;

/// Vanilla `ClockTimeMarkers.ROLL_VILLAGE_SIEGE`: midnight on the overworld clock.
const ROLL_VILLAGE_SIEGE: Identifier = Identifier::vanilla_static("roll_village_siege");
/// Vanilla `random.nextInt(10) == 0`: one night in ten.
const SIEGE_ROLL: i32 = 10;
/// Attempts at a siege origin, and at a zombie position around it.
const POSITION_ATTEMPTS: usize = 10;
/// The siege starts 32 blocks from the player.
const SIEGE_DISTANCE: f32 = 32.0;
const ZOMBIES_PER_SIEGE: i32 = 20;
/// Ticks between two zombies.
const SPAWN_INTERVAL: i32 = 2;
/// Zombies land `nextInt(16) - 8` blocks from the siege origin on each axis.
const ZOMBIE_SPREAD: i32 = 16;

/// Vanilla `VillageSiege.State`. `SIEGE_CAN_ACTIVATE` is never entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SiegeState {
    SiegeTonight,
    SiegeDone,
}

/// Vanilla `VillageSiege`.
pub(crate) struct VillageSiege {
    has_setup_siege: bool,
    pub(super) siege_state: SiegeState,
    pub(super) zombies_to_spawn: i32,
    next_spawn_time: i32,
    pub(super) spawn_pos: BlockPos,
}

impl Default for VillageSiege {
    fn default() -> Self {
        Self {
            has_setup_siege: false,
            siege_state: SiegeState::SiegeDone,
            zombies_to_spawn: 0,
            next_spawn_time: 0,
            spawn_pos: BlockPos::new(0, 0, 0),
        }
    }
}

impl CustomSpawner for VillageSiege {
    fn tick(&mut self, world: &Arc<World>, spawn_enemies: bool) {
        let mut random = rand::rng();
        self.tick_with(world, spawn_enemies, &mut random);
    }
}

impl VillageSiege {
    /// Vanilla `VillageSiege.tick` with an explicit random source.
    pub(super) fn tick_with(
        &mut self,
        world: &Arc<World>,
        spawn_enemies: bool,
        random: &mut impl Rng,
    ) {
        if world.is_bright_outside() || !spawn_enemies {
            self.siege_state = SiegeState::SiegeDone;
            self.has_setup_siege = false;
            return;
        }
        if let Some(clock) = world.dimension_type.default_clock
            && world.is_clock_at_time_marker(clock, &ROLL_VILLAGE_SIEGE)
        {
            self.siege_state = if random.random_range(0..SIEGE_ROLL) == 0 {
                SiegeState::SiegeTonight
            } else {
                SiegeState::SiegeDone
            };
        }
        if self.siege_state == SiegeState::SiegeDone {
            return;
        }
        if !self.has_setup_siege {
            if !self.try_to_setup_siege(world, random) {
                return;
            }
            self.has_setup_siege = true;
        }
        if self.next_spawn_time > 0 {
            self.next_spawn_time -= 1;
            return;
        }
        self.next_spawn_time = SPAWN_INTERVAL;
        if self.zombies_to_spawn > 0 {
            self.try_spawn(world, random);
            self.zombies_to_spawn -= 1;
        } else {
            self.siege_state = SiegeState::SiegeDone;
        }
    }

    /// Vanilla `tryToSetupSiege`: the first non-spectator player standing in a
    /// village outside `#without_zombie_sieges` biomes anchors the siege. As in
    /// vanilla, the siege counts as set up even when no origin 32 blocks away
    /// has room for a zombie.
    fn try_to_setup_siege(&mut self, world: &World, random: &mut impl Rng) -> bool {
        let mut players = Vec::new();
        world.players.iter_players(|_, player| {
            players.push(Arc::clone(player));
            true
        });
        for player in players {
            if player.is_spectator() {
                continue;
            }
            let center = player.block_position();
            if !world.is_village(center)
                || world
                    .biome_at(center)
                    .is_some_and(|biome| biome.has_tag(&BiomeTag::WITHOUT_ZOMBIE_SIEGES))
            {
                continue;
            }
            for _ in 0..POSITION_ATTEMPTS {
                let angle = random.random::<f32>() * TAU;
                self.spawn_pos = BlockPos::new(
                    center.x() + (cos(angle) * SIEGE_DISTANCE).floor() as i32,
                    center.y(),
                    center.z() + (sin(angle) * SIEGE_DISTANCE).floor() as i32,
                );
                if find_random_spawn_pos(world, self.spawn_pos, random).is_some() {
                    self.next_spawn_time = 0;
                    self.zombies_to_spawn = ZOMBIES_PER_SIEGE;
                    break;
                }
            }
            return true;
        }
        false
    }

    /// Vanilla `trySpawn`: one zombie near the siege origin, facing a random way.
    fn try_spawn(&self, world: &Arc<World>, random: &mut impl Rng) {
        let Some(position) = find_random_spawn_pos(world, self.spawn_pos, random) else {
            return;
        };
        // Vanilla `new Zombie(level)` stands at the origin while it is
        // finalized; a failure to build it is logged and skipped.
        let zombie = match create_entity_instance(world, &ZOMBIE, DVec3::ZERO) {
            Ok(zombie) => zombie,
            Err(error) => {
                log::warn!("Failed to create zombie for village siege at {position:?}: {error:?}");
                return;
            }
        };
        if let Some(mob) = zombie.as_mob() {
            mob.finalize_spawn(world, EntitySpawnReason::Event, None);
        }
        zombie.snap_to(position, random.random::<f32>() * 360.0, 0.0);
        if let Err(error) = world.try_add_fresh_entity_with_passengers(zombie) {
            log::debug!("Village siege zombie spawn failed: {error:?}");
        }
    }
}

/// Vanilla `findRandomSpawnPos`: a `WORLD_SURFACE` position within 8 blocks of
/// `pos` that is still in the village and passes the zombie's monster rules.
pub(super) fn find_random_spawn_pos(
    world: &World,
    pos: BlockPos,
    random: &mut impl Rng,
) -> Option<DVec3> {
    for _ in 0..POSITION_ATTEMPTS {
        let x = pos.x() + random.random_range(0..ZOMBIE_SPREAD) - ZOMBIE_SPREAD / 2;
        let z = pos.z() + random.random_range(0..ZOMBIE_SPREAD) - ZOMBIE_SPREAD / 2;
        let y = world.level_height_at(HeightmapType::WorldSurface, x, z);
        let offset = BlockPos::new(x, y, z);
        if world.is_village(offset)
            && <ZombieEntity as Monster>::check_monster_spawn_rules(
                &ZOMBIE,
                world,
                EntitySpawnReason::Event,
                offset,
                random,
            )
        {
            let (x, y, z) = offset.get_bottom_center();
            return Some(DVec3::new(x, y, z));
        }
    }
    None
}
