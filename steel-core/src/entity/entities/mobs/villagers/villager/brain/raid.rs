//! The villager side of raids and bells (vanilla `ReactToBell`,
//! `SetRaidStatus`, `ResetRaidStatus`, `RingBell`, `SetHiddenState`,
//! `LocateHidingPlace`, `MoveToSkySeeingSpot`, `CelebrateVillagersSurvivedRaid`
//! and the `PRE_RAID`, `RAID` and `HIDE` packages).

use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::components::{
    FireworkExplosion, FireworkExplosionShape, Fireworks,
};
use steel_registry::data_components::vanilla_components::FIREWORKS;
use steel_registry::dye_color::DyeColor;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_blocks, vanilla_entities, vanilla_items};
use steel_utils::{BlockPos, GlobalPos};

use super::{
    Behaviors, SPEED_MODIFIER, Weighted, is_home, minimal_look_behavior,
    set_walk_target_from_block_memory,
};
use crate::behavior::blocks::BellBlock;
use crate::chunk::heightmap::HeightmapType;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    trigger_if, trigger_one_shuffled, village_bound_random_stroll,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, HEARD_BELL_TIME, HIDING_PLACE, HOME, INTERACTION_TARGET, LOOK_TARGET,
    MEETING_POINT, PATH, PositionTracker, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::{Activity, ActivityData, BehaviorControl, MemoryStatus};
use crate::entity::entities::FireworkRocketEntity;
use crate::entity::entities::VillagerEntity;
use crate::entity::projectile::Projectile as _;
use crate::entity::{Entity as _, SharedEntity, next_entity_id};
use crate::poi::OccupationStatus;
use crate::world::World;
use crate::world::raid::Raid;

/// Vanilla `RingBell.BELL_RING_CHANCE`: villagers ring only on the other 5%.
const BELL_RING_CHANCE: f32 = 0.95;
/// Vanilla `RingBell.RING_BELL_FROM_DISTANCE`.
const RING_BELL_FROM_DISTANCE: f64 = 3.0;
/// Vanilla `SetHiddenState.HIDE_TIMEOUT`.
const HIDE_TIMEOUT: i64 = 300;
/// Vanilla `CelebrateVillagersSurvivedRaid(600, 600)`.
const CELEBRATE_DURATION: i32 = 600;

/// Vanilla `VillagerGoalPackages.getPreRaidPackage`.
pub(super) fn pre_raid_package() -> ActivityData<VillagerEntity> {
    let gather: Weighted = vec![
        (
            Box::new(set_walk_target_from_block_memory(
                &MEETING_POINT,
                SPEED_MODIFIER * 1.5,
                2,
                150,
                200,
            )),
            6,
        ),
        (
            Box::new(village_bound_random_stroll(SPEED_MODIFIER * 1.5)),
            2,
        ),
    ];
    let behaviors: Behaviors = vec![
        (0, Box::new(ring_bell())),
        (0, Box::new(trigger_one_shuffled(gather))),
        (5, Box::new(minimal_look_behavior())),
        (99, Box::new(reset_raid_status())),
    ];
    ActivityData::new(Activity::PRE_RAID, behaviors)
}

/// Vanilla `VillagerGoalPackages.getRaidPackage`.
pub(super) fn raid_package() -> ActivityData<VillagerEntity> {
    let outside: Weighted = vec![
        (Box::new(move_to_sky_seeing_spot(SPEED_MODIFIER)), 5),
        (
            Box::new(village_bound_random_stroll(SPEED_MODIFIER * 1.1)),
            2,
        ),
    ];
    let behaviors: Behaviors = vec![
        (
            0,
            Box::new(trigger_if(
                raid_exists_and_not_victory,
                trigger_one_shuffled(outside),
            )),
        ),
        (
            0,
            Box::new(TimedBehavior::new(CelebrateVillagersSurvivedRaid::new())),
        ),
        (
            2,
            Box::new(trigger_if(
                raid_exists_and_active,
                locate_hiding_place(24, SPEED_MODIFIER * 1.4, 1),
            )),
        ),
        (5, Box::new(minimal_look_behavior())),
        (99, Box::new(reset_raid_status())),
    ];
    ActivityData::new(Activity::RAID, behaviors)
}

/// Vanilla `VillagerGoalPackages.getHidePackage`.
pub(super) fn hide_package() -> ActivityData<VillagerEntity> {
    let behaviors: Behaviors = vec![
        (0, Box::new(set_hidden_state(15, 3))),
        (
            1,
            Box::new(locate_hiding_place(32, SPEED_MODIFIER * 1.25, 2)),
        ),
        (5, Box::new(minimal_look_behavior())),
    ];
    ActivityData::new(Activity::HIDE, behaviors)
}

fn raid_at(body: &VillagerEntity) -> Option<Arc<Raid>> {
    body.level()?.get_raid_at(body.block_position())
}

/// Vanilla `VillagerGoalPackages.raidExistsAndActive`.
fn raid_exists_and_active(body: &VillagerEntity) -> bool {
    raid_at(body).is_some_and(|raid| raid.is_active() && !raid.is_victory() && !raid.is_loss())
}

/// Vanilla `VillagerGoalPackages.raidExistsAndNotVictory`, which despite its
/// name holds only after a victory.
fn raid_exists_and_not_victory(body: &VillagerEntity) -> bool {
    raid_at(body).is_some_and(|raid| raid.is_victory())
}

/// Vanilla `ReactToBell`: a bell with no raid around sends the villager
/// into hiding.
pub(super) fn react_to_bell() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &HEARD_BELL_TIME,
            MemoryStatus::ValuePresent,
        )],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if world.get_raid_at(body.block_position()).is_none() {
                body.brain.set_active_activity_if_possible(Activity::HIDE);
            }
            true
        },
    )
}

/// Vanilla `SetRaidStatus`: once in a while, follows the raid around it
/// into `PRE_RAID` or `RAID`.
pub(super) fn set_raid_status() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        Vec::new(),
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if rand::random_range(0..20) != 0 {
                return false;
            }
            if let Some(raid) = world.get_raid_at(body.block_position()) {
                let activity = if raid.has_first_wave_spawned() && !raid.is_between_waves() {
                    Activity::RAID
                } else {
                    Activity::PRE_RAID
                };
                body.brain.set_default_activity(activity);
                body.brain.set_active_activity_if_possible(activity);
            }
            true
        },
    )
}

/// Vanilla `ResetRaidStatus`: once the raid is gone, stopped or lost, goes
/// back to the schedule.
fn reset_raid_status() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        Vec::new(),
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if rand::random_range(0..20) != 0 {
                return false;
            }
            let raid = world.get_raid_at(body.block_position());
            if raid.is_none_or(|raid| raid.is_stopped() || raid.is_loss()) {
                body.brain.set_default_activity(Activity::IDLE);
                body.brain.update_activity_from_schedule(world);
            }
            true
        },
    )
}

/// Vanilla `RingBell`: now and then rings the meeting-point bell when
/// standing next to it.
fn ring_bell() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &MEETING_POINT,
            MemoryStatus::ValuePresent,
        )],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if rand::random::<f32>() <= BELL_RING_CHANCE {
                return false;
            }
            let Some(meeting_point) = body.brain.memories().lock().get(&MEETING_POINT) else {
                return false;
            };
            let pos = meeting_point.pos;
            if pos.closer_than(body.block_position(), RING_BELL_FROM_DISTANCE)
                && world.get_block_state(pos).get_block() == &vanilla_blocks::BELL
            {
                BellBlock::attempt_to_ring(Some(body), world, pos, None);
            }
            true
        },
    )
}

/// Vanilla `SetHiddenState`: stays hidden for `seconds` once at the hiding
/// place, gives up after [`HIDE_TIMEOUT`] ticks, then forgets the bell.
fn set_hidden_state(seconds: i32, close_enough_dist: i32) -> impl BehaviorControl<VillagerEntity> {
    let stay_hidden_ticks = seconds * 20;
    let mut ticks_hidden = 0;
    OneShot::new(
        vec![
            MemoryRequirement::new(&HIDING_PLACE, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&HEARD_BELL_TIME, MemoryStatus::ValuePresent),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, time: i64| {
            let (Some(hiding_place), Some(time_triggered)) = ({
                let memories = body.brain.memories().lock();
                (memories.get(&HIDING_PLACE), memories.get(&HEARD_BELL_TIME))
            }) else {
                return false;
            };
            let timed_out_trying_to_hide = time_triggered + HIDE_TIMEOUT <= time;
            if ticks_hidden <= stay_hidden_ticks && !timed_out_trying_to_hide {
                if hiding_place
                    .pos
                    .closer_than(body.block_position(), f64::from(close_enough_dist))
                {
                    ticks_hidden += 1;
                }
            } else {
                {
                    let mut memories = body.brain.memories().lock();
                    memories.erase(&HEARD_BELL_TIME);
                    memories.erase(&HIDING_PLACE);
                }
                body.brain.update_activity_from_schedule(world);
                ticks_hidden = 0;
            }
            true
        },
    )
}

/// Vanilla `LocateHidingPlace`: the home right here, else a random home in
/// `radius`, else its own home becomes the hiding place to walk to.
fn locate_hiding_place(
    radius: i32,
    speed_modifier: f32,
    close_enough_dist: i32,
) -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&HOME, MemoryStatus::Registered),
            MemoryRequirement::new(&HIDING_PLACE, MemoryStatus::Registered),
            MemoryRequirement::new(&PATH, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&BREED_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::Registered),
        ],
        move |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let body_pos = body.block_position();
            let position = body.position();
            let close_enough = f64::from(close_enough_dist);
            let found = {
                let poi = world.poi_storage.lock();
                // Vanilla `PoiManager.find`: the first home in range, if it is
                // close enough.
                poi.get_in_range(
                    &is_home,
                    body_pos,
                    close_enough_dist + 1,
                    OccupationStatus::Any,
                )
                .first()
                .map(|(pos, _)| *pos)
                .filter(|pos| pos.closer_to_center_than(position, close_enough))
                .or_else(|| {
                    // Vanilla `PoiManager.getRandom`: a shuffled pick.
                    let homes = poi.get_in_range(&is_home, body_pos, radius, OccupationStatus::Any);
                    (!homes.is_empty()).then(|| homes[rand::random_range(0..homes.len())].0)
                })
            };
            let found =
                found.or_else(|| body.brain.memories().lock().get(&HOME).map(|home| home.pos));
            if let Some(pos) = found {
                let mut memories = body.brain.memories().lock();
                memories.erase(&PATH);
                memories.erase(&LOOK_TARGET);
                memories.erase(&BREED_TARGET);
                memories.erase(&INTERACTION_TARGET);
                memories.set(&HIDING_PLACE, Some(GlobalPos::new(world.key.clone(), pos)));
                if !pos.closer_to_center_than(position, close_enough) {
                    memories.set(
                        &WALK_TARGET,
                        Some(WalkTarget::new(
                            PositionTracker::block(pos),
                            speed_modifier,
                            close_enough_dist,
                        )),
                    );
                }
            }
            true
        },
    )
}

/// Vanilla `MoveToSkySeeingSpot`: under a roof, walks out to open sky.
fn move_to_sky_seeing_spot(speed_modifier: f32) -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if world.can_see_sky(body.block_position()) {
                return false;
            }
            if let Some(pos) = outdoor_position(world, body) {
                body.brain.memories().lock().set(
                    &WALK_TARGET,
                    Some(WalkTarget::to_point(pos, speed_modifier, 0)),
                );
            }
            true
        },
    )
}

/// Vanilla `MoveToSkySeeingSpot.getOutdoorPosition`.
fn outdoor_position(world: &World, body: &VillagerEntity) -> Option<DVec3> {
    let pos = body.block_position();
    for _ in 0..10 {
        let random_pos = pos.offset(
            rand::random_range(0..20) - 10,
            rand::random_range(0..6) - 3,
            rand::random_range(0..20) - 10,
        );
        if has_no_blocks_above(world, body, random_pos) {
            return Some(DVec3::new(
                f64::from(random_pos.x()) + 0.5,
                f64::from(random_pos.y()),
                f64::from(random_pos.z()) + 0.5,
            ));
        }
    }
    None
}

/// Vanilla `MoveToSkySeeingSpot.hasNoBlocksAbove`.
fn has_no_blocks_above(world: &World, body: &VillagerEntity, target: BlockPos) -> bool {
    world.can_see_sky(target)
        && f64::from(
            world
                .heightmap_pos(HeightmapType::MotionBlocking, target)
                .y(),
        ) <= body.position().y
}

/// Vanilla `CelebrateVillagersSurvivedRaid`: after a won raid, cheers and
/// shoots fireworks under the open sky.
struct CelebrateVillagersSurvivedRaid {
    current_raid: Option<Arc<Raid>>,
}

impl CelebrateVillagersSurvivedRaid {
    const fn new() -> Self {
        Self { current_raid: None }
    }

    /// Vanilla `getFirework`.
    fn firework(color: DyeColor, flight_duration: i32) -> Option<ItemStack> {
        let explosion = FireworkExplosion::new(
            FireworkExplosionShape::Burst,
            vec![color.firework_color()],
            Vec::new(),
            false,
            false,
        );
        let fireworks = match Fireworks::new(flight_duration, vec![explosion]) {
            Ok(fireworks) => fireworks,
            Err(error) => {
                log::error!("celebration firework is invalid: {error}");
                return None;
            }
        };
        let mut rocket = ItemStack::new(&vanilla_items::FIREWORK_ROCKET);
        rocket.set(FIREWORKS, fireworks);
        Some(rocket)
    }

    fn launch_firework(world: &Arc<World>, body: &VillagerEntity) {
        let color = DyeColor::VALUES[rand::random_range(0..DyeColor::VALUES.len())];
        let flight_duration = rand::random_range(0..3);
        let Some(firework) = Self::firework(color, flight_duration) else {
            return;
        };
        let position = body.position();
        let rocket = FireworkRocketEntity::launched(
            &vanilla_entities::FIREWORK_ROCKET,
            next_entity_id(),
            DVec3::new(position.x, body.get_eye_y(), position.z),
            Arc::downgrade(world),
            firework,
        );
        if let Some(owner) = world.get_entity_by_id(body.id()) {
            rocket.set_owner_entity(Some(&owner));
        }
        let rocket: SharedEntity = Arc::new(rocket);
        if let Err(error) = world.try_add_entity(rocket) {
            log::debug!("failed to spawn a celebration firework: {error}");
        }
    }
}

impl Behavior<VillagerEntity> for CelebrateVillagersSurvivedRaid {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &[]
    }

    fn duration(&self) -> (i32, i32) {
        (CELEBRATE_DURATION, CELEBRATE_DURATION)
    }

    fn check_extra_start_conditions(&mut self, world: &Arc<World>, body: &VillagerEntity) -> bool {
        let test_pos = body.block_position();
        self.current_raid = world.get_raid_at(test_pos);
        self.current_raid
            .as_ref()
            .is_some_and(|raid| raid.is_victory())
            && has_no_blocks_above(world, body, test_pos)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &VillagerEntity, _time: i64) -> bool {
        self.current_raid
            .as_ref()
            .is_some_and(|raid| !raid.is_stopped())
    }

    fn stop(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        self.current_raid = None;
        body.brain.update_activity_from_schedule(world);
    }

    fn tick(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if rand::random_range(0..100) == 0 {
            body.play_celebrate_sound();
        }
        if rand::random_range(0..200) == 0
            && has_no_blocks_above(world, body, body.block_position())
        {
            Self::launch_firework(world, body);
        }
    }
}

#[cfg(test)]
mod tests;
