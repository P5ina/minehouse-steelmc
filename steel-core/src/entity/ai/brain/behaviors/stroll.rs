//! Village-aware wandering (vanilla `VillageBoundRandomStroll`,
//! `InsideBrownianWalk` and `SetWalkTargetAwayFrom`).

use std::f64::consts::FRAC_PI_2;
use std::sync::Arc;

use glam::DVec3;
use rand::seq::SliceRandom as _;
use steel_utils::{BlockPos, Direction, SectionPos};

use crate::entity::ai::brain::behavior::OneShot;
use crate::entity::ai::brain::memory::{MemoryModuleType, MemoryRequirement};
use crate::entity::ai::brain::memory_types::{PositionTracker, WALK_TARGET, WalkTarget};
use crate::entity::ai::brain::{BehaviorControl, MemoryStatus};
use crate::entity::ai::goal::random_pos::{
    default_random_pos_towards, land_random_pos, land_random_pos_away,
};
use crate::entity::{PathfinderMob, SharedEntity};
use crate::physics::WorldCollisionProvider;
use crate::world::World;

/// Vanilla `VillageBoundRandomStroll.MAX_XZ_DIST`.
const VILLAGE_STROLL_XZ_DIST: i32 = 10;
/// Vanilla `VillageBoundRandomStroll.MAX_Y_DIST`.
const VILLAGE_STROLL_Y_DIST: i32 = 7;
/// Vanilla `SetWalkTargetAwayFrom`: how far `LandRandomPos.getPosAway` looks.
const FLEE_XZ_DIST: i32 = 16;
const FLEE_Y_DIST: i32 = 7;
const FLEE_ATTEMPTS: usize = 10;

/// Vanilla `BehaviorUtils.findSectionClosestToVillage`: the section within
/// `radius` of `center` that is closest to a village, or `center` when none
/// is closer.
#[must_use]
pub fn find_section_closest_to_village(
    world: &World,
    center: SectionPos,
    radius: i32,
) -> SectionPos {
    let dist_to_village = world.sections_to_village(center);
    let mut best: Option<(SectionPos, i32)> = None;
    for x in -radius..=radius {
        for y in -radius..=radius {
            for z in -radius..=radius {
                let section = SectionPos::new(center.x() + x, center.y() + y, center.z() + z);
                let distance = world.sections_to_village(section);
                if distance < dist_to_village && best.is_none_or(|(_, best)| distance < best) {
                    best = Some((section, distance));
                }
            }
        }
    }
    best.map_or(center, |(section, _)| section)
}

/// Vanilla `SectionPos.center()` as `Vec3.atBottomCenterOf`.
fn section_bottom_center(section: SectionPos) -> DVec3 {
    DVec3::new(
        f64::from(section.x() * 16 + 8) + 0.5,
        f64::from(section.y() * 16 + 8),
        f64::from(section.z() * 16 + 8) + 0.5,
    )
}

/// Wanders like `RandomStroll`, but outside a village heads for the nearby
/// section closest to one (vanilla `VillageBoundRandomStroll.create`).
pub fn village_bound_random_stroll<E: PathfinderMob + ?Sized>(
    speed_modifier: f32,
) -> impl BehaviorControl<E> {
    village_bound_random_stroll_within(
        speed_modifier,
        VILLAGE_STROLL_XZ_DIST,
        VILLAGE_STROLL_Y_DIST,
    )
}

/// [`village_bound_random_stroll`] with its own reach.
pub fn village_bound_random_stroll_within<E: PathfinderMob + ?Sized>(
    speed_modifier: f32,
    max_xz_dist: i32,
    max_y_dist: i32,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&WALK_TARGET) {
                return false;
            }
            let body_pos = body.block_position();
            let land_pos = if world.is_village(body_pos) {
                land_random_pos(body, max_xz_dist, max_y_dist)
            } else {
                let section = SectionPos::from_block_pos(body_pos);
                let optimal = find_section_closest_to_village(world, section, 2);
                if optimal == section {
                    land_random_pos(body, max_xz_dist, max_y_dist)
                } else {
                    default_random_pos_towards(
                        body,
                        max_xz_dist,
                        max_y_dist,
                        section_bottom_center(optimal),
                        FRAC_PI_2,
                    )
                }
            };
            memories.lock().set(
                &WALK_TARGET,
                land_pos.map(|pos| WalkTarget::to_point(pos, speed_modifier, 0)),
            );
            true
        },
    )
}

/// Indoors, shuffles to a neighboring block under a roof
/// (vanilla `InsideBrownianWalk.create`).
pub fn inside_brownian_walk<E: PathfinderMob + ?Sized>(
    speed_modifier: f32,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            if memories.lock().has_value(&WALK_TARGET) {
                return false;
            }
            let body_pos = body.block_position();
            if world.can_see_sky(body_pos) {
                return false;
            }
            let Some(entity) = world.get_entity_by_id(body.id()) else {
                return true;
            };
            let collisions = WorldCollisionProvider::for_entity(world, entity.as_ref());
            let mut poses: Vec<BlockPos> = (-1..=1)
                .flat_map(|x| (-1..=1).flat_map(move |y| (-1..=1).map(move |z| (x, y, z))))
                .map(|(x, y, z)| body_pos.offset(x, y, z))
                .collect();
            poses.shuffle(&mut rand::rng());
            let target = poses.into_iter().find(|pos| {
                !world.can_see_sky(*pos)
                    && collisions.loaded_and_entity_can_stand_on_face(*pos, Direction::Up)
                    && !collisions.has_entity_context_collision(
                        body.bounding_box(),
                        body.position().y,
                        body.is_descending(),
                    )
            });
            if let Some(target) = target {
                memories.lock().set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::block(target),
                        speed_modifier,
                        0,
                    )),
                );
            }
            true
        },
    )
}

/// Runs from the entity in `walk_away_from` when it is closer than
/// `desired_distance` (vanilla `SetWalkTargetAwayFrom.entity`).
pub fn set_walk_target_away_from_entity<E: PathfinderMob + ?Sized>(
    walk_away_from: &'static MemoryModuleType<SharedEntity>,
    speed_modifier: f32,
    desired_distance: i32,
    interrupt_current_walk: bool,
) -> impl BehaviorControl<E> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(walk_away_from, MemoryStatus::ValuePresent),
        ],
        move |_world: &Arc<World>, body: &E, _time: i64| {
            let Some(memories) = body.brain_memories() else {
                return false;
            };
            let (target, avoid) = {
                let memories = memories.lock();
                let Some(avoid) = memories.get_ref(walk_away_from) else {
                    return false;
                };
                (memories.get(&WALK_TARGET), avoid.position())
            };
            if target.is_some() && !interrupt_current_walk {
                return false;
            }
            let body_position = body.position();
            let desired = f64::from(desired_distance);
            if body_position.distance_squared(avoid) >= desired * desired {
                return false;
            }
            #[expect(clippy::float_cmp, reason = "vanilla compares the speeds exactly")]
            if let Some(target) = target
                && target.speed_modifier == speed_modifier
            {
                let current_direction = target.target.current_position() - body_position;
                let avoid_direction = avoid - body_position;
                if current_direction.dot(avoid_direction) < 0.0 {
                    return false;
                }
            }
            for _ in 0..FLEE_ATTEMPTS {
                if let Some(flee_to) = land_random_pos_away(body, FLEE_XZ_DIST, FLEE_Y_DIST, avoid)
                {
                    memories.lock().set(
                        &WALK_TARGET,
                        Some(WalkTarget::to_point(flee_to, speed_modifier, 0)),
                    );
                    break;
                }
            }
            true
        },
    )
}
