//! Vanilla `RemoveBlockGoal`: walk to a block and trample it away.

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::BlockRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack_template::ItemStackTemplate;
use steel_registry::particle_type::{ItemParticleOption, ParticleData};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{vanilla_items, vanilla_particle_types};
use steel_utils::BlockPos;
use steel_utils::random::Random as _;
use steel_utils::random::legacy_random::LegacyRandom;

use super::move_to_block::MoveToBlockGoal;
use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls};
use crate::entity::{PathfinderMob, SharedEntity};
use crate::world::World;

/// Vanilla `RemoveBlockGoal` search range.
const SEARCH_RANGE: i32 = 24;
/// Vanilla `RemoveBlockGoal.WAIT_AFTER_BLOCK_FOUND`.
const WAIT_AFTER_BLOCK_FOUND: i32 = 20;
/// Vanilla `RemoveBlockGoal.tick`: the block goes after this many ticks.
const REMOVE_AFTER_TICKS: i32 = 60;

/// A sound played by a [`RemoveBlockGoal`] at a block position.
#[derive(Clone, Copy)]
pub(crate) struct RemoveBlockSound {
    pub sound: SoundEventRef,
    pub source: SoundSource,
    pub volume: f32,
}

impl RemoveBlockSound {
    /// Plays the sound with vanilla's `0.9F + random * 0.2F` pitch.
    fn play(self, world: &World, pos: BlockPos, random: &mut LegacyRandom) {
        let pitch = 0.9 + random.next_f32() * 0.2;
        world.play_sound(self.sound, self.source, pos, self.volume, pitch, None);
    }
}

/// Vanilla `RemoveBlockGoal`, with the sound hooks its subclasses override.
pub struct RemoveBlockGoal {
    move_to: MoveToBlockGoal,
    block_to_remove: BlockRef,
    ticks_since_reached_goal: i32,
    destroy_progress_sound: Option<RemoveBlockSound>,
    break_sound: Option<RemoveBlockSound>,
    random: LegacyRandom,
}

impl RemoveBlockGoal {
    #[must_use]
    pub(crate) fn new(
        block_to_remove: BlockRef,
        speed_modifier: f64,
        vertical_search_range: i32,
    ) -> Self {
        Self {
            move_to: MoveToBlockGoal::with_vertical_search_range(
                speed_modifier,
                SEARCH_RANGE,
                vertical_search_range,
                move |world, pos| is_valid_target(world, pos, block_to_remove),
            ),
            block_to_remove,
            ticks_since_reached_goal: 0,
            destroy_progress_sound: None,
            break_sound: None,
            random: LegacyRandom::from_seed(rand::random()),
        }
    }

    /// Vanilla `acceptedDistance` override.
    #[must_use]
    pub(crate) fn with_accepted_distance(mut self, accepted_distance: f64) -> Self {
        self.move_to = self.move_to.with_accepted_distance(accepted_distance);
        self
    }

    /// Vanilla `playDestroyProgressSound` override.
    #[must_use]
    pub(crate) const fn with_destroy_progress_sound(mut self, sound: RemoveBlockSound) -> Self {
        self.destroy_progress_sound = Some(sound);
        self
    }

    /// Vanilla `playBreakSound` override.
    #[must_use]
    pub(crate) const fn with_break_sound(mut self, sound: RemoveBlockSound) -> Self {
        self.break_sound = Some(sound);
        self
    }

    /// Vanilla `getPosWithBlock`: the mob's own block, then the blocks around
    /// and two below it.
    fn pos_with_block(&self, world: &World, pos: BlockPos) -> Option<BlockPos> {
        let is_target =
            |pos: BlockPos| world.get_block_state(pos).get_block() == self.block_to_remove;
        if is_target(pos) {
            return Some(pos);
        }
        [
            pos.below(),
            pos.west(),
            pos.east(),
            pos.north(),
            pos.south(),
            pos.below().below(),
        ]
        .into_iter()
        .find(|&neighbor| is_target(neighbor))
    }
}

/// Vanilla `RemoveBlockGoal.isValidTarget`: the block with two blocks of air
/// above it. Unloaded chunks read as air, so they never match.
fn is_valid_target(world: &World, pos: BlockPos, block: BlockRef) -> bool {
    world.get_block_state(pos).get_block() == block
        && world.get_block_state(pos.above()).is_air()
        && world.get_block_state(pos.above().above()).is_air()
}

impl Goal for RemoveBlockGoal {
    fn controls(&self) -> GoalControls {
        self.move_to.controls()
    }

    fn requires_update_every_tick(&self) -> bool {
        self.move_to.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        if !world.get_game_rule(&MOB_GRIEFING) {
            return false;
        }
        if *self.move_to.next_start_tick_mut() > 0 {
            *self.move_to.next_start_tick_mut() -= 1;
            return false;
        }
        if self.move_to.find_nearest_block(mob, world.as_ref()) {
            *self.move_to.next_start_tick_mut() = reduced_tick_delay(WAIT_AFTER_BLOCK_FOUND);
            true
        } else {
            *self.move_to.next_start_tick_mut() = self.move_to.next_start_tick(mob);
            false
        }
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.move_to.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.start(mob);
        self.ticks_since_reached_goal = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.stop(mob);
        mob.set_fall_distance(1.0);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.move_to.tick(mob, entity);
        let Some(world) = mob.level() else {
            return;
        };
        let eat_pos = self.pos_with_block(&world, mob.block_position());
        let (true, Some(eat_pos)) = (self.move_to.is_reached_target(), eat_pos) else {
            return;
        };
        let center = DVec3::new(
            f64::from(eat_pos.x()) + 0.5,
            f64::from(eat_pos.y()),
            f64::from(eat_pos.z()) + 0.5,
        );
        if self.ticks_since_reached_goal > 0 {
            let movement = mob.velocity();
            mob.set_velocity(DVec3::new(movement.x, 0.3, movement.z));
            let spread = DVec3::new(
                f64::from((self.random.next_f32() - 0.5) * 0.08),
                f64::from((self.random.next_f32() - 0.5) * 0.08),
                f64::from((self.random.next_f32() - 0.5) * 0.08),
            );
            world.send_particles(
                ParticleData::new(
                    &vanilla_particle_types::ITEM,
                    ItemParticleOption::new(ItemStackTemplate::new(&vanilla_items::EGG)),
                ),
                center + DVec3::new(0.0, 0.7, 0.0),
                3,
                spread,
                f64::from(0.15_f32),
            );
        }
        if self.ticks_since_reached_goal % 2 == 0 {
            let movement = mob.velocity();
            mob.set_velocity(DVec3::new(movement.x, -0.3, movement.z));
            if self.ticks_since_reached_goal % 6 == 0
                && let Some(sound) = self.destroy_progress_sound
            {
                sound.play(&world, self.move_to.block_pos(), &mut self.random);
            }
        }
        if self.ticks_since_reached_goal > REMOVE_AFTER_TICKS {
            world.remove_block(eat_pos, false);
            for _ in 0..20 {
                let spread = DVec3::new(
                    self.random.next_gaussian() * 0.02,
                    self.random.next_gaussian() * 0.02,
                    self.random.next_gaussian() * 0.02,
                );
                world.send_particles(
                    ParticleData::simple(&vanilla_particle_types::POOF),
                    center,
                    1,
                    spread,
                    f64::from(0.15_f32),
                );
            }
            if let Some(sound) = self.break_sound {
                sound.play(&world, eat_pos, &mut self.random);
            }
        }
        self.ticks_since_reached_goal += 1;
    }
}
