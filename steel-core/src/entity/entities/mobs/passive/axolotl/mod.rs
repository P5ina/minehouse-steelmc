//! Vanilla `Axolotl`.

mod ai;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use glam::DVec3;
use parking_lot::MutexGuard;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::AXOLOTL_VARIANT;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::entity_variant::AxolotlVariant;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::AxolotlEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_damage_types, vanilla_items,
    vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey};

use crate::behavior::ITEM_BEHAVIORS;
use crate::entity::ai::brain::memory_types::{ATTACK_TARGET, PLAY_DEAD_TICKS};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::smooth_swimming::{
    SmoothSwimmingMoveControl, tick_smooth_swimming_look_control,
};
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::fish::{init_water_animal, water_animal_spawn_unobstructed};
use crate::entity::leash::Leashable as _;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, Entity, EntityBase,
    EntityBaseLoad, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, MobEffectInstance, PathfinderMob, SharedEntity, SpawnGroupData,
    SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::{LevelReader, ServerLevelAccessor, World};

/// Vanilla `Axolotl.AXOLOTL_TOTAL_AIR_SUPPLY`.
const AXOLOTL_TOTAL_AIR_SUPPLY: i32 = 6000;
/// Vanilla `Axolotl.REHYDRATE_AIR_SUPPLY`.
const REHYDRATE_AIR_SUPPLY: i32 = 1800;
/// Vanilla `Axolotl.REGEN_BUFF_MAX_DURATION`.
const REGEN_BUFF_MAX_DURATION: i32 = 2400;
/// Vanilla `Axolotl.REGEN_BUFF_BASE_DURATION`.
const REGEN_BUFF_BASE_DURATION: i32 = 100;
/// Vanilla `Axolotl.RARE_VARIANT_CHANCE`.
const RARE_VARIANT_CHANCE: u32 = 1200;
/// Vanilla `Axolotl.PLAYER_REGEN_DETECTION_RANGE`.
const PLAYER_REGEN_DETECTION_RANGE: f64 = 20.0;
const DRY_OUT_DAMAGE: f32 = 2.0;
const WATER_DRAG: f64 = 0.9;
const MAX_HEAD_ROT: f32 = 1.0;
/// Vanilla `Axolotl.hurtServer`: hurts below half health make it more
/// likely to play dead.
const PLAY_DEAD_HEALTH_FRACTION: f32 = 0.5;
const PLAY_DEAD_ROLL: u32 = 3;
const ATTACK_SOUND_VOLUME: f32 = 1.0;
const BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(0.0, 0.1875, 0.0)];
const BABY_WIDTH: f32 = 0.375;
const BABY_HEIGHT: f32 = 0.21;
const BABY_EYE_HEIGHT: f32 = 0.09375;
/// Vanilla `Axolotl.AxolotlMoveControl`.
const AXOLOTL_MOVE_CONTROL: SmoothSwimmingMoveControl = SmoothSwimmingMoveControl {
    max_turn_x: 85,
    max_turn_y: 10,
    in_water_speed_modifier: 0.1,
    outside_water_speed_modifier: 0.5,
    apply_gravity: false,
};
/// Vanilla `Axolotl.AxolotlLookControl` max y rotation from center.
const AXOLOTL_LOOK_MAX_Y_ROT_FROM_CENTER: i32 = 20;

/// Vanilla `Axolotl.checkAxolotlSpawnRules`.
fn check_axolotl_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn rand::Rng,
) -> bool {
    level
        .get_block_state(pos.below())
        .get_block()
        .has_tag(&BlockTag::AXOLOTLS_SPAWNABLE_ON)
}

/// Vanilla `Axolotl.Variant.getSpawnVariant`.
fn spawn_variant(common: bool) -> AxolotlVariant {
    let valid: Vec<AxolotlVariant> = AxolotlVariant::VALUES
        .into_iter()
        .filter(|variant| variant.is_common() == common)
        .collect();
    valid[rand::random_range(0..valid.len())]
}

/// Vanilla axolotl: hunts in water, plays dead when hurt, and helps the
/// players that fight beside it.
#[entity_behavior(class = "Axolotl", spawn_rule)]
pub struct AxolotlEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    entity_data: SyncMutex<AxolotlEntityData>,
    brain: Brain<Self>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `AxolotlEntity`.
unsafe impl DowncastType for AxolotlEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/axolotl");
}

impl AxolotlEntity {
    /// Vanilla `SpawnPlacements` predicate: `Axolotl.checkAxolotlSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_axolotl_spawn_rules;

    /// Creates a new axolotl at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs an axolotl from persisted base entity state.
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
        AnimalBase::initialize_pathfinding_malus(&mob_base);
        init_water_animal(&mob_base);
        *mob_base.navigation().lock() = PathNavigation::amphibious();
        let mut entity_data = AxolotlEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            animal_base: AnimalBase::new(),
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(&[], ai::sensors(), ai::activities(), None),
        }
    }

    const fn brain(&self) -> &Brain<Self> {
        &self.brain
    }

    fn brain_memories_lock(&self) -> MutexGuard<'_, Memories> {
        self.brain.memories().lock()
    }

    /// Vanilla `getVariant`.
    #[must_use]
    pub fn variant(&self) -> AxolotlVariant {
        AxolotlVariant::by_id(*self.entity_data.lock().variant.get())
    }

    /// Vanilla `setVariant`.
    pub fn set_variant(&self, variant: AxolotlVariant) {
        self.entity_data.lock().variant.set(variant.id());
    }

    /// Vanilla `isPlayingDead`.
    #[must_use]
    pub fn is_playing_dead(&self) -> bool {
        *self.entity_data.lock().playing_dead.get()
    }

    /// Vanilla `setPlayingDead`.
    pub fn set_playing_dead(&self, playing_dead: bool) {
        self.entity_data.lock().playing_dead.set(playing_dead);
    }

    /// Vanilla `fromBucket`.
    #[must_use]
    pub fn is_from_bucket(&self) -> bool {
        *self.entity_data.lock().from_bucket.get()
    }

    /// Vanilla `setFromBucket`.
    pub fn set_from_bucket(&self, from_bucket: bool) {
        self.entity_data.lock().from_bucket.set(from_bucket);
    }

    /// Vanilla `rehydrate`: a splash of water restores some air.
    pub fn rehydrate(&self) {
        let new_air_supply = self.air_supply() + REHYDRATE_AIR_SUPPLY;
        self.set_air_supply(new_air_supply.min(self.max_air_supply()));
    }

    /// Vanilla `handleAirSupply`: out of water and rain it dries out.
    fn handle_air_supply(&self, world: &World, pre_tick_air_supply: i32) {
        if Entity::is_alive(self) && !self.is_in_water_or_rain() {
            self.set_air_supply(pre_tick_air_supply - 1);
            if self.should_take_drowning_damage() {
                self.set_air_supply(0);
                self.hurt(
                    world,
                    &DamageSource::environment(&vanilla_damage_types::DRY_OUT),
                    DRY_OUT_DAMAGE,
                );
            }
        } else {
            self.set_air_supply(self.max_air_supply());
        }
    }

    /// Vanilla `applySupportingEffects`: Regeneration for the player it
    /// fought beside, and Mining Fatigue lifted.
    pub fn apply_supporting_effects(&self, player: &Player) {
        let regen = player.mob_effect(vanilla_mob_effects::REGENERATION);
        if regen
            .as_ref()
            .is_none_or(|effect| effect.ends_within(REGEN_BUFF_MAX_DURATION - 1))
        {
            let previous_duration = regen.map_or(0, |effect| effect.duration());
            let regen_duration =
                REGEN_BUFF_MAX_DURATION.min(REGEN_BUFF_BASE_DURATION + previous_duration);
            player.add_mob_effect_with_source(
                MobEffectInstance::with_duration(
                    vanilla_mob_effects::REGENERATION,
                    regen_duration,
                    0,
                ),
                Some(self),
            );
        }
        player.remove_mob_effect(vanilla_mob_effects::MINING_FATIGUE);
    }
}

impl Entity for AxolotlEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
    }

    fn base_tick(&self) {
        let air_supply = self.air_supply();
        Mob::base_tick_mob(self);
        if !self.is_no_ai()
            && let Some(world) = self.level()
        {
            self.handle_air_supply(&world, air_supply);
        }
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let dimensions = if AgeableMob::is_baby(self) {
            EntityDimensions::new_with_attachments(
                BABY_WIDTH,
                BABY_HEIGHT,
                BABY_EYE_HEIGHT,
                EntityAttachments::new(&BABY_PASSENGER, &[], &[], &[]),
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
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    fn is_pushed_by_fluid(&self) -> bool {
        false
    }

    fn max_air_supply(&self) -> i32 {
        AXOLOTL_TOTAL_AIR_SUPPLY
    }

    fn swim_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_AXOLOTL_SWIM
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_AXOLOTL_SPLASH
    }

    fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
        if let Some(variant) = item_stack.get(AXOLOTL_VARIANT) {
            self.set_variant(*variant);
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        self.save_animal(nbt);
        nbt.insert("Variant", self.variant().id());
        nbt.insert("FromBucket", i8::from(self.is_from_bucket()));
        nbt.insert("Brain", self.brain.save());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.load_animal(nbt);
        self.set_variant(
            nbt.int("Variant")
                .map_or_else(AxolotlVariant::default, AxolotlVariant::by_id),
        );
        self.set_from_bucket(nbt.byte("FromBucket").is_some_and(|value| value != 0));
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
    }
}

impl LivingEntity for AxolotlEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
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
        Some(&sound_events::ENTITY_AXOLOTL_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_AXOLOTL_DEATH)
    }

    /// Vanilla `Axolotl.hurtServer`: a hurt axolotl in water may play dead.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let current_health = self.get_health();
        if !self.is_no_ai()
            && rand::random_range(0..PLAY_DEAD_ROLL) == 0
            && ((rand::random_range(0..PLAY_DEAD_ROLL) as f32) < amount
                || current_health / self.get_max_health() < PLAY_DEAD_HEALTH_FRACTION)
            && amount < current_health
            && self.is_in_water()
            && (source.causing_entity().is_some() || source.direct_entity().is_some())
            && !self.is_playing_dead()
        {
            self.brain_memories_lock()
                .set(&PLAY_DEAD_TICKS, Some(ai::TOTAL_PLAYDEAD_TIME));
        }
        self.default_hurt_server(world, source, amount)
    }

    fn can_be_seen_as_enemy(&self) -> bool {
        !self.is_playing_dead() && !self.is_invulnerable() && self.can_be_seen_by_anyone()
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
        self.set_velocity(self.velocity() * WATER_DRAG);
        result
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        AgeableMob::tick_ageable_mob(self);
        Animal::tick_animal_love(self);
        result
    }
}

impl AgeableMob for AxolotlEntity {
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

    /// Vanilla `Axolotl.getBreedOffspring`: rarely a blue baby, otherwise
    /// one parent's color.
    fn get_breed_offspring(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        let offspring = self.create_breed_offspring(world)?;
        let variant = if rand::random_range(0..RARE_VARIANT_CHANCE) == 0 {
            spawn_variant(false)
        } else if rand::random::<bool>() {
            self.variant()
        } else {
            partner
                .as_entity_event_source()
                .downcast_ref::<Self>()
                .map_or_else(|| self.variant(), Self::variant)
        };
        if let Some(baby) = offspring.downcast_ref::<Self>() {
            baby.set_variant(variant);
            baby.set_persistence_required();
        }
        Some(offspring)
    }
}

impl Animal for AxolotlEntity {
    fn animal_base(&self) -> &AnimalBase {
        &self.animal_base
    }

    fn is_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::AXOLOTL_FOOD)
    }
}

impl Mob for AxolotlEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `AxolotlMoveControl`: still while playing dead.
    fn tick_move_control(&self) {
        if !self.is_playing_dead() {
            AXOLOTL_MOVE_CONTROL.tick(self);
        }
    }

    /// Vanilla `AxolotlLookControl`: still while playing dead.
    fn tick_look_control(&self) {
        if !self.is_playing_dead() {
            tick_smooth_swimming_look_control(self, AXOLOTL_LOOK_MAX_Y_ROT_FROM_CENTER);
        }
    }

    fn max_head_x_rot(&self) -> f32 {
        MAX_HEAD_ROT
    }

    fn max_head_y_rot(&self) -> f32 {
        MAX_HEAD_ROT
    }

    /// Vanilla `Axolotl.getTarget`: the brain's attack target.
    fn target(&self) -> Option<SharedEntity> {
        self.brain_memories_lock().get(&ATTACK_TARGET)
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            ai::update_activity(self);
        }
        if !self.is_no_ai() {
            let playing_dead = self
                .brain_memories_lock()
                .get(&PLAY_DEAD_TICKS)
                .is_some_and(|ticks| ticks > 0);
            self.set_playing_dead(playing_dead);
        }
    }

    fn play_attack_sound(&self) {
        self.play_sound(
            &sound_events::ENTITY_AXOLOTL_ATTACK,
            ATTACK_SOUND_VOLUME,
            1.0,
        );
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_AXOLOTL_IDLE_WATER
        } else {
            &sound_events::ENTITY_AXOLOTL_IDLE_AIR
        })
    }

    /// Vanilla `Axolotl.playAmbientSound`: silent while playing dead.
    fn play_ambient_sound(&self) {
        if !self.is_playing_dead() {
            self.make_sound(self.ambient_sound());
        }
    }

    /// Vanilla `Axolotl.usePlayerItem`: a tropical fish bucket leaves a water
    /// bucket behind.
    fn use_player_item(&self, player: &Player, hand: InteractionHand) {
        let is_fish_bucket = player
            .inventory
            .lock()
            .get_item_in_hand(hand)
            .is(&vanilla_items::TROPICAL_FISH_BUCKET);
        if !is_fish_bucket {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
            return;
        }
        let overflow = player.inventory.lock().apply_filled_result(
            hand,
            ITEM_BEHAVIORS.default_instance(&vanilla_items::WATER_BUCKET),
            player.has_infinite_materials(),
            true,
        );
        if !overflow.is_empty() {
            let _ = player.drop_item(overflow, false, Prediction::Predicted);
        }
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        !self.is_from_bucket() && self.custom_name().is_none()
    }

    fn requires_custom_persistence(&self) -> bool {
        self.is_passenger() || self.is_leashed() || self.is_from_bucket()
    }

    fn mob_can_be_leashed(&self) -> bool {
        true
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        water_animal_spawn_unobstructed(self, level)
    }

    /// Vanilla `Axolotl.finalizeSpawn`: a group shares two common colors, and
    /// every axolotl after the first in a group is a baby.
    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if spawn_reason == EntitySpawnReason::Bucket {
            return group_data;
        }
        let (ageable, variants, is_baby) = match group_data {
            Some(SpawnGroupData::Axolotl(ageable, variants)) => {
                (ageable, variants, ageable.group_size() >= 2)
            }
            _ => (
                AgeableMobGroupData::with_should_spawn_baby(false),
                [spawn_variant(true), spawn_variant(true)],
                false,
            ),
        };
        self.set_variant(variants[rand::random_range(0..variants.len())]);
        if is_baby {
            self.set_age(self.get_baby_start_age());
        }
        let result = self.finalize_spawn_ageable_mob(
            world,
            spawn_reason,
            Some(SpawnGroupData::AgeableMob(ageable)),
        );
        match result {
            Some(SpawnGroupData::AgeableMob(ageable)) => {
                Some(SpawnGroupData::Axolotl(ageable, variants))
            }
            other => other,
        }
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for AxolotlEntity {
    fn get_walk_target_value_in(&self, _pos: BlockPos, _level: &dyn LevelReader) -> f32 {
        0.0
    }
}
