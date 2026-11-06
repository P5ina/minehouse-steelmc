//! Vanilla `Dolphin`: a playful water creature that swims with players,
//! fetches items and, once fed, leads the way to sunken treasure.

mod goals;

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::CTakeItemEntity;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::DolphinEntityData;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{sound_events, vanilla_blocks, vanilla_damage_types};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, ChunkPos, DowncastType, DowncastTypeKey};

use goals::{
    DolphinJumpGoal, DolphinSwimToTreasureGoal, DolphinSwimWithPlayerGoal, MoveToItemGoal,
    PlayWithItemsGoal, is_boat, is_nautilus,
};

use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    AvoidEntityGoal, BreathAirGoal, FollowPlayerRiddenEntityGoal, HurtByTargetGoal,
    LookAtPlayerGoal, MeleeAttackGoal, RandomLookAroundGoal, RandomSwimmingGoal, TryFindLiquidGoal,
};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::smooth_swimming::{
    SmoothSwimmingMoveControl, tick_smooth_swimming_look_control,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::objects::items::ItemEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::fish::{
    init_water_animal, water_animal_experience_reward, water_animal_spawn_unobstructed,
};
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, DefaultEntityPush, Entity, EntityBase,
    EntityBaseLoad, EntityMovementEmission, EntityPose, EntitySpawnReason, EntitySyncedData,
    LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob, RemovalReason, SharedEntity,
    SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `Dolphin.TOTAL_AIR_SUPPLY`.
pub const DOLPHIN_MAX_AIR_SUPPLY: i32 = 4800;
/// Vanilla `Dolphin.TOTAL_MOISTNESS_LEVEL`: ticks a dolphin survives on land.
pub const DOLPHIN_MAX_MOISTNESS: i32 = 2400;
/// Vanilla `Dolphin.BABY_SCALE`.
const DOLPHIN_BABY_SCALE: f32 = 0.65;
const DOLPHIN_BABY_EYE_HEIGHT: f32 = 0.09375;
const DOLPHIN_BABY_PASSENGER: [EntityAttachmentPoint; 1] =
    [EntityAttachmentPoint::new(0.0, 0.3125, 0.0)];
const DOLPHIN_BABY_SPAWN_CHANCE: f32 = 0.1;
const DOLPHIN_AMBIENT_SOUND_INTERVAL: i32 = 120;
const DOLPHIN_MAX_HEAD_ROT: f32 = 1.0;
const DOLPHIN_DRY_OUT_DAMAGE: f32 = 1.0;
const DOLPHIN_LAND_FLOP_SPREAD: f32 = 0.2;
const DOLPHIN_LAND_FLOP_JUMP: f64 = 0.5;
const DOLPHIN_IDLE_SINK: f64 = -0.005;
const DOLPHIN_WATER_DRAG: f64 = 0.9;
const DOLPHIN_NEXT_POS_DISTANCE: f64 = 12.0;
/// Vanilla `AgeableWaterCreature.checkSurfaceAgeableWaterCreatureSpawnRules`
/// depth below sea level.
const SURFACE_SPAWN_DEPTH: i32 = 13;

/// Vanilla `SmoothSwimmingMoveControl(this, 85, 10, 0.02F, 0.1F, true)`.
const DOLPHIN_MOVE_CONTROL: SmoothSwimmingMoveControl = SmoothSwimmingMoveControl {
    max_turn_x: 85,
    max_turn_y: 10,
    in_water_speed_modifier: 0.02,
    outside_water_speed_modifier: 0.1,
    apply_gravity: true,
};
/// Vanilla `SmoothSwimmingLookControl(this, 10)`.
const DOLPHIN_LOOK_MAX_Y_ROT_FROM_CENTER: i32 = 10;

/// Vanilla `AgeableWaterCreature.checkSurfaceAgeableWaterCreatureSpawnRules`:
/// near the surface, in water with more water below and above.
pub(crate) fn check_surface_ageable_water_creature_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    let sea_level = level.sea_level();
    pos.y() >= sea_level - SURFACE_SPAWN_DEPTH
        && pos.y() <= sea_level
        && level
            .get_block_state(pos.below())
            .get_fluid_state()
            .fluid_id
            .has_tag(&FluidTag::WATER)
        && level.get_block_state(pos.above()).get_block() == &vanilla_blocks::WATER
}

/// Vanilla dolphin.
#[entity_behavior(class = "Dolphin", spawn_rule)]
pub struct DolphinEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    entity_data: SyncMutex<DolphinEntityData>,
    /// Vanilla `Dolphin.treasurePos`.
    treasure_pos: SyncMutex<Option<BlockPos>>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `DolphinEntity`.
unsafe impl DowncastType for DolphinEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/dolphin");
}

impl DolphinEntity {
    /// Vanilla `SpawnPlacements` predicate.
    pub(crate) const SPAWN_RULE: SpawnRule = check_surface_ageable_water_creature_spawn_rules;

    /// Creates a new dolphin.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved dolphin.
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
        init_dolphin(&mob_base);

        let mut entity_data = DolphinEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            entity_data: SyncMutex::new(entity_data),
            treasure_pos: SyncMutex::new(None),
        }
    }

    /// Vanilla `Dolphin.gotFish`.
    #[must_use]
    pub fn got_fish(&self) -> bool {
        *self.entity_data.lock().got_fish.get()
    }

    /// Vanilla `Dolphin.setGotFish`.
    pub fn set_got_fish(&self, got_fish: bool) {
        self.entity_data.lock().got_fish.set(got_fish);
    }

    /// Vanilla `Dolphin.getMoistnessLevel`.
    #[must_use]
    pub fn moistness_level(&self) -> i32 {
        *self.entity_data.lock().moistness_level.get()
    }

    /// Vanilla `Dolphin.setMoisntessLevel`.
    pub fn set_moistness_level(&self, level: i32) {
        self.entity_data.lock().moistness_level.set(level);
    }

    /// Vanilla `Dolphin.treasurePos`.
    #[must_use]
    pub fn treasure_pos(&self) -> Option<BlockPos> {
        *self.treasure_pos.lock()
    }

    fn set_treasure_pos(&self, pos: Option<BlockPos>) {
        *self.treasure_pos.lock() = pos;
    }

    /// Vanilla `Dolphin.closeToNextPos`.
    fn close_to_next_pos(&self) -> bool {
        self.mob_base
            .navigation()
            .lock()
            .target_pos()
            .is_some_and(|target| {
                target.closer_to_center_than(self.position(), DOLPHIN_NEXT_POS_DISTANCE)
            })
    }

    /// Vanilla `Dolphin.tick` after `super.tick()`: dries out on land.
    fn tick_moistness(&self) {
        if self.is_no_ai() {
            self.set_air_supply(DOLPHIN_MAX_AIR_SUPPLY);
            return;
        }

        if self.is_in_water_or_rain() {
            self.set_moistness_level(DOLPHIN_MAX_MOISTNESS);
            return;
        }

        self.set_moistness_level(self.moistness_level() - 1);
        if self.moistness_level() <= 0
            && let Some(world) = self.level()
        {
            self.hurt(
                &world,
                &DamageSource::environment(&vanilla_damage_types::DRY_OUT),
                DOLPHIN_DRY_OUT_DAMAGE,
            );
        }

        if self.on_ground() {
            let x = (rand::random::<f32>() * 2.0 - 1.0) * DOLPHIN_LAND_FLOP_SPREAD;
            let z = (rand::random::<f32>() * 2.0 - 1.0) * DOLPHIN_LAND_FLOP_SPREAD;
            self.set_velocity(
                self.velocity() + DVec3::new(f64::from(x), DOLPHIN_LAND_FLOP_JUMP, f64::from(z)),
            );
            let (_, x_rot) = self.rotation();
            self.set_rotation((rand::random::<f32>() * 360.0, x_rot));
            self.set_on_ground(false);
            self.mark_velocity_sync();
        }
    }
}

/// Vanilla `Dolphin` constructor and `registerGoals`.
fn init_dolphin(mob_base: &MobBase) {
    init_water_animal(mob_base);
    *mob_base.navigation().lock() = PathNavigation::water_bound(true);
    *mob_base.can_pick_up_loot().lock() = true;

    let mut goal_selector = mob_base.goal_selector().lock();
    goal_selector.add_goal(0, BreathAirGoal::new());
    goal_selector.add_goal(0, TryFindLiquidGoal::new(FluidTag::DOLPHIN_TRIES_TO_FIND));
    goal_selector.add_goal(1, DolphinSwimToTreasureGoal::new());
    goal_selector.add_goal(2, DolphinSwimWithPlayerGoal::new(4.0));
    goal_selector.add_goal(4, RandomSwimmingGoal::new(1.0, 10));
    goal_selector.add_goal(4, RandomLookAroundGoal::new());
    goal_selector.add_goal(5, LookAtPlayerGoal::new(6.0));
    goal_selector.add_goal(5, DolphinJumpGoal::new(10));
    goal_selector.add_goal(6, MeleeAttackGoal::new(1.2, true));
    goal_selector.add_goal(7, MoveToItemGoal::new());
    goal_selector.add_goal(8, PlayWithItemsGoal);
    goal_selector.add_goal(8, FollowPlayerRiddenEntityGoal::new(is_boat));
    goal_selector.add_goal(8, FollowPlayerRiddenEntityGoal::new(is_nautilus));
    goal_selector.add_goal(
        9,
        AvoidEntityGoal::with_selector(8.0, 1.0, 1.0, |target, _| target.as_guardian().is_some()),
    );
    drop(goal_selector);

    mob_base.target_selector().lock().add_goal(
        1,
        HurtByTargetGoal::new()
            .with_ignored_damage_selector(|attacker| attacker.as_guardian().is_some())
            .set_alert_others([]),
    );
}

impl DefaultEntityPush for DolphinEntity {}

impl Entity for DolphinEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        self.tick_moistness();
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn base_tick(&self) {
        // Vanilla `Dolphin.handleAirSupply` is empty: `BreathAirGoal` and
        // `increaseAirSupply` manage the dolphin's air instead.
        Mob::base_tick_mob(self);
    }

    fn is_pushed_by_fluid(&self) -> bool {
        false
    }

    fn max_air_supply(&self) -> i32 {
        DOLPHIN_MAX_AIR_SUPPLY
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let dimensions = if AgeableMob::is_baby(self) {
            let scaled = self.entity_type.dimensions.scale(DOLPHIN_BABY_SCALE);
            EntityDimensions::new_with_attachments(
                scaled.width,
                scaled.height,
                DOLPHIN_BABY_EYE_HEIGHT,
                EntityAttachments::new(&DOLPHIN_BABY_PASSENGER, &[], &[], &[]),
            )
        } else {
            self.entity_type.dimensions
        };
        dimensions.scale(LivingEntity::get_scale(self))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn swim_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_DOLPHIN_SWIM
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_DOLPHIN_SPLASH
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        nbt.insert("GotFish", self.got_fish());
        nbt.insert("Moistness", self.moistness_level());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.set_got_fish(nbt.byte("GotFish").is_some_and(|got_fish| got_fish != 0));
        self.set_moistness_level(nbt.int("Moistness").unwrap_or(DOLPHIN_MAX_MOISTNESS));
    }
}

impl LivingEntity for DolphinEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let clamped = health.clamp(0.0, self.get_max_health());
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn get_age_scale(&self) -> f32 {
        if AgeableMob::is_baby(self) {
            DOLPHIN_BABY_SCALE
        } else {
            1.0
        }
    }

    fn increase_air_supply(&self, _current_supply: i32) -> i32 {
        DOLPHIN_MAX_AIR_SUPPLY
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_DOLPHIN_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_DOLPHIN_DEATH)
    }

    fn base_experience_reward(&self) -> i32 {
        water_animal_experience_reward()
    }

    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.move_relative(self.get_speed(), input);
        let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
        self.set_velocity(self.velocity() * DOLPHIN_WATER_DRAG);
        if self.target().is_none() {
            self.set_velocity(self.velocity() + DVec3::new(0.0, DOLPHIN_IDLE_SINK, 0.0));
        }
        result
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        result
    }
}

impl AgeableMob for DolphinEntity {
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
}

impl Mob for DolphinEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        DOLPHIN_MOVE_CONTROL.tick(self);
    }

    fn tick_look_control(&self) {
        tick_smooth_swimming_look_control(self, DOLPHIN_LOOK_MAX_Y_ROT_FROM_CENTER);
    }

    fn max_head_x_rot(&self) -> f32 {
        DOLPHIN_MAX_HEAD_ROT
    }

    fn max_head_y_rot(&self) -> f32 {
        DOLPHIN_MAX_HEAD_ROT
    }

    fn can_attack(&self, target: &dyn LivingEntity) -> bool {
        !AgeableMob::is_baby(self) && self.default_can_attack_mob(target)
    }

    fn play_attack_sound(&self) {
        self.play_sound(&sound_events::ENTITY_DOLPHIN_ATTACK, 1.0, 1.0);
    }

    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        if !self.equipment_in_slot(EquipmentSlot::MainHand).is_empty() {
            return;
        }
        let item_stack = item_entity.get_item();
        if !self.can_hold_item(&item_stack) {
            return;
        }
        let count = item_stack.count();
        self.living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, item_stack);
        self.set_guaranteed_drop(EquipmentSlot::MainHand);
        world.broadcast_to_nearby(
            ChunkPos::from_entity_pos(item_entity.position()),
            CTakeItemEntity::new(item_entity.id(), self.id(), count),
            None,
        );
        item_entity.set_removed(RemovalReason::Discarded);
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let is_fish = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            !item_stack.is_empty() && item_stack.item.has_tag(&ItemTag::FISHES)
        };
        if !is_fish {
            return AgeableMob::mob_interact_ageable(self, player, hand);
        }

        self.play_sound(&sound_events::ENTITY_DOLPHIN_EAT, 1.0, 1.0);
        if self.can_age_up() {
            Mob::use_player_item(self, player, hand);
            self.age_up(
                AgeableMobBase::get_speed_up_seconds_when_feeding(-self.get_age()),
                true,
            );
        } else {
            self.set_got_fish(true);
            Mob::use_player_item(self, player, hand);
        }
        InteractionResult::Success
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_DOLPHIN_AMBIENT_WATER
        } else {
            &sound_events::ENTITY_DOLPHIN_AMBIENT
        })
    }

    fn ambient_sound_interval(&self) -> i32 {
        DOLPHIN_AMBIENT_SOUND_INTERVAL
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        water_animal_spawn_unobstructed(self, level)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.set_air_supply(DOLPHIN_MAX_AIR_SUPPLY);
        let (y_rot, _) = self.rotation();
        self.set_rotation((y_rot, 0.0));
        let group_data = group_data.unwrap_or(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_baby_spawn_chance(DOLPHIN_BABY_SPAWN_CHANCE),
        ));
        self.finalize_spawn_ageable_mob(world, spawn_reason, Some(group_data))
    }
}

impl PathfinderMob for DolphinEntity {}

#[cfg(test)]
mod tests;
