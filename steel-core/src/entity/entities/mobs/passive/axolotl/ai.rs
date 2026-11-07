//! Vanilla `AxolotlAi`, `PlayDead`, `ValidatePlayDead` and
//! `AxolotlAttackablesSensor`.

use std::sync::Arc;

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::fluid::FluidStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, vanilla_entities, vanilla_mob_effects};

use steel_utils::Identifier;

use super::AxolotlEntity;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::behavior::{Behavior, OneShot, TimedBehavior};
use crate::entity::ai::brain::behaviors::gate::{GateBehavior, OrderPolicy, RunningPolicy};
use crate::entity::ai::brain::behaviors::{
    AnimalMakeLove, CountDownCooldownTicks, FollowTemptation, LookAtTargetSink, MoveToTargetSink,
    UniformInt, baby_follow_adult, erase_memory_if, is_breeding, melee_attack, random_stroll,
    random_swim, run_one, set_entity_look_target_sometimes,
    set_walk_target_from_attack_target_if_target_out_of_reach, set_walk_target_from_look_target,
    start_attacking, stop_attacking_if_target_invalid, try_find_liquid,
};
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{
    ATTACK_TARGET, HAS_HUNTING_COOLDOWN, HURT_BY_ENTITY, LOOK_TARGET, NEAREST_ATTACKABLE,
    NEAREST_VISIBLE_ADULT, NEAREST_VISIBLE_LIVING_ENTITIES, PLAY_DEAD_TICKS,
    TEMPTATION_COOLDOWN_TICKS, WALK_TARGET,
};
use crate::entity::ai::brain::nearest_visible::is_entity_attackable;
use crate::entity::ai::brain::sensors::{
    AdultSensor, HurtBySensor, NearestLivingEntitySensor, TemptingSensor,
};
use crate::entity::ai::brain::{ActivityData, BehaviorControl, Memories, MemoryStatus, Sensor};
use crate::entity::{Entity, LivingEntity, MobEffectInstance, SharedEntity};
use crate::world::World;

/// Vanilla `Axolotl.TOTAL_PLAYDEAD_TIME`.
pub(super) const TOTAL_PLAYDEAD_TIME: i32 = 200;
/// Vanilla `AxolotlAi.ADULT_FOLLOW_RANGE`.
const ADULT_FOLLOW_RANGE: UniformInt = UniformInt::of(5, 16);
const SPEED_MULTIPLIER_WHEN_MAKING_LOVE: f32 = 0.2;
const SPEED_MULTIPLIER_ON_LAND: f32 = 0.15;
const SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER: f32 = 0.5;
const SPEED_MULTIPLIER_WHEN_CHASING_IN_WATER: f32 = 0.6;
const SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT_IN_WATER: f32 = 0.6;
/// Vanilla `AxolotlAi.updateActivity`: the hunting cooldown after a fight.
const HUNTING_COOLDOWN_TICKS: i64 = 2400;
/// Vanilla `AxolotlAttackablesSensor.TARGET_DETECTION_DISTANCE`.
const TARGET_DETECTION_DISTANCE: f64 = 8.0;
const LOOK_AT_PLAYER_DISTANCE: f32 = 6.0;
const LOOK_AT_PLAYER_INTERVAL: UniformInt = UniformInt::of(30, 60);
const FIND_WATER_RANGE: i32 = 6;
static AXOLOTL_TRIES_TO_FIND: Identifier = FluidTag::AXOLOTL_TRIES_TO_FIND;

/// Vanilla `SensorType.FOOD_TEMPTATIONS` with the axolotl's `isFood`.
fn is_tempting(item: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item.item(), &ItemTag::AXOLOTL_FOOD)
}

/// The sensors vanilla's axolotl `Brain.provider` lists.
pub(super) fn sensors() -> Vec<Box<dyn Sensor<AxolotlEntity>>> {
    vec![
        Box::new(NearestLivingEntitySensor),
        Box::new(AdultSensor),
        Box::new(HurtBySensor),
        Box::new(AxolotlAttackablesSensor),
        Box::new(TemptingSensor::<AxolotlEntity>::new(is_tempting)),
    ]
}

/// Vanilla `AxolotlAi.getActivities`.
pub(super) fn activities() -> Vec<ActivityData<AxolotlEntity>> {
    vec![
        core_activity(),
        idle_activity(),
        fight_activity(),
        play_dead_activity(),
    ]
}

fn core_activity() -> ActivityData<AxolotlEntity> {
    ActivityData::with_first_priority(
        Activity::CORE,
        0,
        vec![
            Box::new(TimedBehavior::new(LookAtTargetSink::new(45, 90))),
            Box::new(TimedBehavior::new(MoveToTargetSink::new())),
            Box::new(validate_play_dead()),
            Box::new(TimedBehavior::new(CountDownCooldownTicks::new(
                &TEMPTATION_COOLDOWN_TICKS,
            ))),
        ],
    )
}

fn idle_activity() -> ActivityData<AxolotlEntity> {
    ActivityData::new(
        Activity::IDLE,
        vec![
            (
                0,
                Box::new(set_entity_look_target_sometimes(
                    LOOK_AT_PLAYER_DISTANCE,
                    LOOK_AT_PLAYER_INTERVAL,
                    |entity: &dyn LivingEntity| entity.entity_type() == &vanilla_entities::PLAYER,
                )),
            ),
            (
                1,
                Box::new(TimedBehavior::new(AnimalMakeLove::with_speed(
                    &vanilla_entities::AXOLOTL,
                    SPEED_MULTIPLIER_WHEN_MAKING_LOVE,
                    2,
                ))),
            ),
            (
                2,
                Box::new(run_one(
                    Vec::new(),
                    vec![
                        (
                            Box::new(TimedBehavior::new(FollowTemptation::new(speed_modifier))),
                            1,
                        ),
                        (
                            Box::new(baby_follow_adult(
                                ADULT_FOLLOW_RANGE,
                                speed_modifier_following_adult,
                                &NEAREST_VISIBLE_ADULT,
                                false,
                            )),
                            1,
                        ),
                    ],
                )),
            ),
            (
                3,
                Box::new(start_attacking(
                    |_world: &Arc<World>, body: &AxolotlEntity| {
                        find_nearest_valid_attack_target(body)
                    },
                )),
            ),
            (
                3,
                Box::new(try_find_liquid(
                    FIND_WATER_RANGE,
                    SPEED_MULTIPLIER_ON_LAND,
                    &AXOLOTL_TRIES_TO_FIND,
                )),
            ),
            (
                4,
                Box::new(GateBehavior::new(
                    vec![MemoryRequirement::new(
                        &WALK_TARGET,
                        MemoryStatus::ValueAbsent,
                    )],
                    Vec::new(),
                    OrderPolicy::Ordered,
                    RunningPolicy::TryAll,
                    vec![
                        (
                            Box::new(random_swim(SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER)),
                            2,
                        ),
                        (Box::new(random_stroll(SPEED_MULTIPLIER_ON_LAND, false)), 2),
                        (
                            Box::new(set_walk_target_from_look_target(
                                can_set_walk_target_from_look_target,
                                speed_modifier,
                                3,
                            )),
                            3,
                        ),
                        (Box::new(trigger_if(Entity::is_in_water)), 5),
                        (Box::new(trigger_if(Entity::on_ground)), 5),
                    ],
                )),
            ),
        ],
    )
}

fn fight_activity() -> ActivityData<AxolotlEntity> {
    ActivityData::with_first_priority(
        Activity::FIGHT,
        0,
        vec![
            Box::new(stop_attacking_if_target_invalid(
                |world: &Arc<World>, body: &AxolotlEntity, target: &SharedEntity| {
                    on_stop_attacking(world, body, target);
                },
            )),
            Box::new(set_walk_target_from_attack_target_if_target_out_of_reach(
                speed_modifier_chasing,
            )),
            Box::new(melee_attack(20)),
            Box::new(erase_memory_if(
                |body: &AxolotlEntity| is_breeding(body),
                &ATTACK_TARGET,
            )),
        ],
    )
    .while_memory_present(&ATTACK_TARGET)
}

fn play_dead_activity() -> ActivityData<AxolotlEntity> {
    ActivityData::new(
        Activity::PLAY_DEAD,
        vec![
            (0, Box::new(TimedBehavior::new(PlayDead::new()))),
            (
                1,
                Box::new(erase_memory_if(
                    |body: &AxolotlEntity| is_breeding(body),
                    &PLAY_DEAD_TICKS,
                )),
            ),
        ],
    )
    .with_conditions(vec![MemoryRequirement::new(
        &PLAY_DEAD_TICKS,
        MemoryStatus::ValuePresent,
    )])
    .erasing_when_stopped(vec![&PLAY_DEAD_TICKS])
}

/// Vanilla `BehaviorBuilder.triggerIf`.
fn trigger_if(
    predicate: impl Fn(&AxolotlEntity) -> bool + Send + 'static,
) -> impl BehaviorControl<AxolotlEntity> {
    OneShot::new(
        Vec::new(),
        move |_world: &Arc<World>, body: &AxolotlEntity, _time: i64| predicate(body),
    )
}

/// Vanilla `AxolotlAi.canSetWalkTargetFromLookTarget`.
fn can_set_walk_target_from_look_target(body: &AxolotlEntity) -> bool {
    let Some(world) = body.level() else {
        return false;
    };
    let Some(look_target) = body.brain_memories_lock().get(&LOOK_TARGET) else {
        return false;
    };
    let pos = look_target.current_block_position();
    world.get_block_state(pos).get_fluid_state().is_water() == body.is_in_water()
}

/// Vanilla `AxolotlAi.updateActivity`: playing dead holds until it ends,
/// and leaving a fight starts the hunting cooldown.
pub(super) fn update_activity(body: &AxolotlEntity) {
    let brain = body.brain();
    let old_activity = brain.active_non_core_activity();
    if old_activity != Some(Activity::PLAY_DEAD) {
        brain.set_active_activity_to_first_valid(&[
            Activity::PLAY_DEAD,
            Activity::FIGHT,
            Activity::IDLE,
        ]);
        if old_activity == Some(Activity::FIGHT)
            && brain.active_non_core_activity() != Some(Activity::FIGHT)
        {
            brain.memories().lock().set_with_expiry(
                &HAS_HUNTING_COOLDOWN,
                true,
                HUNTING_COOLDOWN_TICKS,
            );
        }
    }
}

fn speed_modifier_chasing(body: &AxolotlEntity) -> f32 {
    if body.is_in_water() {
        SPEED_MULTIPLIER_WHEN_CHASING_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

fn speed_modifier_following_adult(body: &AxolotlEntity) -> f32 {
    if body.is_in_water() {
        SPEED_MULTIPLIER_WHEN_FOLLOWING_ADULT_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

fn speed_modifier(body: &AxolotlEntity) -> f32 {
    if body.is_in_water() {
        SPEED_MULTIPLIER_WHEN_IDLING_IN_WATER
    } else {
        SPEED_MULTIPLIER_ON_LAND
    }
}

/// Vanilla `AxolotlAi.findNearestValidAttackTarget`.
fn find_nearest_valid_attack_target(body: &AxolotlEntity) -> Option<SharedEntity> {
    if is_breeding(body) {
        return None;
    }
    body.brain_memories_lock().get(&NEAREST_ATTACKABLE)
}

/// Vanilla `Axolotl.onStopAttacking`: a target a nearby player killed earns
/// that player the axolotl's support.
fn on_stop_attacking(world: &Arc<World>, body: &AxolotlEntity, target: &SharedEntity) {
    let Some(target) = target.as_living_entity() else {
        return;
    };
    if !target.is_dead_or_dying() {
        return;
    }
    let Some(last_damage_source) = target.last_damage_source() else {
        return;
    };
    let Some(player) = last_damage_source
        .causing_entity()
        .and_then(|entity| entity.as_player())
    else {
        return;
    };
    let search_box = body
        .bounding_box()
        .inflate(super::PLAYER_REGEN_DETECTION_RANGE);
    if Arc::ptr_eq(&player.get_world(), world) && player.bounding_box().intersects(search_box) {
        body.apply_supporting_effects(player);
    }
}

/// Vanilla `AxolotlAttackablesSensor`: the closest visible mob in water
/// within 8 blocks that the axolotl always fights, or hunts when off its
/// hunting cooldown.
struct AxolotlAttackablesSensor;

impl Sensor<AxolotlEntity> for AxolotlAttackablesSensor {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&NEAREST_ATTACKABLE);
        memories.register(&NEAREST_VISIBLE_LIVING_ENTITIES);
        memories.register(&HAS_HUNTING_COOLDOWN);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &AxolotlEntity) {
        let memories = body.brain().memories();
        let (visible, hunting_cooldown) = {
            let memories = memories.lock();
            (
                memories.get(&NEAREST_VISIBLE_LIVING_ENTITIES),
                memories.has_value(&HAS_HUNTING_COOLDOWN),
            )
        };
        let nearest = visible.and_then(|visible| {
            visible.find_closest(|entity| {
                entity
                    .as_living_entity()
                    .is_some_and(|mob| is_matching_entity(world, body, mob, hunting_cooldown))
            })
        });
        memories.lock().set(&NEAREST_ATTACKABLE, nearest);
    }
}

fn is_matching_entity(
    world: &World,
    body: &AxolotlEntity,
    mob: &dyn LivingEntity,
    hunting_cooldown: bool,
) -> bool {
    let is_close = mob.position().distance_squared(body.position())
        <= TARGET_DETECTION_DISTANCE * TARGET_DETECTION_DISTANCE;
    let entity_types = &REGISTRY.entity_types;
    let is_hostile_target =
        entity_types.is_in_tag(mob.entity_type(), &EntityTypeTag::AXOLOTL_ALWAYS_HOSTILES);
    let is_hunt_target = !hunting_cooldown
        && entity_types.is_in_tag(mob.entity_type(), &EntityTypeTag::AXOLOTL_HUNT_TARGETS);
    is_close
        && mob.is_in_water()
        && (is_hostile_target || is_hunt_target)
        && is_entity_attackable(world, body, mob)
}

/// Vanilla `PlayDead`: lies still in water and regenerates while the play
/// dead countdown runs.
struct PlayDead {
    entry_condition: [MemoryRequirement; 2],
}

impl PlayDead {
    const fn new() -> Self {
        Self {
            entry_condition: [
                MemoryRequirement::new(&PLAY_DEAD_TICKS, MemoryStatus::ValuePresent),
                MemoryRequirement::new(&HURT_BY_ENTITY, MemoryStatus::ValuePresent),
            ],
        }
    }
}

impl Behavior<AxolotlEntity> for PlayDead {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &self.entry_condition
    }

    fn duration(&self) -> (i32, i32) {
        (TOTAL_PLAYDEAD_TIME, TOTAL_PLAYDEAD_TIME)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &AxolotlEntity) -> bool {
        body.is_in_water()
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &AxolotlEntity, _time: i64) -> bool {
        body.is_in_water() && body.brain_memories_lock().has_value(&PLAY_DEAD_TICKS)
    }

    fn start(&mut self, _world: &Arc<World>, body: &AxolotlEntity, _time: i64) {
        {
            let mut memories = body.brain_memories_lock();
            memories.erase(&WALK_TARGET);
            memories.erase(&LOOK_TARGET);
        }
        body.add_mob_effect(MobEffectInstance::with_duration(
            vanilla_mob_effects::REGENERATION,
            TOTAL_PLAYDEAD_TIME,
            0,
        ));
    }
}

/// Vanilla `ValidatePlayDead`: counts the play dead ticks down, then wakes
/// the axolotl back into its default activity.
fn validate_play_dead() -> impl BehaviorControl<AxolotlEntity> {
    OneShot::new(
        vec![
            MemoryRequirement::new(&PLAY_DEAD_TICKS, MemoryStatus::ValuePresent),
            MemoryRequirement::new(&HURT_BY_ENTITY, MemoryStatus::Registered),
        ],
        |_world: &Arc<World>, body: &AxolotlEntity, _time: i64| {
            {
                let mut memories = body.brain_memories_lock();
                let Some(ticks) = memories.get(&PLAY_DEAD_TICKS) else {
                    return false;
                };
                if ticks > 0 {
                    memories.set(&PLAY_DEAD_TICKS, Some(ticks - 1));
                    return true;
                }
                memories.erase(&PLAY_DEAD_TICKS);
                memories.erase(&HURT_BY_ENTITY);
            }
            body.brain().use_default_activity();
            true
        },
    )
}
