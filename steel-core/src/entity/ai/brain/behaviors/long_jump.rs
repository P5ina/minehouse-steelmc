//! Vanilla long jumps: `LongJumpToRandomPos`, `LongJumpMidJump` and
//! `LongJumpUtil`.

use std::f32::consts::PI;
use std::sync::Arc;

use glam::DVec3;
use rand::seq::SliceRandom as _;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{vanilla_attributes, vanilla_blocks};
use steel_utils::geometry::WorldAabb;
use steel_utils::{BlockPos, Identifier};

use super::UniformInt;
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    LONG_JUMP_COOLDOWN_TICKS, LONG_JUMP_MID_JUMP, LOOK_TARGET, PositionTracker,
};
use crate::entity::ai::goal::random_pos::has_malus;
use crate::entity::{EntityPose, LivingEntity, Mob, PathfinderMob};
use crate::physics::WorldCollisionProvider;
use crate::world::World;

/// Vanilla `LongJumpToRandomPos.FIND_JUMP_TRIES`.
const FIND_JUMP_TRIES: i32 = 20;
/// Vanilla `LongJumpToRandomPos.PREPARE_JUMP_DURATION`.
const PREPARE_JUMP_DURATION: i64 = 40;
/// Vanilla `LongJumpToRandomPos.MIN_PATHFIND_DISTANCE_TO_VALID_JUMP`.
const MIN_PATHFIND_DISTANCE_TO_VALID_JUMP: f32 = 8.0;
/// Vanilla `LongJumpToRandomPos.TIME_OUT_DURATION`.
const JUMP_TIME_OUT_DURATION: i32 = 200;
/// Vanilla `LongJumpMidJump.TIME_OUT_DURATION`.
const MID_JUMP_TIME_OUT_DURATION: i32 = 100;
/// Vanilla `LongJumpToRandomPos.ALLOWED_ANGLES`.
const ALLOWED_ANGLES: [i32; 4] = [65, 70, 75, 80];

const MID_JUMP_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&LONG_JUMP_MID_JUMP, MemoryStatus::ValuePresent),
];

const JUMP_ENTRY: [MemoryRequirement; 3] = [
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&LONG_JUMP_COOLDOWN_TICKS, MemoryStatus::ValueAbsent),
    MemoryRequirement::new(&LONG_JUMP_MID_JUMP, MemoryStatus::ValueAbsent),
];

/// Vanilla `Mth.ceil`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "vanilla truncates to int like a Java cast"
)]
const fn mth_ceil(value: f64) -> i32 {
    value.ceil() as i32
}

fn set_long_jump_cooldown(body: &(impl LivingEntity + ?Sized), ticks: i32) {
    if let Some(memories) = body.brain_memories() {
        memories.lock().set(&LONG_JUMP_COOLDOWN_TICKS, Some(ticks));
    }
}

/// Vanilla `LongJumpMidJump`: keeps the long-jump pose until the body lands.
pub struct LongJumpMidJump {
    time_between_long_jumps: UniformInt,
    landing_sound: SoundEventRef,
}

impl LongJumpMidJump {
    #[must_use]
    pub const fn new(time_between_long_jumps: UniformInt, landing_sound: SoundEventRef) -> Self {
        Self {
            time_between_long_jumps,
            landing_sound,
        }
    }
}

impl<E: Mob + ?Sized> Behavior<E> for LongJumpMidJump {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &MID_JUMP_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (MID_JUMP_TIME_OUT_DURATION, MID_JUMP_TIME_OUT_DURATION)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        !body.on_ground()
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        body.set_discard_friction(true);
        body.set_pose(EntityPose::LongJumping);
    }

    fn stop(&mut self, world: &Arc<World>, body: &E, _time: i64) {
        if body.on_ground() {
            body.set_velocity(body.velocity() * DVec3::new(0.1, 1.0, 0.1));
            world.play_sound_at(
                self.landing_sound,
                SoundSource::Neutral,
                body.position(),
                2.0,
                1.0,
                None,
            );
        }
        body.set_discard_friction(false);
        body.set_pose(EntityPose::Standing);
        if let Some(memories) = body.brain_memories() {
            memories.lock().erase(&LONG_JUMP_MID_JUMP);
        }
        set_long_jump_cooldown(body, self.time_between_long_jumps.sample());
    }
}

/// A block a long jump may land on, weighted by squared distance (vanilla
/// `LongJumpToRandomPos.PossibleJump`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PossibleJump {
    pub target_pos: BlockPos,
    pub weight: i32,
}

/// Vanilla `LongJumpToRandomPos`: picks a random reachable-only-by-jumping
/// block nearby, turns towards it, then leaps.
pub struct LongJumpToRandomPos<E> {
    time_between_long_jumps: UniformInt,
    max_long_jump_height: i32,
    max_long_jump_width: i32,
    max_jump_velocity_multiplier: f32,
    jump_candidates: Vec<PossibleJump>,
    initial_position: Option<DVec3>,
    chosen_jump: Option<DVec3>,
    find_jump_tries: i32,
    prepare_jump_start: i64,
    get_jump_sound: fn(&E) -> SoundEventRef,
    acceptable_landing_spot: fn(&E, BlockPos) -> bool,
    /// Vanilla `LongJumpToPreferredBlock`: the tag of blocks to prefer
    /// landing on, and the chance a jump goes looking for one.
    preferred_blocks: Option<(&'static Identifier, f32)>,
    not_preferred_jump_candidates: Vec<PossibleJump>,
    currently_wanting_preferred_ones: bool,
}

impl<E: PathfinderMob> LongJumpToRandomPos<E> {
    /// Vanilla `LongJumpToRandomPos(...)` with
    /// `defaultAcceptableLandingSpot`.
    #[must_use]
    pub fn new(
        time_between_long_jumps: UniformInt,
        max_long_jump_height: i32,
        max_long_jump_width: i32,
        max_jump_velocity_multiplier: f32,
        get_jump_sound: fn(&E) -> SoundEventRef,
    ) -> Self {
        Self::with_landing_spot(
            time_between_long_jumps,
            max_long_jump_height,
            max_long_jump_width,
            max_jump_velocity_multiplier,
            get_jump_sound,
            default_acceptable_landing_spot,
        )
    }

    #[must_use]
    pub const fn with_landing_spot(
        time_between_long_jumps: UniformInt,
        max_long_jump_height: i32,
        max_long_jump_width: i32,
        max_jump_velocity_multiplier: f32,
        get_jump_sound: fn(&E) -> SoundEventRef,
        acceptable_landing_spot: fn(&E, BlockPos) -> bool,
    ) -> Self {
        Self {
            time_between_long_jumps,
            max_long_jump_height,
            max_long_jump_width,
            max_jump_velocity_multiplier,
            jump_candidates: Vec::new(),
            initial_position: None,
            chosen_jump: None,
            find_jump_tries: 0,
            prepare_jump_start: 0,
            get_jump_sound,
            acceptable_landing_spot,
            preferred_blocks: None,
            not_preferred_jump_candidates: Vec::new(),
            currently_wanting_preferred_ones: false,
        }
    }

    /// Vanilla `LongJumpToPreferredBlock`: like `with_landing_spot`, but a
    /// jump started with `preferred_blocks_chance` aims for a block standing
    /// on `preferred_block_tag` first.
    #[expect(clippy::too_many_arguments, reason = "mirrors the vanilla constructor")]
    #[must_use]
    pub const fn to_preferred_block(
        time_between_long_jumps: UniformInt,
        max_long_jump_height: i32,
        max_long_jump_width: i32,
        max_jump_velocity_multiplier: f32,
        get_jump_sound: fn(&E) -> SoundEventRef,
        preferred_block_tag: &'static Identifier,
        preferred_blocks_chance: f32,
        acceptable_landing_spot: fn(&E, BlockPos) -> bool,
    ) -> Self {
        let mut behavior = Self::with_landing_spot(
            time_between_long_jumps,
            max_long_jump_height,
            max_long_jump_width,
            max_jump_velocity_multiplier,
            get_jump_sound,
            acceptable_landing_spot,
        );
        behavior.preferred_blocks = Some((preferred_block_tag, preferred_blocks_chance));
        behavior
    }

    /// Vanilla `getJumpCandidate`, with `LongJumpToPreferredBlock`'s
    /// override: while wanting a preferred block, every other candidate is
    /// set aside and only the first one of those is offered back.
    fn get_jump_candidate(&mut self, world: &World) -> Option<PossibleJump> {
        let Some((preferred_block_tag, _)) = self.preferred_blocks else {
            return self.take_jump_candidate();
        };
        if !self.currently_wanting_preferred_ones {
            return self.take_jump_candidate();
        }
        while !self.jump_candidates.is_empty() {
            if let Some(jump) = self.take_jump_candidate() {
                if world
                    .get_block_state(jump.target_pos.below())
                    .get_block()
                    .has_tag(preferred_block_tag)
                {
                    return Some(jump);
                }
                self.not_preferred_jump_candidates.push(jump);
            }
        }
        (!self.not_preferred_jump_candidates.is_empty())
            .then(|| self.not_preferred_jump_candidates.remove(0))
    }

    /// Vanilla `getJumpCandidate`: a weighted random candidate, removed from
    /// the list.
    fn take_jump_candidate(&mut self) -> Option<PossibleJump> {
        let total: i64 = self
            .jump_candidates
            .iter()
            .map(|jump| i64::from(jump.weight))
            .sum();
        if total <= 0 {
            return None;
        }
        let mut selection = rand::random_range(0..total);
        let index = self.jump_candidates.iter().position(|jump| {
            selection -= i64::from(jump.weight);
            selection < 0
        })?;
        Some(self.jump_candidates.remove(index))
    }

    /// Vanilla `isAcceptableLandingPosition`.
    fn is_acceptable_landing_position(&self, body: &E, target_pos: BlockPos) -> bool {
        let body_pos = body.block_position();
        if body_pos.x() == target_pos.x() && body_pos.z() == target_pos.z() {
            return false;
        }
        (self.acceptable_landing_spot)(body, target_pos)
    }

    /// Vanilla `calculateOptimalJumpVector`.
    fn calculate_optimal_jump_vector(
        &self,
        world: &Arc<World>,
        body: &E,
        target: DVec3,
    ) -> Option<DVec3> {
        let mut allowed_angles = ALLOWED_ANGLES;
        allowed_angles.shuffle(&mut rand::rng());
        let jump_strength = body
            .attributes()
            .lock()
            .required_value(vanilla_attributes::JUMP_STRENGTH);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "vanilla casts the jump velocity to float"
        )]
        let max_jump_velocity =
            (jump_strength * f64::from(self.max_jump_velocity_multiplier)) as f32;
        allowed_angles.into_iter().find_map(|angle| {
            calculate_jump_vector_for_angle(world, body, target, max_jump_velocity, angle, true)
        })
    }

    /// Vanilla `pickCandidate`.
    fn pick_candidate(&mut self, world: &Arc<World>, body: &E, time: i64) {
        while !self.jump_candidates.is_empty() {
            let Some(jump) = self.get_jump_candidate(world) else {
                continue;
            };
            let target_pos = jump.target_pos;
            if !self.is_acceptable_landing_position(body, target_pos) {
                continue;
            }
            let (x, y, z) = target_pos.get_center();
            let Some(jump_vector) =
                self.calculate_optimal_jump_vector(world, body, DVec3::new(x, y, z))
            else {
                continue;
            };
            if let Some(memories) = body.brain_memories() {
                memories
                    .lock()
                    .set(&LOOK_TARGET, Some(PositionTracker::block(target_pos)));
            }
            let path = body.create_path_to_with_max_length(
                target_pos,
                0,
                MIN_PATHFIND_DISTANCE_TO_VALID_JUMP,
            );
            if path.is_none_or(|path| !path.can_reach()) {
                self.chosen_jump = Some(jump_vector);
                self.prepare_jump_start = time;
                return;
            }
        }
    }
}

impl<E: PathfinderMob> Behavior<E> for LongJumpToRandomPos<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &JUMP_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (JUMP_TIME_OUT_DURATION, JUMP_TIME_OUT_DURATION)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &E) -> bool {
        let can_start = body.on_ground()
            && !body.is_in_water()
            && !body.is_in_lava()
            && world.get_block_state(body.block_position()).get_block()
                != &vanilla_blocks::HONEY_BLOCK;
        if !can_start {
            set_long_jump_cooldown(body, self.time_between_long_jumps.sample() / 2);
        }
        can_start
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        let is_valid = self.initial_position == Some(body.position())
            && self.find_jump_tries > 0
            && !body.is_in_water()
            && (self.chosen_jump.is_some() || !self.jump_candidates.is_empty());
        if !is_valid && let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            if !memories.has_value(&LONG_JUMP_MID_JUMP) {
                memories.set(
                    &LONG_JUMP_COOLDOWN_TICKS,
                    Some(self.time_between_long_jumps.sample() / 2),
                );
                memories.erase(&LOOK_TARGET);
            }
        }
        is_valid
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        self.chosen_jump = None;
        self.find_jump_tries = FIND_JUMP_TRIES;
        self.initial_position = Some(body.position());
        let mob_pos = body.block_position();
        let width = self.max_long_jump_width;
        let height = self.max_long_jump_height;
        self.jump_candidates = BlockPos::between_closed(
            mob_pos.offset(-width, -height, -width),
            mob_pos.offset(width, height, width),
        )
        .filter(|pos| *pos != mob_pos)
        .map(|pos| {
            let dx = pos.x() - mob_pos.x();
            let dy = pos.y() - mob_pos.y();
            let dz = pos.z() - mob_pos.z();
            PossibleJump {
                target_pos: pos,
                weight: dx * dx + dy * dy + dz * dz,
            }
        })
        .collect();
        self.not_preferred_jump_candidates.clear();
        self.currently_wanting_preferred_ones = self
            .preferred_blocks
            .is_some_and(|(_, chance)| rand::random::<f32>() < chance);
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, time: i64) {
        if let Some(chosen_jump) = self.chosen_jump {
            if time - self.prepare_jump_start >= PREPARE_JUMP_DURATION {
                let (_, x_rot) = body.rotation();
                body.set_rotation((body.y_body_rot(), x_rot));
                body.set_discard_friction(true);
                let org_length = chosen_jump.length();
                let length_with_jump_boost = org_length + f64::from(body.get_jump_boost_power());
                body.set_velocity(chosen_jump * (length_with_jump_boost / org_length));
                if let Some(memories) = body.brain_memories() {
                    memories.lock().set(&LONG_JUMP_MID_JUMP, Some(true));
                }
                world.play_sound_at(
                    (self.get_jump_sound)(body),
                    SoundSource::Neutral,
                    body.position(),
                    1.0,
                    1.0,
                    None,
                );
            }
        } else {
            self.find_jump_tries -= 1;
            self.pick_candidate(world, body, time);
        }
    }
}

/// Vanilla `LongJumpToRandomPos.defaultAcceptableLandingSpot`.
pub fn default_acceptable_landing_spot<E: PathfinderMob + ?Sized>(
    body: &E,
    target_pos: BlockPos,
) -> bool {
    body.level().is_some_and(|world| {
        world.get_block_state(target_pos.below()).is_solid_render() && !has_malus(body, target_pos)
    })
}

/// Vanilla `LongJumpUtil.calculateJumpVectorForAngle`.
#[must_use]
pub fn calculate_jump_vector_for_angle<E: Mob>(
    world: &Arc<World>,
    body: &E,
    target_pos: DVec3,
    max_jump_velocity: f32,
    angle: i32,
    check_collision: bool,
) -> Option<DVec3> {
    let mob_pos = body.position();
    let direction_vector_plane =
        DVec3::new(target_pos.x - mob_pos.x, 0.0, target_pos.z - mob_pos.z).normalize_or_zero()
            * 0.5;
    let target_position = target_pos - direction_vector_plane;
    let direction_vector = target_position - mob_pos;
    #[expect(
        clippy::cast_precision_loss,
        reason = "jump angles are small whole degrees"
    )]
    let angrad = angle as f32 * PI / 180.0;
    let xz_ang = direction_vector.z.atan2(direction_vector.x);
    let r2 = DVec3::new(direction_vector.x, 0.0, direction_vector.z).length_squared();
    let r = r2.sqrt();
    let y = direction_vector.y;
    let g = body.get_gravity();
    let sin2ang = f64::from(2.0 * angrad).sin();
    let cosangsqr = f64::from(angrad).cos().powi(2);
    let sinangrad = f64::from(angrad).sin();
    let cosangrad = f64::from(angrad).cos();
    let sinxz_ang = xz_ang.sin();
    let cosxz_ang = xz_ang.cos();
    let v0sqr = r2 * g / (r * sin2ang - 2.0 * y * cosangsqr);
    if v0sqr < 0.0 {
        return None;
    }
    let v0 = v0sqr.sqrt();
    if v0 > f64::from(max_jump_velocity) {
        return None;
    }
    let v0r = v0 * cosangrad;
    let v0y = v0 * sinangrad;
    if check_collision {
        let samples = mth_ceil(r / v0r) * 2;
        let mut ri = 0.0;
        let mut previous_pos: Option<DVec3> = None;
        let dimensions = body.dimensions_for_pose(EntityPose::LongJumping);
        for _ in 0..samples - 1 {
            ri += r / f64::from(samples);
            let yi =
                sinangrad / cosangrad * ri - ri.powi(2) * g / (2.0 * v0sqr * cosangrad.powi(2));
            let sample_pos = DVec3::new(
                mob_pos.x + ri * cosxz_ang,
                mob_pos.y + yi,
                mob_pos.z + ri * sinxz_ang,
            );
            if let Some(previous_pos) = previous_pos
                && !is_clear_transition(
                    world,
                    body,
                    dimensions.width,
                    dimensions.height,
                    previous_pos,
                    sample_pos,
                )
            {
                return None;
            }
            previous_pos = Some(sample_pos);
        }
    }
    Some(DVec3::new(v0r * cosxz_ang, v0y, v0r * sinxz_ang) * f64::from(0.95_f32))
}

/// Vanilla `LongJumpUtil.isClearTransition`.
fn is_clear_transition<E: Mob>(
    world: &Arc<World>,
    body: &E,
    width: f32,
    height: f32,
    position1: DVec3,
    position2: DVec3,
) -> bool {
    let direction = position2 - position1;
    let min_dimension = f64::from(width.min(height));
    let checks = mth_ceil(direction.length() / min_dimension);
    let normalized_direction = direction.normalize_or_zero();
    let mut next_point_to_check = position1;
    let collision_world = WorldCollisionProvider::for_entity(world, body);
    let half_width = f64::from(width) / 2.0;
    for i in 0..checks {
        next_point_to_check = if i == checks - 1 {
            position2
        } else {
            next_point_to_check + normalized_direction * (min_dimension * f64::from(0.9_f32))
        };
        let aabb = WorldAabb::entity_box(
            next_point_to_check.x,
            next_point_to_check.y,
            next_point_to_check.z,
            half_width,
            f64::from(height),
        );
        if collision_world.has_entity_context_collision(
            aabb,
            body.position().y,
            body.is_descending(),
        ) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use steel_registry::vanilla_block_tags::BlockTag;
    use steel_registry::{init_vanilla_registry, sound_events, vanilla_entities};
    use steel_utils::ChunkPos;
    use steel_utils::types::UpdateFlags;

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::GoatEntity;
    use crate::entity::{init_entities, next_entity_id};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};

    #[test]
    fn jump_vector_heads_for_the_target_and_respects_the_speed_cap() {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world("goat_long_jump");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let goat = GoatEntity::new(
            &vanilla_entities::GOAT,
            next_entity_id(),
            DVec3::new(2.5, 100.0, 2.5),
            Arc::downgrade(world),
        );
        let target = DVec3::new(6.5, 101.5, 2.5);

        let Some(jump) = calculate_jump_vector_for_angle(world, &goat, target, 10.0, 70, false)
        else {
            panic!("a four block hop at 70 degrees is reachable");
        };
        assert!(jump.x > 0.0 && jump.y > 0.0);
        assert!(jump.z.abs() < 1.0e-9);
        assert!((jump.y / jump.x - f64::from(70.0_f32.to_radians()).tan()).abs() < 1.0e-3);

        assert!(calculate_jump_vector_for_angle(world, &goat, target, 0.1, 70, false).is_none());
    }

    static PREFERRED: Identifier = BlockTag::FROG_PREFER_JUMP_TO;

    #[test]
    fn preferred_jumps_aim_for_lily_pads_first() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("frog_preferred_long_jump");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let pad = BlockPos::new(4, 64, 4);
        assert!(world.set_block(
            pad,
            vanilla_blocks::LILY_PAD.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
        let mut jump = LongJumpToRandomPos::<GoatEntity>::to_preferred_block(
            UniformInt::of(1, 1),
            2,
            4,
            1.0,
            |_| &sound_events::ENTITY_FROG_LONG_JUMP,
            &PREFERRED,
            1.0,
            |_, _| true,
        );
        let plain = PossibleJump {
            target_pos: BlockPos::new(1, 64, 1),
            weight: 1_000,
        };
        let on_pad = PossibleJump {
            target_pos: pad.above(),
            weight: 1,
        };
        jump.jump_candidates = vec![plain, on_pad];
        jump.currently_wanting_preferred_ones = true;
        assert_eq!(jump.get_jump_candidate(world), Some(on_pad));
        assert_eq!(jump.get_jump_candidate(world), Some(plain));
        assert_eq!(jump.get_jump_candidate(world), None);

        jump.jump_candidates = vec![plain];
        jump.currently_wanting_preferred_ones = false;
        assert_eq!(jump.get_jump_candidate(world), Some(plain));
    }
}
