//! The villager brain (vanilla `Villager.BRAIN_PROVIDER` and
//! `VillagerGoalPackages`), with the villager-only behaviors and sensors.
//!
//! Not yet ported: sleeping,
//! breeding, gifts to heroes, trading with other villagers, iron
//! golem calls and raids.

use std::sync::Arc;

use steel_registry::entity_type::{EntityTypeRef, MobCategory};
use steel_registry::item_stack::ItemStack;
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::{vanilla_entities, vanilla_villager_professions};
use steel_utils::entity_events::EntityStatus;

use self::farm::{HarvestFarmland, UseBonemeal};
use self::poi::{
    GoToPotentialJobSite, acquirable_job_site, assign_profession_from_job_site,
    poi_competitor_scan, reset_profession, set_closest_home_as_walk_target,
    set_walk_target_from_block_memory, yield_job_site,
};
pub(super) use self::poi::{held_job_site, is_acquirable_job, is_home, is_meeting};
use self::work::{SecondaryPoiSensor, WorkAtPoi, stroll_to_poi_list};
use super::{VillagerEntity, trading_player_of};
use crate::entity::ai::brain::behavior::{Behavior, OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::{
    DoNothing, LookAtTargetSink, MoveToTargetSink, Swim, acquire_poi, go_to_wanted_item,
    inside_brownian_walk, interact_with_door, interact_with_type, run_one, set_entity_look_target,
    set_look_and_interact, set_walk_target_away_from_entity, set_walk_target_from_look_target,
    stroll_around_poi, stroll_to_poi, trigger_one_shuffled, validate_bed_poi, validate_nearby_poi,
    village_bound_random_stroll, village_bound_random_stroll_within,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    BREED_TARGET, GOLEM_DETECTED_RECENTLY, HOME, HURT_BY, HURT_BY_ENTITY, INTERACTION_TARGET,
    JOB_SITE, LOOK_TARGET, MEETING_POINT, NEAREST_HOSTILE, NEAREST_LIVING_ENTITIES,
    NEAREST_VISIBLE_LIVING_ENTITIES, PATH, POTENTIAL_JOB_SITE, PositionTracker, SECONDARY_JOB_SITE,
    VISIBLE_VILLAGER_BABIES, WALK_TARGET, WalkTarget,
};
use crate::entity::ai::brain::sensors::{
    HurtBySensor, NearestItemSensor, NearestLivingEntitySensor, PlayerSensor,
};
use crate::entity::ai::brain::{
    Activity, ActivityData, BehaviorControl, Brain, Memories, MemoryStatus, Sensor,
};
use crate::entity::ai::goal::random_pos::{land_random_pos, land_random_pos_weighted};
use crate::entity::{AgeableMob, Entity as _, LivingEntity, Mob as _, SharedEntity};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;

/// Vanilla `VillagerGoalPackages.STROLL_SPEED_MODIFIER`.
const STROLL_SPEED_MODIFIER: f32 = 0.4;
/// Vanilla `BRAIN_PROVIDER`: the speed modifier every package gets.
const SPEED_MODIFIER: f32 = 0.5;
/// Vanilla `VillagerGoalPackages.getPanicPackage`: `speedModifier * 1.5`.
const PANIC_SPEED_MODIFIER: f32 = SPEED_MODIFIER * 1.5;
/// Vanilla `EnvironmentAttributes.VILLAGER_ACTIVITY`.
const VILLAGER_ACTIVITY: &str = "minecraft:gameplay/villager_activity";
/// Vanilla `EnvironmentAttributes.BABY_VILLAGER_ACTIVITY`.
const BABY_VILLAGER_ACTIVITY: &str = "minecraft:gameplay/baby_villager_activity";

/// Vanilla `Villager.BRAIN_PROVIDER.makeBrain`: babies play where adults
/// work, and both share the other activities.
pub(super) fn make_brain(baby: bool, profession: VillagerProfessionRef) -> Brain<VillagerEntity> {
    let mut activities = Vec::new();
    if baby {
        activities.push(play_package());
    } else {
        activities.push(
            work_package(profession).with_conditions(vec![MemoryRequirement::new(
                &JOB_SITE,
                MemoryStatus::ValuePresent,
            )]),
        );
    }
    activities.push(core_package(profession));
    activities.push(meet_package().with_conditions(vec![MemoryRequirement::new(
        &MEETING_POINT,
        MemoryStatus::ValuePresent,
    )]));
    activities.push(rest_package());
    activities.push(idle_package());
    activities.push(panic_package());
    // TODO(raids): the `PRE_RAID`, `RAID` and `HIDE` packages.
    Brain::new(&[], sensors(), activities, None)
}

/// Vanilla `registerBrainGoals`: the schedule the villager's age follows.
pub(super) const fn schedule_for(baby: bool) -> &'static str {
    if baby {
        BABY_VILLAGER_ACTIVITY
    } else {
        VILLAGER_ACTIVITY
    }
}

fn sensors() -> Vec<Box<dyn Sensor<VillagerEntity>>> {
    // TODO(villager brain): vanilla also has the nearest bed sensor.
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(PlayerSensor),
        Box::new(NearestItemSensor),
        Box::new(HurtBySensor),
        Box::new(VillagerHostilesSensor),
        Box::new(VillagerBabiesSensor),
        Box::new(GolemSensor),
        Box::new(SecondaryPoiSensor),
    ]
}

type Behaviors = Vec<(i32, Box<dyn BehaviorControl<VillagerEntity>>)>;
type Weighted = Vec<(Box<dyn BehaviorControl<VillagerEntity>>, i32)>;

fn is_type(entity: &SharedEntity, entity_type: EntityTypeRef) -> bool {
    entity.entity_type() == entity_type
}

/// Vanilla `VillagerGoalPackages.getCorePackage`.
fn core_package(profession: VillagerProfessionRef) -> ActivityData<VillagerEntity> {
    // TODO(raids): vanilla also reacts to bells and tracks raids (priority 0).
    let behaviors: Behaviors = vec![
        (0, Box::new(TimedBehavior::new(Swim::new(0.8)))),
        (0, Box::new(interact_with_door())),
        (
            0,
            Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
        ),
        (0, Box::new(TimedBehavior::new(VillagerPanicTrigger))),
        (0, Box::new(wake_up())),
        (
            0,
            Box::new(validate_nearby_poi(
                move |poi_type| held_job_site(profession, poi_type),
                &JOB_SITE,
            )),
        ),
        (
            0,
            Box::new(validate_nearby_poi(
                move |poi_type| acquirable_job_site(profession, poi_type),
                &POTENTIAL_JOB_SITE,
            )),
        ),
        (1, Box::new(TimedBehavior::new(MoveToTargetSink::new()))),
        (2, Box::new(poi_competitor_scan())),
        (
            3,
            Box::new(TimedBehavior::new(LookAndFollowTradingPlayerSink {
                speed_modifier: SPEED_MODIFIER,
            })),
        ),
        (
            5,
            Box::new(go_to_wanted_item(
                |_: &VillagerEntity| true,
                SPEED_MODIFIER,
                false,
                4,
            )),
        ),
        (
            6,
            Box::new(acquire_poi(
                move |poi_type| acquirable_job_site(profession, poi_type),
                &JOB_SITE,
                &POTENTIAL_JOB_SITE,
                true,
                None,
                |_, _| true,
            )),
        ),
        (
            7,
            Box::new(TimedBehavior::new(GoToPotentialJobSite {
                speed_modifier: SPEED_MODIFIER,
            })),
        ),
        (8, Box::new(yield_job_site(SPEED_MODIFIER))),
        (
            10,
            Box::new(acquire_poi(
                is_home,
                &HOME,
                &HOME,
                false,
                Some(EntityStatus::VillagerHappy),
                validate_bed_poi,
            )),
        ),
        (
            10,
            Box::new(acquire_poi(
                is_meeting,
                &MEETING_POINT,
                &MEETING_POINT,
                true,
                Some(EntityStatus::VillagerHappy),
                |_, _| true,
            )),
        ),
        (10, Box::new(assign_profession_from_job_site())),
        (10, Box::new(reset_profession())),
    ];
    ActivityData::new(Activity::CORE, behaviors)
}

/// Vanilla `VillagerGoalPackages.getPlayPackage`.
fn play_package() -> ActivityData<VillagerEntity> {
    // TODO(villager brain): vanilla also jumps on beds (weight 2).
    let wander: Weighted = vec![
        (
            Box::new(interact_with_type(
                |entity| is_type(entity, &vanilla_entities::VILLAGER),
                8,
                &INTERACTION_TARGET,
                SPEED_MODIFIER,
                2,
            )),
            2,
        ),
        (
            Box::new(interact_with_type(
                |entity| is_type(entity, &vanilla_entities::CAT),
                8,
                &INTERACTION_TARGET,
                SPEED_MODIFIER,
                2,
            )),
            1,
        ),
        (Box::new(village_bound_random_stroll(SPEED_MODIFIER)), 1),
        (
            Box::new(set_walk_target_from_look_target(
                |_| true,
                |_| SPEED_MODIFIER,
                2,
            )),
            1,
        ),
        (Box::new(DoNothing::new(20, 40)), 2),
    ];
    let behaviors: Behaviors = vec![
        (
            0,
            Box::new(TimedBehavior::new(MoveToTargetSink::with_timeout(80, 120))),
        ),
        (5, Box::new(full_look_behavior())),
        (5, Box::new(play_tag_with_other_kids())),
        (
            5,
            Box::new(run_one(
                vec![MemoryRequirement::new(
                    &VISIBLE_VILLAGER_BABIES,
                    MemoryStatus::ValueAbsent,
                )],
                wander,
            )),
        ),
        (99, Box::new(update_activity_from_schedule())),
    ];
    ActivityData::new(Activity::PLAY, behaviors)
}

/// Vanilla `VillagerGoalPackages.getWorkPackage`.
fn work_package(profession: VillagerProfessionRef) -> ActivityData<VillagerEntity> {
    let farmer = profession.key == vanilla_villager_professions::FARMER.key;
    let work_at_poi = if farmer {
        WorkAtPoi::at_composter()
    } else {
        WorkAtPoi::new()
    };
    let work: Weighted = vec![
        (Box::new(TimedBehavior::new(work_at_poi)), 7),
        (
            Box::new(stroll_around_poi(&JOB_SITE, STROLL_SPEED_MODIFIER, 4)),
            2,
        ),
        (
            Box::new(stroll_to_poi(&JOB_SITE, STROLL_SPEED_MODIFIER, 1, 10)),
            5,
        ),
        (
            Box::new(stroll_to_poi_list(
                &SECONDARY_JOB_SITE,
                SPEED_MODIFIER,
                1,
                6,
                &JOB_SITE,
            )),
            5,
        ),
        (
            Box::new(TimedBehavior::new(HarvestFarmland::new())),
            if farmer { 2 } else { 5 },
        ),
        (
            Box::new(TimedBehavior::new(UseBonemeal::new())),
            if farmer { 4 } else { 7 },
        ),
    ];
    // TODO(villager brain): vanilla also gives gifts to heroes at priority 3.
    let behaviors: Behaviors = vec![
        (5, Box::new(minimal_look_behavior())),
        (5, Box::new(run_one(Vec::new(), work))),
        (10, Box::new(TimedBehavior::new(ShowTradesToPlayer::new()))),
        (
            10,
            Box::new(set_look_and_interact(
                |entity| is_type(entity, &vanilla_entities::PLAYER),
                4,
            )),
        ),
        (
            2,
            Box::new(set_walk_target_from_block_memory(
                &JOB_SITE,
                SPEED_MODIFIER,
                9,
                100,
                1200,
            )),
        ),
        (99, Box::new(update_activity_from_schedule())),
    ];
    ActivityData::new(Activity::WORK, behaviors)
}

/// Vanilla `VillagerGoalPackages.getMeetPackage`.
fn meet_package() -> ActivityData<VillagerEntity> {
    let socialize: Weighted = vec![
        (
            Box::new(stroll_around_poi(&MEETING_POINT, STROLL_SPEED_MODIFIER, 40)),
            2,
        ),
        (Box::new(socialize_at_bell()), 2),
    ];
    // TODO(villager brain): vanilla also gives gifts to heroes and trades
    // with other villagers at priority 3.
    let behaviors: Behaviors = vec![
        (2, Box::new(trigger_one_shuffled(socialize))),
        (10, Box::new(TimedBehavior::new(ShowTradesToPlayer::new()))),
        (
            10,
            Box::new(set_look_and_interact(
                |entity| is_type(entity, &vanilla_entities::PLAYER),
                4,
            )),
        ),
        (
            2,
            Box::new(set_walk_target_from_block_memory(
                &MEETING_POINT,
                SPEED_MODIFIER,
                6,
                100,
                200,
            )),
        ),
        (3, Box::new(validate_nearby_poi(is_meeting, &MEETING_POINT))),
        (5, Box::new(full_look_behavior())),
        (99, Box::new(update_activity_from_schedule())),
    ];
    ActivityData::new(Activity::MEET, behaviors)
}

/// Vanilla `SocializeAtBell`: now and then, near the bell, walks up to the
/// closest villager in sight to chat.
fn socialize_at_bell() -> impl BehaviorControl<VillagerEntity> {
    /// Vanilla `SPEED_MODIFIER`.
    const SOCIALIZE_SPEED_MODIFIER: f32 = 0.3;
    OneShot::new(
        vec![
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&MEETING_POINT, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&NEAREST_VISIBLE_LIVING_ENTITIES, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::ValueAbsent),
        ],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let (meeting_point, visible) = {
                let memories = body.brain.memories().lock();
                if memories.has_value(&INTERACTION_TARGET) {
                    return false;
                }
                let (Some(meeting_point), Some(visible)) = (
                    memories.get(&MEETING_POINT),
                    memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES),
                ) else {
                    return false;
                };
                (meeting_point, visible)
            };
            let is_villager = |entity: &SharedEntity| is_type(entity, &vanilla_entities::VILLAGER);
            let (x, y, z) = meeting_point.pos.get_center();
            let near_bell = glam::DVec3::new(x, y, z).distance_squared(body.position()) < 16.0;
            if rand::random_range(0..100) != 0
                || world.key != meeting_point.dimension
                || !near_bell
                || !visible.contains_matching(is_villager)
            {
                return false;
            }
            let friend = visible.find_closest(|entity| {
                is_villager(entity) && entity.position().distance_squared(body.position()) <= 32.0
            });
            if let Some(friend) = friend {
                let mut memories = body.brain.memories().lock();
                memories.set(&INTERACTION_TARGET, Some(Arc::clone(&friend)));
                memories.set(
                    &LOOK_TARGET,
                    Some(PositionTracker::entity(Arc::clone(&friend), true)),
                );
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::entity(friend, false),
                        SOCIALIZE_SPEED_MODIFIER,
                        1,
                    )),
                );
            }
            true
        },
    )
}

/// Vanilla `VillagerGoalPackages.getRestPackage`.
fn rest_package() -> ActivityData<VillagerEntity> {
    let homeless: Weighted = vec![
        (Box::new(set_closest_home_as_walk_target(SPEED_MODIFIER)), 1),
        (Box::new(inside_brownian_walk(SPEED_MODIFIER)), 4),
        (Box::new(go_to_closest_village(SPEED_MODIFIER, 4)), 2),
        (Box::new(DoNothing::new(20, 40)), 2),
    ];
    // TODO(villager brain): vanilla also sleeps in the bed (priority 3).
    let behaviors: Behaviors = vec![
        (
            2,
            Box::new(set_walk_target_from_block_memory(
                &HOME,
                SPEED_MODIFIER,
                1,
                150,
                1200,
            )),
        ),
        (3, Box::new(validate_nearby_poi(is_home, &HOME))),
        (
            5,
            Box::new(run_one(
                vec![MemoryRequirement::new(&HOME, MemoryStatus::ValueAbsent)],
                homeless,
            )),
        ),
        (5, Box::new(minimal_look_behavior())),
        (99, Box::new(update_activity_from_schedule())),
    ];
    ActivityData::new(Activity::REST, behaviors)
}

/// Vanilla `VillagerGoalPackages.getIdlePackage`.
fn idle_package() -> ActivityData<VillagerEntity> {
    // TODO(villager brain): vanilla also walks up to a villager to breed
    // (weight 1) and jumps on beds (weight 1).
    let wander: Weighted = vec![
        (
            Box::new(interact_with_type(
                |entity| is_type(entity, &vanilla_entities::VILLAGER),
                8,
                &INTERACTION_TARGET,
                SPEED_MODIFIER,
                2,
            )),
            2,
        ),
        (
            Box::new(interact_with_type(
                |entity| is_type(entity, &vanilla_entities::CAT),
                8,
                &INTERACTION_TARGET,
                SPEED_MODIFIER,
                2,
            )),
            1,
        ),
        (Box::new(village_bound_random_stroll(SPEED_MODIFIER)), 1),
        (
            Box::new(set_walk_target_from_look_target(
                |_| true,
                |_| SPEED_MODIFIER,
                2,
            )),
            1,
        ),
        (Box::new(DoNothing::new(30, 60)), 1),
    ];
    // TODO(villager brain): vanilla also gives gifts to heroes, trades with
    // other villagers and breeds at priority 3.
    let behaviors: Behaviors = vec![
        (2, Box::new(run_one(Vec::new(), wander))),
        (
            3,
            Box::new(set_look_and_interact(
                |entity| is_type(entity, &vanilla_entities::PLAYER),
                4,
            )),
        ),
        (3, Box::new(TimedBehavior::new(ShowTradesToPlayer::new()))),
        (5, Box::new(full_look_behavior())),
        (99, Box::new(update_activity_from_schedule())),
    ];
    ActivityData::new(Activity::IDLE, behaviors)
}

/// Vanilla `VillagerGoalPackages.getPanicPackage`.
fn panic_package() -> ActivityData<VillagerEntity> {
    let behaviors: Behaviors = vec![
        (0, Box::new(villager_calm_down())),
        (
            1,
            Box::new(set_walk_target_away_from_entity(
                &NEAREST_HOSTILE,
                PANIC_SPEED_MODIFIER,
                6,
                false,
            )),
        ),
        (
            1,
            Box::new(set_walk_target_away_from_entity(
                &HURT_BY_ENTITY,
                PANIC_SPEED_MODIFIER,
                6,
                false,
            )),
        ),
        (
            3,
            Box::new(village_bound_random_stroll_within(
                PANIC_SPEED_MODIFIER,
                2,
                2,
            )),
        ),
        (5, Box::new(minimal_look_behavior())),
    ];
    ActivityData::new(Activity::PANIC, behaviors)
}

fn look_at_type(entity_type: EntityTypeRef) -> Box<dyn BehaviorControl<VillagerEntity>> {
    Box::new(set_entity_look_target(8.0, move |entity| {
        entity.entity_type() == entity_type
    }))
}

fn look_at_category(category: MobCategory) -> Box<dyn BehaviorControl<VillagerEntity>> {
    Box::new(set_entity_look_target(8.0, move |entity| {
        entity.entity_type().mob_category == category
    }))
}

/// Vanilla `VillagerGoalPackages.getFullLookBehavior`.
fn full_look_behavior() -> impl BehaviorControl<VillagerEntity> {
    run_one(
        Vec::new(),
        vec![
            (look_at_type(&vanilla_entities::CAT), 8),
            (look_at_type(&vanilla_entities::VILLAGER), 2),
            (look_at_type(&vanilla_entities::PLAYER), 2),
            (look_at_category(MobCategory::Creature), 1),
            (look_at_category(MobCategory::WaterCreature), 1),
            (look_at_category(MobCategory::Axolotls), 1),
            (look_at_category(MobCategory::UndergroundWaterCreature), 1),
            (look_at_category(MobCategory::WaterAmbient), 1),
            (look_at_category(MobCategory::Monster), 1),
            (Box::new(DoNothing::new(30, 60)), 2),
        ],
    )
}

/// Vanilla `VillagerGoalPackages.getMinimalLookBehavior`.
fn minimal_look_behavior() -> impl BehaviorControl<VillagerEntity> {
    run_one(
        Vec::new(),
        vec![
            (look_at_type(&vanilla_entities::VILLAGER), 2),
            (look_at_type(&vanilla_entities::PLAYER), 2),
            (Box::new(DoNothing::new(30, 60)), 8),
        ],
    )
}

/// Vanilla `UpdateActivityFromSchedule`.
fn update_activity_from_schedule() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        Vec::new(),
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            body.brain.update_activity_from_schedule(world);
            true
        },
    )
}

/// Vanilla `WakeUp`: leaves the bed outside the rest activity.
fn wake_up() -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        Vec::new(),
        |_world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if !body.brain.is_active(Activity::REST) && body.is_sleeping() {
                body.stop_sleeping();
                true
            } else {
                false
            }
        },
    )
}

/// Vanilla `VillagerCalmDown`: once nothing scares it any more, forgets who
/// hurt it and goes back to its schedule.
fn villager_calm_down() -> impl BehaviorControl<VillagerEntity> {
    /// Vanilla `SAFE_DISTANCE_FROM_DANGER`, squared.
    const SAFE_DISTANCE_SQR: f64 = 36.0;
    OneShot::new(
        vec![
            MemoryRequirement::new(&HURT_BY, MemoryStatus::Registered),
            MemoryRequirement::new(&HURT_BY_ENTITY, MemoryStatus::Registered),
            MemoryRequirement::new(&NEAREST_HOSTILE, MemoryStatus::Registered),
        ],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let feel_scared = {
                let memories = body.brain.memories().lock();
                memories.has_value(&HURT_BY)
                    || memories.has_value(&NEAREST_HOSTILE)
                    || memories.get_ref(&HURT_BY_ENTITY).is_some_and(|entity| {
                        entity.position().distance_squared(body.position()) <= SAFE_DISTANCE_SQR
                    })
            };
            if !feel_scared {
                {
                    let mut memories = body.brain.memories().lock();
                    memories.erase(&HURT_BY);
                    memories.erase(&HURT_BY_ENTITY);
                }
                body.brain.update_activity_from_schedule(world);
            }
            true
        },
    )
}

/// Vanilla `GoToClosestVillage`: outside a village, walks to the reachable
/// spot closest to one.
fn go_to_closest_village(
    speed_modifier: f32,
    close_enough_distance: i32,
) -> impl BehaviorControl<VillagerEntity> {
    OneShot::new(
        vec![MemoryRequirement::new(
            &WALK_TARGET,
            MemoryStatus::ValueAbsent,
        )],
        move |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            if body.brain.memories().lock().has_value(&WALK_TARGET)
                || world.is_village(body.block_position())
            {
                return false;
            }
            let sections_to = |pos: steel_utils::BlockPos| {
                world.sections_to_village(steel_utils::SectionPos::from_block_pos(pos))
            };
            let sections_to_village = sections_to(body.block_position());
            let mut target = None;
            for _ in 0..5 {
                let Some(land_pos) =
                    land_random_pos_weighted(body, 15, 7, |pos| -f64::from(sections_to(pos)))
                else {
                    continue;
                };
                let land_sections = sections_to(steel_utils::BlockPos::containing(
                    land_pos.x, land_pos.y, land_pos.z,
                ));
                if land_sections < sections_to_village {
                    target = Some(land_pos);
                    break;
                }
                if land_sections == sections_to_village {
                    target = Some(land_pos);
                }
            }
            if let Some(target) = target {
                body.brain.memories().lock().set(
                    &WALK_TARGET,
                    Some(WalkTarget::to_point(
                        target,
                        speed_modifier,
                        close_enough_distance,
                    )),
                );
            }
            true
        },
    )
}

/// The interaction target of another villager's brain.
fn interaction_target_of(friend: &SharedEntity) -> Option<SharedEntity> {
    friend
        .as_living_entity()?
        .brain_memories()?
        .lock()
        .get(&INTERACTION_TARGET)
}

/// Vanilla `PlayTagWithOtherKids`: babies chase each other around the village.
fn play_tag_with_other_kids() -> impl BehaviorControl<VillagerEntity> {
    /// Vanilla `MAX_CHASERS_PER_TARGET`.
    const MAX_CHASERS_PER_TARGET: usize = 5;
    /// Vanilla `FLEE_SPEED_MODIFIER` and `CHASE_SPEED_MODIFIER`.
    const TAG_SPEED_MODIFIER: f32 = 0.6;
    OneShot::new(
        vec![
            MemoryRequirement::new(&VISIBLE_VILLAGER_BABIES, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&WALK_TARGET, MemoryStatus::ValueAbsent),
            MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
            MemoryRequirement::new(&INTERACTION_TARGET, MemoryStatus::Registered),
        ],
        |world: &Arc<World>, body: &VillagerEntity, _time: i64| {
            let friends = {
                let memories = body.brain.memories().lock();
                if memories.has_value(&WALK_TARGET) {
                    return false;
                }
                let Some(friends) = memories.get(&VISIBLE_VILLAGER_BABIES) else {
                    return false;
                };
                friends
            };
            if rand::random_range(0..10) != 0 {
                return false;
            }
            let my_id = body.id();
            let chased_by_friend = friends.iter().any(|friend| {
                interaction_target_of(friend).is_some_and(|target| target.id() == my_id)
            });
            if chased_by_friend {
                for _ in 0..10 {
                    if let Some(pos) = land_random_pos(body, 20, 8)
                        && world.is_village(steel_utils::BlockPos::containing(pos.x, pos.y, pos.z))
                    {
                        body.brain.memories().lock().set(
                            &WALK_TARGET,
                            Some(WalkTarget::to_point(pos, TAG_SPEED_MODIFIER, 0)),
                        );
                        break;
                    }
                }
                return true;
            }
            // Vanilla `checkHowManyChasersEachFriendHas`, then the least
            // chased friend that has between one and five chasers.
            let mut chased: Vec<(SharedEntity, usize)> = Vec::new();
            for friend in &friends {
                let Some(target) = interaction_target_of(friend) else {
                    continue;
                };
                match chased.iter_mut().find(|(kid, _)| kid.id() == target.id()) {
                    Some((_, count)) => *count += 1,
                    None => chased.push((target, 1)),
                }
            }
            chased.sort_by_key(|(_, count)| *count);
            let kid_to_chase = chased
                .into_iter()
                .find(|(_, count)| (1..=MAX_CHASERS_PER_TARGET).contains(count))
                .map(|(kid, _)| kid)
                .or_else(|| friends.first().cloned());
            if let Some(kid) = kid_to_chase {
                let mut memories = body.brain.memories().lock();
                memories.set(&INTERACTION_TARGET, Some(Arc::clone(&kid)));
                memories.set(
                    &LOOK_TARGET,
                    Some(PositionTracker::entity(Arc::clone(&kid), true)),
                );
                memories.set(
                    &WALK_TARGET,
                    Some(WalkTarget::new(
                        PositionTracker::entity(kid, false),
                        TAG_SPEED_MODIFIER,
                        1,
                    )),
                );
            }
            true
        },
    )
}

/// Vanilla `VillagerPanicTrigger`: switches to panicking while hurt or near
/// a hostile.
struct VillagerPanicTrigger;

fn is_hurt(body: &VillagerEntity) -> bool {
    body.brain.memories().lock().has_value(&HURT_BY)
}

fn has_hostile(body: &VillagerEntity) -> bool {
    body.brain.memories().lock().has_value(&NEAREST_HOSTILE)
}

impl Behavior<VillagerEntity> for VillagerPanicTrigger {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &[]
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        is_hurt(body) || has_hostile(body)
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if !is_hurt(body) && !has_hostile(body) {
            return;
        }
        if !body.brain.is_active(Activity::PANIC) {
            let mut memories = body.brain.memories().lock();
            memories.erase(&PATH);
            memories.erase(&WALK_TARGET);
            memories.erase(&LOOK_TARGET);
            memories.erase(&BREED_TARGET);
            memories.erase(&INTERACTION_TARGET);
        }
        body.brain.set_active_activity_if_possible(Activity::PANIC);
    }

    // TODO(villager brain): vanilla calls `spawnGolemIfNeeded` every 100
    // ticks while panicking; villagers only want a golem after sleeping.
}

/// Vanilla `LookAndFollowTradingPlayerSink`: faces and follows the trading
/// player while they stay close.
struct LookAndFollowTradingPlayerSink {
    speed_modifier: f32,
}

const FOLLOW_TRADING_PLAYER_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&WALK_TARGET, MemoryStatus::Registered),
    MemoryRequirement::new(&LOOK_TARGET, MemoryStatus::Registered),
];

impl LookAndFollowTradingPlayerSink {
    /// Vanilla `checkExtraStartConditions`, with the trading player.
    fn trading_player(body: &VillagerEntity) -> Option<SharedEntity> {
        let player = trading_player_of(body)?;
        if !LivingEntity::is_alive(body)
            || body.is_in_water()
            || body.hurt_time() > 0
            || body.position().distance_squared(player.position()) > 16.0
        {
            return None;
        }
        Some(player)
    }

    fn follow_player(&self, body: &VillagerEntity, player: SharedEntity) {
        let mut memories = body.brain.memories().lock();
        memories.set(
            &WALK_TARGET,
            Some(WalkTarget::new(
                PositionTracker::entity(Arc::clone(&player), false),
                self.speed_modifier,
                2,
            )),
        );
        memories.set(&LOOK_TARGET, Some(PositionTracker::entity(player, true)));
    }
}

impl Behavior<VillagerEntity> for LookAndFollowTradingPlayerSink {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &FOLLOW_TRADING_PLAYER_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (i32::MAX, i32::MAX)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        Self::trading_player(body).is_some()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        Self::trading_player(body).is_some()
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if let Some(player) = Self::trading_player(body) {
            self.follow_player(body, player);
        }
    }

    fn tick(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if let Some(player) = Self::trading_player(body) {
            self.follow_player(body, player);
        }
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        let mut memories = body.brain.memories().lock();
        memories.erase(&WALK_TARGET);
        memories.erase(&LOOK_TARGET);
    }

    fn timed_out(&self, _time: i64, _end_time: i64) -> bool {
        false
    }
}

/// Vanilla `ShowTradesToPlayer`: holds up the trades a nearby player could
/// pay for with the item in their hand.
struct ShowTradesToPlayer {
    player_item_stack: Option<ItemStack>,
    display_items: Vec<ItemStack>,
    cycle_counter: i32,
    display_index: usize,
    look_time: i32,
}

/// Vanilla `ShowTradesToPlayer.MAX_LOOK_TIME`.
const MAX_LOOK_TIME: i32 = 900;
/// Vanilla `ShowTradesToPlayer.STARTING_LOOK_TIME`.
const STARTING_LOOK_TIME: i32 = 40;
/// Vanilla `clearHeldItem`: the main hand's default drop chance.
const DEFAULT_MAIN_HAND_DROP_CHANCE: f32 = 0.085;

const SHOW_TRADES_ENTRY: [MemoryRequirement; 1] = [MemoryRequirement::new(
    &INTERACTION_TARGET,
    MemoryStatus::ValuePresent,
)];

impl ShowTradesToPlayer {
    const fn new() -> Self {
        Self {
            player_item_stack: None,
            display_items: Vec::new(),
            cycle_counter: 0,
            display_index: 0,
            look_time: 0,
        }
    }

    fn interaction_target(body: &VillagerEntity) -> Option<SharedEntity> {
        body.brain.memories().lock().get(&INTERACTION_TARGET)
    }

    /// Vanilla `lookAtTarget`.
    fn look_at_target(body: &VillagerEntity) -> Option<SharedEntity> {
        let target = Self::interaction_target(body)?;
        body.brain.memories().lock().set(
            &LOOK_TARGET,
            Some(PositionTracker::entity(Arc::clone(&target), true)),
        );
        Some(target)
    }

    /// Vanilla `findItemsToDisplay`.
    fn find_items_to_display(&mut self, player: &SharedEntity, body: &VillagerEntity) {
        let mut current = ItemStack::empty();
        if let Some(living) = player.as_living_entity() {
            living.with_equipment_slot(EquipmentSlot::MainHand, &mut |stack| {
                current = stack.clone();
            });
        }
        let changed = self
            .player_item_stack
            .as_ref()
            .is_none_or(|previous| !ItemStack::is_same_item(previous, &current));
        if changed {
            self.display_items.clear();
        }
        if changed && !current.is_empty() {
            body.with_offers(|offers| {
                for offer in offers.iter() {
                    if !offer.is_out_of_stock()
                        && (ItemStack::is_same_item(&current, &offer.cost_a())
                            || ItemStack::is_same_item(&current, &offer.cost_b()))
                    {
                        self.display_items.push(offer.assemble());
                    }
                }
            });
            if let Some(first) = self.display_items.first() {
                self.look_time = MAX_LOOK_TIME;
                display_as_held_item(body, first.clone());
            }
        }
        if changed {
            self.player_item_stack = Some(current);
        }
    }

    /// Vanilla `displayCyclingItems`.
    fn display_cycling_items(&mut self, body: &VillagerEntity) {
        if self.display_items.len() >= 2 {
            self.cycle_counter += 1;
            if self.cycle_counter >= STARTING_LOOK_TIME {
                self.display_index += 1;
                self.cycle_counter = 0;
                if self.display_index > self.display_items.len() - 1 {
                    self.display_index = 0;
                }
                display_as_held_item(body, self.display_items[self.display_index].clone());
            }
        }
    }
}

fn clear_held_item(body: &VillagerEntity) {
    body.set_item_slot(EquipmentSlot::MainHand, ItemStack::empty());
    body.set_equipment_drop_chance(EquipmentSlot::MainHand, DEFAULT_MAIN_HAND_DROP_CHANCE);
}

fn display_as_held_item(body: &VillagerEntity, item_stack: ItemStack) {
    body.set_item_slot(EquipmentSlot::MainHand, item_stack);
    body.set_equipment_drop_chance(EquipmentSlot::MainHand, 0.0);
}

impl Behavior<VillagerEntity> for ShowTradesToPlayer {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &SHOW_TRADES_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (400, 1600)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &VillagerEntity) -> bool {
        let Some(target) = Self::interaction_target(body) else {
            return false;
        };
        is_type(&target, &vanilla_entities::PLAYER)
            && LivingEntity::is_alive(body)
            && target.is_alive()
            && !AgeableMob::is_baby(body)
            && body.position().distance_squared(target.position()) <= 17.0
    }

    fn can_still_use(&mut self, world: &Arc<World>, body: &VillagerEntity, _time: i64) -> bool {
        self.check_extra_start_conditions(world, body) && self.look_time > 0
    }

    fn start(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        Self::look_at_target(body);
        self.cycle_counter = 0;
        self.display_index = 0;
        self.look_time = STARTING_LOOK_TIME;
    }

    fn tick(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        if let Some(target) = Self::look_at_target(body) {
            self.find_items_to_display(&target, body);
        }
        if self.display_items.is_empty() {
            clear_held_item(body);
            self.look_time = self.look_time.min(STARTING_LOOK_TIME);
        } else {
            self.display_cycling_items(body);
        }
        self.look_time -= 1;
    }

    fn stop(&mut self, _world: &Arc<World>, body: &VillagerEntity, _time: i64) {
        body.brain.memories().lock().erase(&INTERACTION_TARGET);
        clear_held_item(body);
        self.player_item_stack = None;
    }
}

/// Vanilla `VillagerHostilesSensor`: remembers the closest visible monster
/// that a villager fears, each within its own distance.
struct VillagerHostilesSensor;

/// Vanilla `ACCEPTABLE_DISTANCE_FROM_HOSTILES`.
fn hostile_distance(entity_type: EntityTypeRef) -> Option<f32> {
    [
        (&vanilla_entities::DROWNED, 8.0),
        (&vanilla_entities::EVOKER, 12.0),
        (&vanilla_entities::HUSK, 8.0),
        (&vanilla_entities::ILLUSIONER, 12.0),
        (&vanilla_entities::PILLAGER, 15.0),
        (&vanilla_entities::RAVAGER, 12.0),
        (&vanilla_entities::VEX, 8.0),
        (&vanilla_entities::VINDICATOR, 10.0),
        (&vanilla_entities::ZOGLIN, 10.0),
        (&vanilla_entities::ZOMBIE, 8.0),
        (&vanilla_entities::ZOMBIE_VILLAGER, 8.0),
    ]
    .into_iter()
    .find(|(hostile, _)| *hostile == entity_type)
    .map(|(_, distance)| distance)
}

impl Sensor<VillagerEntity> for VillagerHostilesSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_HOSTILE);
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &VillagerEntity) {
        let memories = body.brain.memories();
        // The memories lock must be released before searching: visibility
        // checks read the body's attack target.
        let visible = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES);
        let position = body.position();
        let nearest = visible.and_then(|visible| {
            visible.find_closest(|entity| {
                hostile_distance(entity.entity_type()).is_some_and(|distance| {
                    entity.position().distance_squared(position) <= f64::from(distance * distance)
                })
            })
        });
        memories.lock().set(&NEAREST_HOSTILE, nearest);
    }
}

/// Vanilla `VillagerBabiesSensor`: remembers the visible villager babies.
struct VillagerBabiesSensor;

impl Sensor<VillagerEntity> for VillagerBabiesSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&VISIBLE_VILLAGER_BABIES);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &VillagerEntity) {
        let memories = body.brain.memories();
        // The memories lock must be released before searching: visibility
        // checks read the body's attack target.
        let visible = memories.lock().get(&NEAREST_VISIBLE_LIVING_ENTITIES);
        let babies: Vec<SharedEntity> = visible.map_or_else(Vec::new, |visible| {
            visible
                .find_all(|entity| {
                    is_type(entity, &vanilla_entities::VILLAGER)
                        && entity.as_living_entity().is_some_and(LivingEntity::is_baby)
                })
                .cloned()
                .collect()
        });
        memories.lock().set(&VISIBLE_VILLAGER_BABIES, Some(babies));
    }
}

/// Vanilla `GolemSensor`: remembers for a while that an iron golem is near.
struct GolemSensor;

/// Vanilla `GolemSensor.GOLEM_SCAN_RATE`.
const GOLEM_SCAN_RATE: i32 = 200;
/// Vanilla `GolemSensor.MEMORY_TIME_TO_LIVE`.
const GOLEM_MEMORY_TIME_TO_LIVE: i64 = 599;

/// Vanilla `GolemSensor.golemDetected`.
fn golem_detected(memories: &mut Memories) {
    memories.set_with_expiry(&GOLEM_DETECTED_RECENTLY, true, GOLEM_MEMORY_TIME_TO_LIVE);
}

impl Sensor<VillagerEntity> for GolemSensor {
    fn scan_rate(&self) -> i32 {
        GOLEM_SCAN_RATE
    }

    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_LIVING_ENTITIES);
        memories.register(&GOLEM_DETECTED_RECENTLY);
    }

    fn do_tick(&mut self, _world: &Arc<World>, body: &VillagerEntity) {
        let mut memories = body.brain.memories().lock();
        let golem_present = memories
            .get_ref(&NEAREST_LIVING_ENTITIES)
            .is_some_and(|entities| {
                entities
                    .iter()
                    .any(|entity| is_type(entity, &vanilla_entities::IRON_GOLEM))
            });
        if golem_present {
            golem_detected(&mut memories);
        }
    }
}

mod farm;
mod poi;
mod work;

#[cfg(test)]
mod tests;
