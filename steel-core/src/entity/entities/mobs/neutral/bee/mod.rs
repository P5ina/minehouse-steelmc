//! Vanilla bee: a flying neutral animal that pollinates flowers, carries
//! nectar home to its hive, grows crops it flies over and dies after stinging.

mod goals;

use std::f32::consts::PI;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, DoubleBlockHalf};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::BeeEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_damage_types, vanilla_items, vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::{Difficulty, InteractionHand};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Downcast as _, DowncastType, DowncastTypeKey};

use goals::{
    BeeAttackGoal, BeeBecomeAngryTargetGoal, BeeEnterHiveGoal, BeeGoToHiveGoal,
    BeeGoToKnownFlowerGoal, BeeGrowCropGoal, BeeLocateHiveGoal, BeePollinateGoal, BeeWanderGoal,
    ValidateFlowerGoal, ValidateHiveGoal, bee_hurt_by_other_goal,
};

use crate::behavior::InteractionResult;
use crate::block_entity::SharedBlockEntity;
use crate::block_entity::entities::BeehiveBlockEntity;
use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::ai::flying_move::FlyingMoveControl;
use crate::entity::ai::goal::random_pos::air_random_pos_towards;
use crate::entity::ai::goal::{
    BreedGoal, FloatGoal, FollowParentGoal, ResetUniversalAngerTargetGoal, TemptGoal,
};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::neutral_mob::{NeutralMob, NeutralMobState};
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    MobEffectInstance, PathfinderMob, SharedEntity, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, World};

/// Vanilla `FlyingMoveControl(this, 20, true)`.
const BEE_MOVE_CONTROL: FlyingMoveControl = FlyingMoveControl {
    max_turn: 20,
    hovers_in_place: true,
};
/// Vanilla `TICKS_PER_FLAP`: `Mth.ceil(1.4959966F)`.
const TICKS_PER_FLAP: i32 = 2;
const FLAG_ROLL: i8 = 2;
const FLAG_HAS_STUNG: i8 = 4;
const FLAG_HAS_NECTAR: i8 = 8;
/// A bee that has stung dies within this many ticks.
const STING_DEATH_COUNTDOWN: i32 = 1200;
/// After this long without nectar a bee flies back to its remembered flower.
const TICKS_BEFORE_GOING_TO_KNOWN_FLOWER: i32 = 600;
/// After this long without nectar a bee gives up and heads home.
const TICKS_WITHOUT_NECTAR_BEFORE_GOING_HOME: i32 = 3600;
/// A bee drowns after this many ticks under water.
const MAX_UNDER_WATER_TICKS: i32 = 20;
/// Crops a bee can grow on one load of nectar.
const MAX_CROPS_GROWABLE: i32 = 10;
const POISON_SECONDS_NORMAL: i32 = 10;
const POISON_SECONDS_HARD: i32 = 18;
/// Hives and flowers farther than this are forgotten.
const TOO_FAR_DISTANCE: i32 = 48;
const HIVE_CLOSE_ENOUGH_DISTANCE: f64 = 2.0;
const RESTRICTED_WANDER_DISTANCE_REDUCTION: i32 = 24;
const DEFAULT_WANDER_DISTANCE_REDUCTION: i32 = 16;
/// Closer than this the bee paths straight to its hive.
const PATHFIND_TO_HIVE_WHEN_CLOSER_THAN: i32 = 16;
/// Radius searched for hives with space.
const HIVE_SEARCH_DISTANCE: i32 = 20;
const COOLDOWN_BEFORE_LOCATING_NEW_HIVE: i32 = 200;
const COOLDOWN_BEFORE_LOCATING_NEW_FLOWER: i32 = 200;
const MIN_FIND_FLOWER_RETRY_COOLDOWN: i32 = 20;
const MAX_FIND_FLOWER_RETRY_COOLDOWN: i32 = 60;
/// Vanilla `PERSISTENT_ANGER_TIME`: 20 to 39 seconds.
const PERSISTENT_ANGER_TICKS_MIN: i64 = 20 * 20;
const PERSISTENT_ANGER_TICKS_MAX: i64 = 39 * 20;
/// The bee rolls when its target is closer than this squared distance.
const ROLL_DISTANCE_SQR: f64 = 4.0;
const SOUND_VOLUME: f32 = 0.4;
/// Wither from a wither rose fed to a bee.
const WITHER_ROSE_EFFECT_TICKS: i32 = 40;
/// Poison from an open eyeblossom fed to a bee.
const EYEBLOSSOM_EFFECT_TICKS: i32 = 25;

const TAG_CROPS_GROWN_SINCE_POLLINATION: &str = "CropsGrownSincePollination";
const TAG_CANNOT_ENTER_HIVE_TICKS: &str = "CannotEnterHiveTicks";
const TAG_TICKS_SINCE_POLLINATION: &str = "TicksSincePollination";
const TAG_HAS_STUNG: &str = "HasStung";
const TAG_HAS_NECTAR: &str = "HasNectar";
const TAG_FLOWER_POS: &str = "flower_pos";
const TAG_HIVE_POS: &str = "hive_pos";

/// Bee state vanilla keeps in plain fields of `Bee` and its goals.
#[derive(Debug, Default)]
struct BeeState {
    time_since_sting: i32,
    ticks_without_nectar_since_exiting_hive: i32,
    stay_out_of_hive_countdown: i32,
    num_crops_grown_since_pollination: i32,
    remaining_cooldown_before_locating_new_hive: i32,
    remaining_cooldown_before_locating_new_flower: i32,
    saved_flower_pos: Option<BlockPos>,
    hive_pos: Option<BlockPos>,
    under_water_ticks: i32,
    /// Vanilla `BeePollinateGoal.pollinating`, read by navigation and look control.
    pollinating: bool,
    /// Vanilla `BeeGoToHiveGoal.blacklistedTargets`.
    blacklisted_hives: Vec<BlockPos>,
}

#[entity_behavior(class = "Bee")]
/// Vanilla bee.
pub struct BeeEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    anger: NeutralMobState,
    entity_data: SyncMutex<BeeEntityData>,
    state: SyncMutex<BeeState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `BeeEntity`.
unsafe impl DowncastType for BeeEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/bee");
}

fn random_flower_retry_cooldown() -> i32 {
    rand::random_range(MIN_FIND_FLOWER_RETRY_COOLDOWN..=MAX_FIND_FLOWER_RETRY_COOLDOWN)
}

fn block_pos_tag(pos: BlockPos) -> NbtTag {
    NbtTag::IntArray(vec![pos.x(), pos.y(), pos.z()])
}

fn read_block_pos(nbt: BorrowedNbtCompoundView<'_, '_>, key: &str) -> Option<BlockPos> {
    let values = nbt.int_array(key)?;
    (values.len() == 3).then(|| BlockPos::new(values[0], values[1], values[2]))
}

/// Vanilla `Bee.attractsBees`: a dry bee-attractive block, and only the top
/// half of a sunflower.
#[must_use]
pub fn attracts_bees(state: BlockStateId) -> bool {
    if !state.get_block().has_tag(&BlockTag::BEE_ATTRACTIVE) {
        return false;
    }
    if state
        .try_get_value(&BlockStateProperties::WATERLOGGED)
        .unwrap_or(false)
    {
        return false;
    }
    if state.get_block() == &vanilla_blocks::SUNFLOWER {
        return state.get_value(&BlockStateProperties::DOUBLE_BLOCK_HALF) == DoubleBlockHalf::Upper;
    }
    true
}

impl BeeEntity {
    /// Creates a new bee at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a bee from persisted base entity state.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        let ageable_base = AgeableMobBase::new();
        let animal_base = AnimalBase::new();
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::Fire, -1.0);
            malus.set(PathType::Water, -1.0);
            malus.set(PathType::WaterBorder, 16.0);
            malus.set(PathType::Cocoa, -1.0);
            malus.set(PathType::Fence, -1.0);
        }
        let follow_range = living_base
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(vanilla_attributes::FOLLOW_RANGE.default_value);
        {
            let mut navigation = PathNavigation::flying();
            navigation.set_can_open_doors(false);
            navigation.set_can_float(false);
            navigation.set_required_path_length(f64::from(TOO_FAR_DISTANCE) as f32, follow_range);
            *mob_base.navigation().lock() = navigation;
        }
        let mut entity_data = BeeEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        entity_data.anger_end_time.set(-1);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, BeeAttackGoal::new());
            goal_selector.add_goal(1, BeeEnterHiveGoal);
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(
                3,
                TemptGoal::new(
                    1.25,
                    |item_stack| {
                        REGISTRY
                            .items
                            .is_in_tag(item_stack.item(), &ItemTag::BEE_FOOD)
                    },
                    false,
                ),
            );
            goal_selector.add_goal(3, ValidateHiveGoal::new());
            goal_selector.add_goal(3, ValidateFlowerGoal::new());
            goal_selector.add_goal(4, BeePollinateGoal::new());
            goal_selector.add_goal(5, FollowParentGoal::new(1.25));
            goal_selector.add_goal(5, BeeLocateHiveGoal);
            goal_selector.add_goal(5, BeeGoToHiveGoal::new());
            goal_selector.add_goal(6, BeeGoToKnownFlowerGoal::new());
            goal_selector.add_goal(7, BeeGrowCropGoal);
            goal_selector.add_goal(8, BeeWanderGoal);
            // Vanilla registers this after `createNavigation`, so it turns
            // floating back on.
            goal_selector.add_goal(9, FloatGoal::new(&mob_base));

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, bee_hurt_by_other_goal());
            target_selector.add_goal(2, BeeBecomeAngryTargetGoal::new());
            target_selector.add_goal(3, ResetUniversalAngerTargetGoal::new(true));
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base,
            animal_base,
            anger: NeutralMobState::new(),
            entity_data: SyncMutex::new(entity_data),
            state: SyncMutex::new(BeeState {
                remaining_cooldown_before_locating_new_flower: random_flower_retry_cooldown(),
                ..BeeState::default()
            }),
        }
    }

    fn get_flag(&self, flag: i8) -> bool {
        *self.entity_data.lock().flags.get() & flag != 0
    }

    fn set_flag(&self, flag: i8, value: bool) {
        let mut entity_data = self.entity_data.lock();
        let flags = *entity_data.flags.get();
        entity_data
            .flags
            .set(if value { flags | flag } else { flags & !flag });
    }

    /// Vanilla `hasNectar`.
    #[must_use]
    pub fn has_nectar(&self) -> bool {
        self.get_flag(FLAG_HAS_NECTAR)
    }

    fn set_has_nectar(&self, has_nectar: bool) {
        if has_nectar {
            self.reset_ticks_without_nectar_since_exiting_hive();
        }
        self.set_flag(FLAG_HAS_NECTAR, has_nectar);
    }

    /// Vanilla `hasStung`.
    #[must_use]
    pub fn has_stung(&self) -> bool {
        self.get_flag(FLAG_HAS_STUNG)
    }

    fn set_has_stung(&self, has_stung: bool) {
        self.set_flag(FLAG_HAS_STUNG, has_stung);
    }

    /// Vanilla `isRolling`: the bee rolls as it closes in to sting.
    #[must_use]
    pub fn is_rolling(&self) -> bool {
        self.get_flag(FLAG_ROLL)
    }

    fn set_rolling(&self, rolling: bool) {
        self.set_flag(FLAG_ROLL, rolling);
    }

    /// Vanilla `getSavedFlowerPos`.
    #[must_use]
    pub fn saved_flower_pos(&self) -> Option<BlockPos> {
        self.state.lock().saved_flower_pos
    }

    /// Vanilla `hasSavedFlowerPos`.
    #[must_use]
    pub fn has_saved_flower_pos(&self) -> bool {
        self.state.lock().saved_flower_pos.is_some()
    }

    /// Vanilla `setSavedFlowerPos`.
    pub fn set_saved_flower_pos(&self, pos: BlockPos) {
        self.state.lock().saved_flower_pos = Some(pos);
    }

    /// Vanilla `getHivePos`.
    #[must_use]
    pub fn hive_pos(&self) -> Option<BlockPos> {
        self.state.lock().hive_pos
    }

    /// Vanilla `hasHive`.
    #[must_use]
    pub fn has_hive(&self) -> bool {
        self.state.lock().hive_pos.is_some()
    }

    /// Vanilla `setHivePos`.
    pub fn set_hive_pos(&self, pos: BlockPos) {
        self.state.lock().hive_pos = Some(pos);
    }

    fn clear_hive_pos(&self) {
        self.state.lock().hive_pos = None;
    }

    /// Vanilla `setStayOutOfHiveCountdown`.
    pub fn set_stay_out_of_hive_countdown(&self, ticks: i32) {
        self.state.lock().stay_out_of_hive_countdown = ticks;
    }

    /// Vanilla `resetTicksWithoutNectarSinceExitingHive`.
    pub fn reset_ticks_without_nectar_since_exiting_hive(&self) {
        self.state.lock().ticks_without_nectar_since_exiting_hive = 0;
    }

    /// Vanilla `dropOffNectar`: called when the bee leaves the hive after
    /// delivering its nectar.
    pub fn drop_off_nectar(&self) {
        self.set_has_nectar(false);
        self.state.lock().num_crops_grown_since_pollination = 0;
    }

    fn crops_grown_since_pollination(&self) -> i32 {
        self.state.lock().num_crops_grown_since_pollination
    }

    fn increment_crops_grown_since_pollination(&self) {
        self.state.lock().num_crops_grown_since_pollination += 1;
    }

    fn is_pollinating(&self) -> bool {
        self.state.lock().pollinating
    }

    fn set_pollinating(&self, pollinating: bool) {
        self.state.lock().pollinating = pollinating;
    }

    fn is_tired_of_looking_for_nectar(&self) -> bool {
        self.state.lock().ticks_without_nectar_since_exiting_hive
            > TICKS_WITHOUT_NECTAR_BEFORE_GOING_HOME
    }

    fn wants_to_go_to_known_flower(&self) -> bool {
        self.state.lock().ticks_without_nectar_since_exiting_hive
            > TICKS_BEFORE_GOING_TO_KNOWN_FLOWER
    }

    /// Vanilla `dropHive`.
    fn drop_hive(&self) {
        let mut state = self.state.lock();
        state.hive_pos = None;
        state.remaining_cooldown_before_locating_new_hive = COOLDOWN_BEFORE_LOCATING_NEW_HIVE;
    }

    /// Vanilla `dropFlower`.
    fn drop_flower(&self) {
        let mut state = self.state.lock();
        state.saved_flower_pos = None;
        state.remaining_cooldown_before_locating_new_flower = random_flower_retry_cooldown();
    }

    /// Vanilla `closerThan(BlockPos, int)`.
    fn closer_than(&self, pos: BlockPos, distance: i32) -> bool {
        let bee_pos = self.block_position();
        let dx = f64::from(pos.x() - bee_pos.x());
        let dy = f64::from(pos.y() - bee_pos.y());
        let dz = f64::from(pos.z() - bee_pos.z());
        dx * dx + dy * dy + dz * dz < f64::from(distance) * f64::from(distance)
    }

    /// Vanilla `isTooFarAway`.
    fn is_too_far_away(&self, pos: BlockPos) -> bool {
        !self.closer_than(pos, TOO_FAR_DISTANCE)
    }

    /// Vanilla `getBeehiveBlockEntity`.
    fn beehive_block_entity(&self) -> Option<SharedBlockEntity> {
        let hive_pos = self.hive_pos()?;
        if self.is_too_far_away(hive_pos) {
            return None;
        }
        let block_entity = self.level()?.get_block_entity(hive_pos)?;
        block_entity
            .downcast_ref::<BeehiveBlockEntity>()
            .is_some()
            .then_some(block_entity)
    }

    /// Vanilla `isHiveValid`.
    fn is_hive_valid(&self) -> bool {
        self.beehive_block_entity().is_some()
    }

    /// Vanilla `isHiveNearFire`.
    fn is_hive_near_fire(&self) -> bool {
        self.beehive_block_entity().is_some_and(|block_entity| {
            block_entity
                .downcast_ref::<BeehiveBlockEntity>()
                .is_some_and(BeehiveBlockEntity::is_fire_nearby)
        })
    }

    /// Vanilla `doesHiveHaveSpace`.
    fn does_hive_have_space(world: &World, pos: BlockPos) -> bool {
        world.get_block_entity(pos).is_some_and(|block_entity| {
            block_entity
                .downcast_ref::<BeehiveBlockEntity>()
                .is_some_and(|hive| !hive.is_full())
        })
    }

    /// Vanilla `wantsToEnterHive`.
    fn wants_to_enter_hive(&self) -> bool {
        let stay_out = self.state.lock().stay_out_of_hive_countdown;
        if stay_out > 0 || self.is_pollinating() || self.has_stung() || self.target().is_some() {
            return false;
        }
        let wants_to_enter_hive = self.has_nectar()
            || self.is_tired_of_looking_for_nectar()
            || self.level().is_some_and(|world| world.bees_stay_in_hive());
        wants_to_enter_hive && !self.is_hive_near_fire()
    }

    /// Vanilla `pathfindRandomlyTowards`.
    fn pathfind_randomly_towards(&self, target_pos: BlockPos) {
        let target = DVec3::new(
            f64::from(target_pos.x()) + 0.5,
            f64::from(target_pos.y()),
            f64::from(target_pos.z()) + 0.5,
        );
        let bee_pos = self.block_position();
        let y_delta = target.y as i32 - bee_pos.y();
        let y_adjust = if y_delta > 2 {
            4
        } else if y_delta < -2 {
            -4
        } else {
            0
        };
        let mut xz_dist = 6;
        let mut y_dist = 8;
        let dist = (bee_pos.x() - target_pos.x()).abs()
            + (bee_pos.y() - target_pos.y()).abs()
            + (bee_pos.z() - target_pos.z()).abs();
        if dist < 15 {
            xz_dist = dist / 2;
            y_dist = dist / 2;
        }
        if let Some(next) = air_random_pos_towards(
            self,
            xz_dist,
            y_dist,
            y_adjust,
            target,
            f64::from(PI / 10.0),
        ) {
            self.mob_base
                .navigation()
                .lock()
                .set_max_visited_nodes_multiplier(0.5);
            let _ = self.move_to_pos(next, 1.0);
        }
    }

    /// Vanilla `isFlying`.
    fn is_flying(&self) -> bool {
        !self.on_ground()
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    /// Vanilla `FlowerBlock.getBeeInteractionEffect` for the flower item fed
    /// to the bee.
    fn bee_interaction_effect(item_stack: &ItemStack) -> Option<MobEffectInstance> {
        if item_stack.is(&vanilla_items::WITHER_ROSE) {
            Some(MobEffectInstance::with_duration(
                vanilla_mob_effects::WITHER,
                WITHER_ROSE_EFFECT_TICKS,
                0,
            ))
        } else if item_stack.is(&vanilla_items::OPEN_EYEBLOSSOM) {
            Some(MobEffectInstance::with_duration(
                vanilla_mob_effects::POISON,
                EYEBLOSSOM_EFFECT_TICKS,
                0,
            ))
        } else {
            None
        }
    }

    /// Vanilla `Bee.customServerAiStep`: drowning, death after stinging, and
    /// the nectar-less timer. Vanilla does not call `Animal`'s version here.
    fn bee_custom_server_ai_step(&self, world: &World) {
        let has_stung = self.has_stung();
        let drown = {
            let mut state = self.state.lock();
            if self.is_in_water() {
                state.under_water_ticks += 1;
            } else {
                state.under_water_ticks = 0;
            }
            state.under_water_ticks > MAX_UNDER_WATER_TICKS
        };
        if drown {
            let _ = self.hurt_server(
                world,
                &DamageSource::environment(&vanilla_damage_types::DROWN),
                1.0,
            );
        }

        if has_stung {
            let time_since_sting = {
                let mut state = self.state.lock();
                state.time_since_sting += 1;
                state.time_since_sting
            };
            if time_since_sting % 5 == 0
                && rand::random_range(
                    0..(STING_DEATH_COUNTDOWN - time_since_sting).clamp(1, STING_DEATH_COUNTDOWN),
                ) == 0
            {
                let _ = self.hurt_server(
                    world,
                    &DamageSource::environment(&vanilla_damage_types::GENERIC),
                    self.get_health(),
                );
            }
        }

        if !self.has_nectar() {
            self.state.lock().ticks_without_nectar_since_exiting_hive += 1;
        }

        self.update_persistent_anger(world, false);
    }
}

impl NeutralMob for BeeEntity {
    fn neutral_mob_state(&self) -> &NeutralMobState {
        &self.anger
    }

    fn start_persistent_anger_timer(&self) {
        self.set_time_to_remain_angry(rand::random_range(
            PERSISTENT_ANGER_TICKS_MIN..=PERSISTENT_ANGER_TICKS_MAX,
        ));
    }

    fn persistent_anger_end_time(&self) -> i64 {
        *self.entity_data.lock().anger_end_time.get()
    }

    fn set_persistent_anger_end_time(&self, end_time: i64) {
        self.entity_data.lock().anger_end_time.set(end_time);
    }
}

impl DefaultEntityPush for BeeEntity {}

impl Entity for BeeEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    /// Vanilla `playStepSound`: bees make no step sounds.
    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {}

    fn is_flapping(&self) -> bool {
        self.is_flying() && self.tick_count() % TICKS_PER_FLAP == 0
    }

    /// Vanilla `checkFallDamage`: bees never take fall damage.
    fn check_fall_damage(
        self: Arc<Self>,
        _vertical_movement: f64,
        _on_ground: bool,
        _on_state: BlockStateId,
        _pos: BlockPos,
        _world: &Arc<World>,
    ) {
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        let state = self.state.lock();
        if let Some(hive_pos) = state.hive_pos {
            nbt.insert(TAG_HIVE_POS, block_pos_tag(hive_pos));
        }
        if let Some(flower_pos) = state.saved_flower_pos {
            nbt.insert(TAG_FLOWER_POS, block_pos_tag(flower_pos));
        }
        let ticks_without_nectar = state.ticks_without_nectar_since_exiting_hive;
        let stay_out = state.stay_out_of_hive_countdown;
        let crops_grown = state.num_crops_grown_since_pollination;
        drop(state);
        nbt.insert(TAG_HAS_NECTAR, i8::from(self.has_nectar()));
        nbt.insert(TAG_HAS_STUNG, i8::from(self.has_stung()));
        nbt.insert(TAG_TICKS_SINCE_POLLINATION, ticks_without_nectar);
        nbt.insert(TAG_CANNOT_ENTER_HIVE_TICKS, stay_out);
        nbt.insert(TAG_CROPS_GROWN_SINCE_POLLINATION, crops_grown);
        self.add_persistent_anger_save_data(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.set_has_nectar(nbt.byte(TAG_HAS_NECTAR).is_some_and(|value| value != 0));
        self.set_has_stung(nbt.byte(TAG_HAS_STUNG).is_some_and(|value| value != 0));
        {
            let mut state = self.state.lock();
            state.ticks_without_nectar_since_exiting_hive =
                nbt.int(TAG_TICKS_SINCE_POLLINATION).unwrap_or(0);
            state.stay_out_of_hive_countdown = nbt.int(TAG_CANNOT_ENTER_HIVE_TICKS).unwrap_or(0);
            state.num_crops_grown_since_pollination =
                nbt.int(TAG_CROPS_GROWN_SINCE_POLLINATION).unwrap_or(0);
            state.hive_pos = read_block_pos(nbt, TAG_HIVE_POS);
            state.saved_flower_pos = read_block_pos(nbt, TAG_FLOWER_POS);
        }
        self.read_persistent_anger_save_data(nbt);
    }
}

impl LivingEntity for BeeEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_BEE_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_BEE_DEATH)
    }

    fn sound_volume(&self) -> f32 {
        SOUND_VOLUME
    }

    fn omnidirectional_air_mover(&self) -> bool {
        true
    }

    /// Vanilla `jumpInLiquid`: bees only drift up slowly.
    fn jump_in_liquid(&self, _fluid_tag: &steel_utils::Identifier) {
        self.set_velocity(self.velocity() + DVec3::new(0.0, 0.01, 0.0));
    }

    /// Vanilla `hurtServer`: a hit bee stops pollinating.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_invulnerable_to(world, source) {
            return false;
        }
        self.set_pollinating(false);
        self.default_hurt_server(world, source, amount)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        {
            let mut state = self.state.lock();
            if state.stay_out_of_hive_countdown > 0 {
                state.stay_out_of_hive_countdown -= 1;
            }
            if state.remaining_cooldown_before_locating_new_hive > 0 {
                state.remaining_cooldown_before_locating_new_hive -= 1;
            }
            if state.remaining_cooldown_before_locating_new_flower > 0 {
                state.remaining_cooldown_before_locating_new_flower -= 1;
            }
        }
        let should_roll = self.is_angry()
            && !self.has_stung()
            && self.target().is_some_and(|target| {
                target.position().distance_squared(self.position()) < ROLL_DISTANCE_SQR
            });
        self.set_rolling(should_roll);
        if self.tick_count() % 20 == 0 && !self.is_hive_valid() {
            self.clear_hive_pos();
        }
        result
    }
}

impl AgeableMob for BeeEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob().age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob_mut()
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob_mut().baby.set(baby);
    }

    fn age_boundary_changed(&self, _baby: bool) {
        self.refresh_dimensions();
    }
}

impl Animal for BeeEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::BEE_FOOD)
    }
}

impl Mob for BeeEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    /// Vanilla `Bee.createNavigation`: the path stops while the bee pollinates.
    fn tick_path_navigation(&self) {
        if !self.is_pollinating() {
            PathfinderMob::tick_pathfinder_path_navigation(self);
        }
    }

    fn tick_move_control(&self) {
        BEE_MOVE_CONTROL.tick(self);
    }

    /// Vanilla `BeeLookControl.resetXRotOnTick`.
    fn look_control_resets_x_rot(&self) -> bool {
        !self.is_pollinating()
    }

    /// Vanilla `BeeLookControl.tick`: an angry bee keeps its glare.
    fn tick_look_control(&self) {
        if !self.is_angry() {
            self.default_tick_look_control();
        }
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.bee_custom_server_ai_step(&world);
        }
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        None
    }

    /// Vanilla `Bee.doHurtTarget`: stings with poison, then calms down.
    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        let damage_source = DamageSource::direct(&vanilla_damage_types::STING, Arc::clone(entity));
        let damage = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as i32;
        let was_hurt = target.hurt(world, &damage_source, damage as f32);
        if !was_hurt {
            return false;
        }
        let context = EnchantmentPostAttackContext::new(target, &damage_source);
        enchantment_helper::do_post_attack_effects_with_item_source(
            world,
            target.as_ref(),
            &ItemStack::empty(),
            &context,
        );
        if let Some(living_target) = target.as_living_entity() {
            living_target.set_stinger_count(living_target.stinger_count() + 1);
            let poison_seconds = match world.difficulty() {
                Difficulty::Normal => POISON_SECONDS_NORMAL,
                Difficulty::Hard => POISON_SECONDS_HARD,
                _ => 0,
            };
            if poison_seconds > 0 {
                living_target.add_mob_effect_with_source(
                    MobEffectInstance::with_duration(
                        vanilla_mob_effects::POISON,
                        poison_seconds * 20,
                        0,
                    ),
                    Some(entity.as_ref()),
                );
            }
        }
        self.set_has_stung(true);
        self.stop_being_angry();
        self.play_sound(&sound_events::ENTITY_BEE_STING, 1.0, 1.0);
        true
    }

    /// Vanilla `Bee.mobInteract`: feeding a wither rose or open eyeblossom
    /// passes its effect to the bee.
    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if self.is_food(&item_stack)
            && let Some(effect) = Self::bee_interaction_effect(&item_stack)
        {
            self.use_player_item(player, hand);
            self.add_mob_effect(effect);
            return InteractionResult::Success;
        }
        Animal::mob_interact_animal(self, player, hand)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for BeeEntity {
    /// Vanilla `getWalkTargetValue`: bees like open air.
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        if level.get_block_state(pos).is_air() {
            10.0
        } else {
            0.0
        }
    }

    /// Vanilla bee navigation `isStableDestination`: anything resting on a block.
    fn is_stable_destination(&self, pos: BlockPos) -> bool {
        self.level()
            .is_some_and(|world| !world.get_block_state(pos.below()).is_air())
    }
}

/// Vanilla `Level.isLoaded`.
fn is_loaded(world: &World, pos: BlockPos) -> bool {
    world.has_full_chunk(ChunkPos::from_block_pos(pos))
}

#[cfg(test)]
mod tests;
