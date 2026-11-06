//! Vanilla `AbstractSkeleton` and its kinds: `Skeleton`, `Stray`, `Bogged`,
//! `WitherSkeleton` and `Parched`.
//!
//! All five share the bow-or-melee weapon goals, the sun-avoiding goals and
//! the arrow shot; each kind changes its sounds, its arrows and a few rules.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::mob_effect_instance::MobEffectInstance as EffectSpec;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::{
    BoggedEntityData, ParchedEntityData, SkeletonEntityData, StrayEntityData,
    WitherSkeletonEntityData,
};
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, level_events, sound_events, vanilla_attributes,
    vanilla_blocks, vanilla_entities, vanilla_game_events, vanilla_items, vanilla_loot_tables,
    vanilla_mob_effects,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::{Difficulty, InteractionHand};
use steel_utils::{
    BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey, Identifier,
};

use super::zombie::is_halloween;
use crate::behavior::InteractionResult;
use crate::behavior::items::projectile_weapon::{
    get_mob_arrow, monster_projectile, spawn_arrow_using_shoot,
};
use crate::entity::ai::goal::{
    AvoidEntityGoal, FleeSunGoal, Goal, GoalControls, HurtByTargetGoal, LookAtPlayerGoal,
    MeleeAttackGoal, NearestAttackableTargetGoal, RandomLookAroundGoal, RangedBowAttackGoal,
    RestrictSunGoal, WaterAvoidingRandomStrollGoal, weapon_holding_hand,
};
use crate::entity::ai::path::PathType;
use crate::entity::conversion::ConversionParams;
use crate::entity::damage::DamageSource;
use crate::entity::entities::ArrowEntity;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::living_entity::shearing_loot_items_with_rng;
use crate::entity::mob::spawn_equipment::{
    populate_default_equipment_enchantments, populate_default_equipment_slots,
};
use crate::entity::synced_data::LivingEntitySyncedData;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
    MobEffectInstance, Monster, PathfinderMob, RangedAttackMob, SharedEntity, Shearable,
    SpawnGroupData, SpawnRule, sync_dirty_mob_effects,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{DifficultyInstance, ServerLevelAccessor, World};

#[cfg(test)]
mod tests;

const DEFAULT_STEP_HEIGHT: f32 = 0.6;
const XP_REWARD: i32 = 5;
const STEP_SOUND_VOLUME: f32 = 0.15;
const HARD_ATTACK_INTERVAL: i32 = 20;
const NORMAL_ATTACK_INTERVAL: i32 = 40;
const INCREASED_HARD_ATTACK_INTERVAL: i32 = 50;
const INCREASED_NORMAL_ATTACK_INTERVAL: i32 = 70;
const BOW_GOAL_SPEED: f64 = 1.0;
const BOW_GOAL_RADIUS: f32 = 15.0;
const MELEE_GOAL_SPEED: f64 = 1.2;
const WEAPON_GOAL_PRIORITY: i32 = 4;
const AVOID_WOLF_DISTANCE: f32 = 6.0;
const PICK_UP_LOOT_CHANCE: f32 = 0.55;
const HALLOWEEN_PUMPKIN_CHANCE: f32 = 0.25;
const HALLOWEEN_LANTERN_CHANCE: f32 = 0.1;
const ARROW_POWER: f32 = 1.6;
/// Vanilla `target.getY(0.3333333333333333)`.
const TARGET_HEIGHT_FRACTION: f64 = 0.333_333_333_333_333_3;
const ARROW_ARC: f32 = 0.2;
/// Ticks in powder snow before a skeleton starts turning into a stray.
const FREEZE_AFFLICTION_TICKS: i32 = 140;
/// Ticks of shaking before the stray conversion.
const STRAY_CONVERSION_TICKS: i32 = 300;
const NOT_CONVERTING: i32 = -1;
const STRAY_ARROW_SLOWNESS_TICKS: i32 = 600;
const BOGGED_ARROW_POISON_TICKS: i32 = 100;
const PARCHED_ARROW_WEAKNESS_TICKS: i32 = 600;
const WITHER_SKELETON_ATTACK_DAMAGE: f64 = 4.0;
const WITHER_SKELETON_WITHER_TICKS: i32 = 200;
/// Vanilla `arrow.igniteForSeconds(100.0F)`.
const WITHER_SKELETON_ARROW_FIRE_TICKS: i32 = 2000;
const WITHER_SKELETON_LAVA_MALUS: f32 = 8.0;

/// Which vanilla `AbstractSkeleton` subclass an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkeletonVariant {
    Skeleton,
    Stray,
    Bogged,
    WitherSkeleton,
    Parched,
}

impl SkeletonVariant {
    fn ambient_sound(self) -> SoundEventRef {
        match self {
            Self::Skeleton => &sound_events::ENTITY_SKELETON_AMBIENT,
            Self::Stray => &sound_events::ENTITY_STRAY_AMBIENT,
            Self::Bogged => &sound_events::ENTITY_BOGGED_AMBIENT,
            Self::WitherSkeleton => &sound_events::ENTITY_WITHER_SKELETON_AMBIENT,
            Self::Parched => &sound_events::ENTITY_PARCHED_AMBIENT,
        }
    }

    fn hurt_sound(self) -> SoundEventRef {
        match self {
            Self::Skeleton => &sound_events::ENTITY_SKELETON_HURT,
            Self::Stray => &sound_events::ENTITY_STRAY_HURT,
            Self::Bogged => &sound_events::ENTITY_BOGGED_HURT,
            Self::WitherSkeleton => &sound_events::ENTITY_WITHER_SKELETON_HURT,
            Self::Parched => &sound_events::ENTITY_PARCHED_HURT,
        }
    }

    fn death_sound(self) -> SoundEventRef {
        match self {
            Self::Skeleton => &sound_events::ENTITY_SKELETON_DEATH,
            Self::Stray => &sound_events::ENTITY_STRAY_DEATH,
            Self::Bogged => &sound_events::ENTITY_BOGGED_DEATH,
            Self::WitherSkeleton => &sound_events::ENTITY_WITHER_SKELETON_DEATH,
            Self::Parched => &sound_events::ENTITY_PARCHED_DEATH,
        }
    }

    fn step_sound(self) -> SoundEventRef {
        match self {
            Self::Skeleton => &sound_events::ENTITY_SKELETON_STEP,
            Self::Stray => &sound_events::ENTITY_STRAY_STEP,
            Self::Bogged => &sound_events::ENTITY_BOGGED_STEP,
            Self::WitherSkeleton => &sound_events::ENTITY_WITHER_SKELETON_STEP,
            Self::Parched => &sound_events::ENTITY_PARCHED_STEP,
        }
    }

    /// Vanilla `getHardAttackInterval`.
    const fn hard_attack_interval(self) -> i32 {
        match self {
            Self::Bogged | Self::Parched => INCREASED_HARD_ATTACK_INTERVAL,
            _ => HARD_ATTACK_INTERVAL,
        }
    }

    /// Vanilla `getAttackInterval`.
    const fn attack_interval(self) -> i32 {
        match self {
            Self::Bogged | Self::Parched => INCREASED_NORMAL_ATTACK_INTERVAL,
            _ => NORMAL_ATTACK_INTERVAL,
        }
    }

    /// The effect each kind's `getArrow` adds to plain arrows.
    fn arrow_effect(self) -> Option<EffectSpec> {
        match self {
            Self::Stray => Some(EffectSpec::simple(
                vanilla_mob_effects::SLOWNESS,
                STRAY_ARROW_SLOWNESS_TICKS,
                0,
            )),
            Self::Bogged => Some(EffectSpec::simple(
                vanilla_mob_effects::POISON,
                BOGGED_ARROW_POISON_TICKS,
                0,
            )),
            Self::Parched => Some(EffectSpec::simple(
                vanilla_mob_effects::WEAKNESS,
                PARCHED_ARROW_WEAKNESS_TICKS,
                0,
            )),
            Self::Skeleton | Self::WitherSkeleton => None,
        }
    }
}

/// Vanilla `AbstractSkeleton.bowGoal` / `meleeGoal`, marked so
/// `reassessWeaponGoal` can swap them.
struct SkeletonWeaponGoal<G>(G);

impl<G: Goal> Goal for SkeletonWeaponGoal<G> {
    fn controls(&self) -> GoalControls {
        self.0.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn is_interruptable(&self) -> bool {
        self.0.is_interruptable()
    }

    fn is_skeleton_weapon_goal(&self) -> bool {
        true
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.0.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.0.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.0.tick(mob, entity);
    }
}

fn is_entity_type(target: &dyn LivingEntity, entity_type: EntityTypeRef) -> bool {
    target.entity_type() == entity_type
}

/// Builds the vanilla `AbstractSkeleton.registerGoals` mob base (weapon
/// goals are added by `reassessWeaponGoal`).
fn skeleton_mob_base(variant: SkeletonVariant) -> MobBase {
    let mob_base = MobBase::new();
    mob_base.set_xp_reward(XP_REWARD);
    {
        let mut target_selector = mob_base.target_selector().lock();
        if variant == SkeletonVariant::WitherSkeleton {
            // Vanilla `WitherSkeleton.registerGoals` adds this before `super`.
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    is_entity_type(target, &vanilla_entities::PIGLIN)
                        || is_entity_type(target, &vanilla_entities::PIGLIN_BRUTE)
                }),
            );
        }
        let mut goal_selector = mob_base.goal_selector().lock();
        goal_selector.add_goal(2, RestrictSunGoal::new());
        goal_selector.add_goal(3, FleeSunGoal::new(1.0));
        goal_selector.add_goal(
            3,
            AvoidEntityGoal::with_selector(AVOID_WOLF_DISTANCE, 1.0, 1.2, |target, _| {
                is_entity_type(target, &vanilla_entities::WOLF)
            }),
        );
        goal_selector.add_goal(5, WaterAvoidingRandomStrollGoal::new(1.0));
        goal_selector.add_goal(6, LookAtPlayerGoal::new(8.0));
        goal_selector.add_goal(6, RandomLookAroundGoal::new());

        target_selector.add_goal(1, HurtByTargetGoal::new());
        target_selector.add_goal(
            2,
            NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
        );
        target_selector.add_goal(
            3,
            NearestAttackableTargetGoal::new(true, |target, _| {
                is_entity_type(target, &vanilla_entities::IRON_GOLEM)
            }),
        );
        // Vanilla `Turtle.BABY_ON_LAND_SELECTOR`.
        target_selector.add_goal(
            3,
            NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
                is_entity_type(target, &vanilla_entities::TURTLE)
                    && target.is_baby()
                    && !target.is_in_water()
            }),
        );
    }
    mob_base
}

/// Server-only state of a skeleton's powder-snow `ConversionTracker`.
struct FreezeState {
    /// Vanilla `ConversionTracker.afflictionTime` (`FreezingTime`).
    affliction_time: i32,
    /// Vanilla `ConversionTracker.conversionTime` (`StrayConversionTime`).
    conversion_time: i32,
}

/// Implements the entity traits every skeleton kind shares, mirroring
/// vanilla `AbstractSkeleton`.
macro_rules! impl_skeleton_traits {
    ($entity:ty, $variant:expr, $key:literal) => {
        // SAFETY: The owner-scoped type key uniquely identifies this entity.
        unsafe impl DowncastType for $entity {
            const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new($key);
        }

        impl $entity {
            const VARIANT: SkeletonVariant = $variant;

            /// Creates a new skeleton of this kind.
            #[must_use]
            pub fn new(
                entity_type: EntityTypeRef,
                id: i32,
                position: DVec3,
                world: Weak<World>,
            ) -> Self {
                Self::new_with_base(
                    EntityBase::new(id, position, entity_type.dimensions, world),
                    entity_type,
                )
            }

            /// Loads a saved skeleton of this kind.
            #[must_use]
            pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
                Self::new_with_base(
                    EntityBase::from_load(load, entity_type.dimensions),
                    entity_type,
                )
            }

            /// Vanilla `AbstractSkeleton.reassessWeaponGoal`: a bow means the
            /// bow goal at this kind's draw interval, anything else melee.
            pub fn reassess_weapon_goal(&self) {
                let Some(world) = self.level() else {
                    return;
                };
                self.remove_all_goals(|goal| goal.is_skeleton_weapon_goal());
                let hand = weapon_holding_hand(self, &vanilla_items::BOW);
                let mut holds_bow = false;
                self.with_equipment_slot(hand_slot(hand), &mut |stack| {
                    holds_bow = stack.is(&vanilla_items::BOW);
                });
                let mut goal_selector = self.mob_base.goal_selector().lock();
                if holds_bow {
                    let min_attack_interval = if world.difficulty() == Difficulty::Hard {
                        Self::VARIANT.hard_attack_interval()
                    } else {
                        Self::VARIANT.attack_interval()
                    };
                    // VANILLA: the same goal instance is re-added; its
                    // strafing direction survives, everything else resets.
                    goal_selector.add_goal(
                        WEAPON_GOAL_PRIORITY,
                        SkeletonWeaponGoal(RangedBowAttackGoal::new(
                            BOW_GOAL_SPEED,
                            min_attack_interval,
                            BOW_GOAL_RADIUS,
                        )),
                    );
                } else {
                    goal_selector.add_goal(
                        WEAPON_GOAL_PRIORITY,
                        SkeletonWeaponGoal(MeleeAttackGoal::new(MELEE_GOAL_SPEED, false)),
                    );
                }
            }

            /// Vanilla `AbstractSkeleton.getArrow` plus this kind's override.
            fn get_arrow(
                &self,
                world: &Arc<World>,
                entity: &SharedEntity,
                projectile: &ItemStack,
                power: f32,
                firing_weapon: &ItemStack,
            ) -> SharedEntity {
                let arrow = get_mob_arrow(world, entity, projectile, power, Some(firing_weapon));
                if let Some(effect) = Self::VARIANT.arrow_effect()
                    && let Some(plain) = arrow.downcast_ref::<ArrowEntity>()
                {
                    plain.add_effect(effect);
                }
                if Self::VARIANT == SkeletonVariant::WitherSkeleton {
                    arrow.ignite_for_ticks(WITHER_SKELETON_ARROW_FIRE_TICKS);
                }
                arrow
            }

            /// Vanilla `populateDefaultEquipmentSlots`: armor and a bow, or
            /// the wither skeleton's stone sword.
            fn populate_skeleton_equipment(
                &self,
                world: &World,
                random: &mut impl Random,
                difficulty: DifficultyInstance,
            ) {
                if Self::VARIANT == SkeletonVariant::WitherSkeleton {
                    let _ = self.living_base.equipment().lock().set(
                        EquipmentSlot::MainHand,
                        ItemStack::new(&vanilla_items::STONE_SWORD),
                    );
                    return;
                }
                populate_default_equipment_slots(self, world, random, difficulty);
                let _ = self
                    .living_base
                    .equipment()
                    .lock()
                    .set(EquipmentSlot::MainHand, ItemStack::new(&vanilla_items::BOW));
            }

            /// Vanilla `AbstractSkeleton.finalizeSpawn` (and the wither
            /// skeleton's attack damage on top).
            fn finalize_skeleton_spawn(
                &self,
                world: &Arc<World>,
                random: &mut impl Random,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                let difficulty = world.current_difficulty_at(self.block_position());
                let group_data = self.finalize_spawn_mob_base(world, spawn_reason, group_data);
                self.populate_skeleton_equipment(world, random, difficulty);
                if Self::VARIANT != SkeletonVariant::WitherSkeleton {
                    populate_default_equipment_enchantments(self, random, difficulty);
                }
                self.reassess_weapon_goal();
                self.set_can_pick_up_loot(
                    random.next_f32() < PICK_UP_LOOT_CHANCE * difficulty.special_multiplier(),
                );
                if !self.has_item_in_slot(EquipmentSlot::Head)
                    && is_halloween()
                    && random.next_f32() < HALLOWEEN_PUMPKIN_CHANCE
                {
                    let head = if random.next_f32() < HALLOWEEN_LANTERN_CHANCE {
                        &vanilla_items::JACK_O_LANTERN
                    } else {
                        &vanilla_items::CARVED_PUMPKIN
                    };
                    let _ = self
                        .living_base
                        .equipment()
                        .lock()
                        .set(EquipmentSlot::Head, ItemStack::new(head));
                    self.set_equipment_drop_chance(EquipmentSlot::Head, 0.0);
                }
                if Self::VARIANT == SkeletonVariant::WitherSkeleton {
                    self.attributes().lock().set_base_value(
                        vanilla_attributes::ATTACK_DAMAGE,
                        WITHER_SKELETON_ATTACK_DAMAGE,
                    );
                    self.reassess_weapon_goal();
                }
                group_data
            }

            fn update_dirty_mob_effect_entity_data(&self) {
                sync_dirty_mob_effects(&self.living_base, &self.entity_data);
            }
        }

        impl DefaultEntityPush for $entity {}

        impl Entity for $entity {
            fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
                Some(self)
            }

            fn base(&self) -> &EntityBase {
                &self.base
            }

            fn entity_type(&self) -> EntityTypeRef {
                self.entity_type
            }

            fn movement_emission(&self) -> EntityMovementEmission {
                EntityMovementEmission::All
            }

            fn tick(self: Arc<Self>) {
                self.tick_before_living();
                let entity = Arc::clone(&self).into_shared_entity();
                if let Some(living) = entity.as_living_entity() {
                    living.tick_living_entity(&entity);
                }
            }

            fn base_tick(&self) {
                Mob::base_tick_mob(self);
            }

            /// Vanilla `AbstractSkeleton.rideTick`: face where the mount faces.
            fn ride_tick(self: Arc<Self>) {
                Arc::clone(&self).default_ride_tick();
                self.reset_fall_distance();
                if let Some(vehicle) = self.controlled_vehicle()
                    && let Some(mount) = vehicle.as_mob()
                    && vehicle.as_living_entity().is_some()
                    && mount.as_pathfinder_mob().is_some()
                {
                    self.set_y_body_rot(mount.y_body_rot());
                }
            }

            fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
                Some(&self.entity_data)
            }

            fn update_data_before_sync(&self) {
                self.update_dirty_mob_effect_entity_data();
            }

            fn max_up_step(&self) -> f32 {
                self.attributes()
                    .lock()
                    .get_value(vanilla_attributes::STEP_HEIGHT)
                    .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
            }

            fn sound_source(&self) -> SoundSource {
                Monster::sound_source_monster(self)
            }

            fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
                Monster::fall_sounds_monster(self)
            }

            fn swim_sound(&self) -> SoundEventRef {
                Monster::swim_sound_monster(self)
            }

            fn swim_splash_sound(&self) -> SoundEventRef {
                Monster::swim_splash_sound_monster(self)
            }

            fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
                self.play_sound(Self::VARIANT.step_sound(), STEP_SOUND_VOLUME, 1.0);
            }

            fn save_additional(&self, nbt: &mut NbtCompound) {
                self.save_mob(nbt);
                self.save_kind(nbt);
            }

            fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_mob(nbt);
                self.reassess_weapon_goal();
                self.load_kind(nbt);
            }
        }

        impl LivingEntity for $entity {
            fn living_base(&self) -> &LivingEntityBase {
                &self.living_base
            }

            fn living_synced_data(&self) -> Option<&dyn LivingEntitySyncedData> {
                Some(&self.entity_data)
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
                Some(Self::VARIANT.hurt_sound())
            }

            fn death_sound(&self) -> Option<SoundEventRef> {
                Some(Self::VARIANT.death_sound())
            }

            fn should_drop_experience(&self) -> bool {
                Monster::should_drop_experience_monster(self)
            }

            fn should_drop_loot(&self, world: &World) -> bool {
                Monster::should_drop_loot_monster(self, world)
            }

            fn server_ai_step(&self, entity: &SharedEntity) {
                Mob::mob_server_ai_step(self, entity);
            }

            fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
                Monster::monster_ai_step(self, entity)
            }

            /// Vanilla `WitherSkeleton` / `Parched.canBeAffected`.
            fn can_be_affected(&self, effect: &MobEffectInstance) -> bool {
                let immune = match Self::VARIANT {
                    SkeletonVariant::WitherSkeleton => {
                        effect.effect() == vanilla_mob_effects::WITHER
                    }
                    SkeletonVariant::Parched => effect.effect() == vanilla_mob_effects::WEAKNESS,
                    _ => false,
                };
                !immune && self.default_can_be_affected(effect)
            }

            /// Vanilla `AbstractSkeleton.onEquipItem`.
            fn on_equip_item(&self, slot: EquipmentSlot, previous: &ItemStack, stack: &ItemStack) {
                self.default_on_equip_item(slot, previous, stack);
                self.reassess_weapon_goal();
            }
        }

        impl Monster for $entity {}

        impl PathfinderMob for $entity {}

        impl RangedAttackMob for $entity {
            /// Vanilla `AbstractSkeleton.performRangedAttack`.
            fn perform_ranged_attack(
                &self,
                entity: &SharedEntity,
                target: &SharedEntity,
                power: f32,
            ) {
                let Some(world) = self.level() else {
                    return;
                };
                let hand = weapon_holding_hand(self, &vanilla_items::BOW);
                let mut bow = ItemStack::empty();
                self.with_equipment_slot(hand_slot(hand), &mut |stack| bow = stack.clone());
                let projectile = monster_projectile(self, &bow);
                let arrow = self.get_arrow(&world, entity, &projectile, power, &bow);
                let position = self.position();
                let target_position = target.position();
                let target_box = target.bounding_box();
                let xd = target_position.x - position.x;
                let yd = target_position.y
                    + (target_box.max_y() - target_box.min_y()) * TARGET_HEIGHT_FRACTION
                    - arrow.position().y;
                let zd = target_position.z - position.z;
                let distance_to_target = (xd * xd + zd * zd).sqrt();
                spawn_arrow_using_shoot(
                    &world,
                    &arrow,
                    &projectile,
                    entity,
                    DVec3::new(xd, yd + distance_to_target * f64::from(ARROW_ARC), zd),
                    ARROW_POWER,
                    self.ranged_attack_uncertainty(&world),
                );
                self.play_sound(
                    &sound_events::ENTITY_SKELETON_SHOOT,
                    1.0,
                    1.0 / (rand::random::<f32>() * 0.4 + 0.8),
                );
            }
        }

        impl Mob for $entity {
            fn mob_base(&self) -> &MobBase {
                &self.mob_base
            }

            fn tick_goal_selectors(&self, entity: &SharedEntity) {
                PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
            }

            fn tick_path_navigation(&self) {
                PathfinderMob::tick_pathfinder_path_navigation(self);
            }

            fn ambient_sound(&self) -> Option<SoundEventRef> {
                Some(Self::VARIANT.ambient_sound())
            }

            fn mob_flags(&self) -> i8 {
                *self.entity_data.lock().mob().mob_flags.get()
            }

            fn set_mob_flags(&self, flags: i8) {
                self.entity_data.lock().mob_mut().mob_flags.set(flags);
            }

            fn finalize_spawn(
                &self,
                world: &Arc<World>,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                let mut random = LegacyRandom::from_seed(rand::random());
                self.finalize_skeleton_spawn(world, &mut random, spawn_reason, group_data)
            }

            /// Vanilla `AbstractSkeleton.getPreferredWeaponType`; the wither
            /// skeleton has none.
            fn get_preferred_weapon_type(&self) -> Option<Identifier> {
                (Self::VARIANT != SkeletonVariant::WitherSkeleton)
                    .then_some(ItemTag::SKELETON_PREFERRED_WEAPONS)
            }

            /// Vanilla `WitherSkeleton.canHoldItem`.
            fn can_hold_item(&self, item_stack: &ItemStack) -> bool {
                Self::VARIANT != SkeletonVariant::WitherSkeleton
                    || !REGISTRY.items.is_in_tag(
                        item_stack.item(),
                        &ItemTag::WITHER_SKELETON_DISLIKED_WEAPONS,
                    )
            }

            /// Vanilla `AbstractSkeleton.wantsToPickUp`: never spears.
            fn wants_to_pick_up(&self, item_stack: &ItemStack) -> bool {
                !REGISTRY
                    .items
                    .is_in_tag(item_stack.item(), &ItemTag::SPEARS)
                    && self.can_hold_item(item_stack)
            }

            /// Vanilla `WitherSkeleton.doHurtTarget`: withers what it hits.
            fn do_hurt_target(
                &self,
                entity: &SharedEntity,
                world: &World,
                target: &SharedEntity,
            ) -> bool {
                if !self.default_do_hurt_target(entity, world, target) {
                    return false;
                }
                if Self::VARIANT == SkeletonVariant::WitherSkeleton
                    && let Some(living) = target.as_living_entity()
                {
                    let _ = living.add_mob_effect_with_source(
                        MobEffectInstance::with_duration(
                            vanilla_mob_effects::WITHER,
                            WITHER_SKELETON_WITHER_TICKS,
                            0,
                        ),
                        Some(self),
                    );
                }
                true
            }

            fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
                self.mob_interact_kind(player, hand)
            }
        }
    };
}

/// Vanilla `InteractionHand.asEquipmentSlot`.
const fn hand_slot(hand: InteractionHand) -> EquipmentSlot {
    match hand {
        InteractionHand::MainHand => EquipmentSlot::MainHand,
        InteractionHand::OffHand => EquipmentSlot::OffHand,
    }
}

/// A plain skeleton, which turns into a stray after freezing in powder snow.
#[entity_behavior(class = "Skeleton", spawn_rule)]
pub struct SkeletonEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<SkeletonEntityData>,
    freeze_state: SyncMutex<FreezeState>,
    /// The spider this jockey rider mounts once both are in the world.
    ///
    /// Vanilla calls `startRiding` inside `Spider.finalizeSpawn`, before
    /// either mob joins the level; Steel can only link entities already in it.
    pending_mount_id: SyncMutex<Option<i32>>,
}

impl_skeleton_traits!(
    SkeletonEntity,
    SkeletonVariant::Skeleton,
    "steel:entity/skeleton"
);

impl SkeletonEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = SkeletonEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let skeleton = Self {
            base,
            entity_type,
            living_base,
            mob_base: skeleton_mob_base(SkeletonVariant::Skeleton),
            entity_data: SyncMutex::new(entity_data),
            freeze_state: SyncMutex::new(FreezeState {
                affliction_time: 0,
                conversion_time: 0,
            }),
            pending_mount_id: SyncMutex::new(None),
        };
        skeleton.reassess_weapon_goal();
        skeleton
    }

    /// Vanilla `Skeleton.isFreezeConverting` (`DATA_STRAY_CONVERSION_ID`).
    #[must_use]
    pub fn is_freeze_converting(&self) -> bool {
        *self.entity_data.lock().stray_conversion.get()
    }

    fn set_freeze_converting(&self, converting: bool) {
        self.entity_data.lock().stray_conversion.set(converting);
    }

    /// Vanilla `Skeleton.startFreezeConversion`.
    pub fn start_freeze_conversion(&self, time: i32) {
        self.freeze_state.lock().conversion_time = time;
        self.set_freeze_converting(true);
    }

    /// Makes this skeleton ride `vehicle_id` on its first tick (the spider
    /// jockey of vanilla `Spider.finalizeSpawn`).
    pub(crate) fn ride_once_spawned(&self, vehicle_id: i32) {
        *self.pending_mount_id.lock() = Some(vehicle_id);
    }

    /// Vanilla `ConversionTracker.tick` for the powder-snow conversion,
    /// before `super.tick()`.
    fn tick_before_living(&self) {
        let pending_mount = self.pending_mount_id.lock().take();
        if let Some(vehicle_id) = pending_mount
            && let Some(world) = self.level()
            && let Some(vehicle) = world.get_entity_by_id(vehicle_id)
            && !vehicle.is_removed()
        {
            let _ = self.start_riding(&vehicle);
        }
        if self.level().is_none() || !LivingEntity::is_alive(self) || self.is_no_ai() {
            return;
        }
        if !self.is_in_powder_snow() {
            self.freeze_state.lock().affliction_time = NOT_CONVERTING;
            self.set_freeze_converting(false);
            return;
        }
        if self.is_freeze_converting() {
            let convert = {
                let mut state = self.freeze_state.lock();
                state.conversion_time -= 1;
                state.conversion_time < 0
            };
            if convert {
                self.do_stray_conversion();
            }
        } else {
            let start = {
                let mut state = self.freeze_state.lock();
                state.affliction_time += 1;
                state.affliction_time >= FREEZE_AFFLICTION_TICKS
            };
            if start {
                self.start_freeze_conversion(STRAY_CONVERSION_TICKS);
            }
        }
    }

    /// Vanilla `ConversionTracker.doConversion` into a stray.
    fn do_stray_conversion(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let Some(converted) = self.convert_to(
            &vanilla_entities::STRAY,
            ConversionParams::single(true, true),
            None,
        ) else {
            return;
        };
        if !converted.is_silent() {
            world.level_event(
                level_events::SOUND_SKELETON_TO_STRAY,
                converted.block_position(),
                0,
                None,
            );
        }
    }

    /// Vanilla `ConversionTracker.addAdditionalSaveData`.
    fn save_kind(&self, nbt: &mut NbtCompound) {
        let converting = self.is_freeze_converting();
        let afflicted = self.is_in_powder_snow();
        let state = self.freeze_state.lock();
        nbt.insert(
            "StrayConversionTime",
            if converting {
                state.conversion_time
            } else {
                NOT_CONVERTING
            },
        );
        nbt.insert(
            "FreezingTime",
            if afflicted {
                state.affliction_time
            } else {
                NOT_CONVERTING
            },
        );
    }

    /// Vanilla `ConversionTracker.readAdditionalSaveData`.
    fn load_kind(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.freeze_state.lock().affliction_time = nbt.int("FreezingTime").unwrap_or(0);
        let conversion_time = nbt.int("StrayConversionTime").unwrap_or(NOT_CONVERTING);
        if conversion_time == NOT_CONVERTING {
            self.set_freeze_converting(false);
        } else {
            self.start_freeze_conversion(conversion_time);
        }
    }

    #[expect(clippy::unused_self, reason = "shared skeleton interact hook")]
    const fn mob_interact_kind(
        &self,
        _player: &Player,
        _hand: InteractionHand,
    ) -> InteractionResult {
        InteractionResult::Pass
    }
}

/// Builds a skeleton kind with no extra state of its own; the struct itself
/// is declared beside each call, where the entity registry generator sees it.
macro_rules! plain_skeleton_kind {
    (
        $entity:ident, $data:ident, $variant:expr, $key:literal,
        $spawn_rule:expr
    ) => {
        impl_skeleton_traits!($entity, $variant, $key);

        impl $entity {
            pub(crate) const SPAWN_RULE: SpawnRule = $spawn_rule;

            fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
                let living_base = LivingEntityBase::new(entity_type);
                let mut entity_data = $data::new();
                living_base.initialize_synced_data(&mut entity_data);
                let skeleton = Self {
                    base,
                    entity_type,
                    living_base,
                    mob_base: skeleton_mob_base($variant),
                    entity_data: SyncMutex::new(entity_data),
                };
                skeleton.init_kind();
                skeleton.reassess_weapon_goal();
                skeleton
            }

            const fn tick_before_living(&self) {}
        }
    };
}

/// A stray: a frozen skeleton whose arrows slow their target.
#[entity_behavior(class = "Stray", spawn_rule)]
pub struct StrayEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<StrayEntityData>,
}

plain_skeleton_kind!(
    StrayEntity,
    StrayEntityData,
    SkeletonVariant::Stray,
    "steel:entity/stray",
    StrayEntity::check_stray_spawn_rules
);

impl StrayEntity {
    /// Vanilla `Stray.checkStraySpawnRules`: a monster spot that sees the
    /// sky through any powder snow above it, unless from a spawner.
    fn check_stray_spawn_rules(
        entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        random: &mut dyn rand::Rng,
    ) -> bool {
        let mut check_sky_pos = pos;
        loop {
            check_sky_pos = check_sky_pos.above();
            if level.get_block_state(check_sky_pos).get_block() != &vanilla_blocks::POWDER_SNOW {
                break;
            }
        }
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
            && (spawn_reason.is_spawner() || level.can_see_sky(check_sky_pos.below()))
    }

    #[expect(clippy::unused_self, reason = "shared skeleton init hook")]
    const fn init_kind(&self) {}
    #[expect(clippy::unused_self, reason = "shared skeleton save hook")]
    const fn save_kind(&self, _nbt: &mut NbtCompound) {}
    #[expect(clippy::unused_self, reason = "shared skeleton load hook")]
    const fn load_kind(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    #[expect(clippy::unused_self, reason = "shared skeleton interact hook")]
    const fn mob_interact_kind(
        &self,
        _player: &Player,
        _hand: InteractionHand,
    ) -> InteractionResult {
        InteractionResult::Pass
    }
}

/// A wither skeleton: a Nether swordsman whose hits wither.
#[entity_behavior(class = "WitherSkeleton", spawn_rule)]
pub struct WitherSkeletonEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<WitherSkeletonEntityData>,
}

plain_skeleton_kind!(
    WitherSkeletonEntity,
    WitherSkeletonEntityData,
    SkeletonVariant::WitherSkeleton,
    "steel:entity/wither_skeleton",
    |entity_type, level, spawn_reason, pos, random| {
        <WitherSkeletonEntity as Monster>::check_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
            random,
        )
    }
);

impl WitherSkeletonEntity {
    /// Vanilla `WitherSkeleton(...)`: lava is costly to path through.
    fn init_kind(&self) {
        self.set_pathfinding_malus(PathType::Lava, WITHER_SKELETON_LAVA_MALUS);
    }

    #[expect(clippy::unused_self, reason = "shared skeleton save hook")]
    const fn save_kind(&self, _nbt: &mut NbtCompound) {}
    #[expect(clippy::unused_self, reason = "shared skeleton load hook")]
    const fn load_kind(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    #[expect(clippy::unused_self, reason = "shared skeleton interact hook")]
    const fn mob_interact_kind(
        &self,
        _player: &Player,
        _hand: InteractionHand,
    ) -> InteractionResult {
        InteractionResult::Pass
    }
}

/// A parched: a desert skeleton whose arrows weaken their target.
#[entity_behavior(class = "Parched", spawn_rule)]
pub struct ParchedEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<ParchedEntityData>,
}

plain_skeleton_kind!(
    ParchedEntity,
    ParchedEntityData,
    SkeletonVariant::Parched,
    "steel:entity/parched",
    |entity_type, level, spawn_reason, pos, random| {
        <ParchedEntity as Monster>::check_surface_monsters_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
            random,
        )
    }
);

impl ParchedEntity {
    #[expect(clippy::unused_self, reason = "shared skeleton init hook")]
    const fn init_kind(&self) {}
    #[expect(clippy::unused_self, reason = "shared skeleton save hook")]
    const fn save_kind(&self, _nbt: &mut NbtCompound) {}
    #[expect(clippy::unused_self, reason = "shared skeleton load hook")]
    const fn load_kind(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    #[expect(clippy::unused_self, reason = "shared skeleton interact hook")]
    const fn mob_interact_kind(
        &self,
        _player: &Player,
        _hand: InteractionHand,
    ) -> InteractionResult {
        InteractionResult::Pass
    }
}

/// A bogged: a swamp skeleton with poison arrows and mushrooms to shear.
#[entity_behavior(class = "Bogged", spawn_rule)]
pub struct BoggedEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<BoggedEntityData>,
}

plain_skeleton_kind!(
    BoggedEntity,
    BoggedEntityData,
    SkeletonVariant::Bogged,
    "steel:entity/bogged",
    |entity_type, level, spawn_reason, pos, random| {
        <BoggedEntity as Monster>::check_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
            random,
        )
    }
);

impl BoggedEntity {
    #[expect(clippy::unused_self, reason = "shared skeleton init hook")]
    const fn init_kind(&self) {}

    /// Vanilla `Bogged.isSheared`.
    #[must_use]
    pub fn is_sheared(&self) -> bool {
        *self.entity_data.lock().sheared.get()
    }

    /// Vanilla `Bogged.setSheared`.
    pub fn set_sheared(&self, sheared: bool) {
        self.entity_data.lock().sheared.set(sheared);
    }

    fn save_kind(&self, nbt: &mut NbtCompound) {
        nbt.insert("sheared", i8::from(self.is_sheared()));
    }

    fn load_kind(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_sheared(nbt.byte("sheared").is_some_and(|value| value != 0));
    }

    /// Vanilla `Bogged.mobInteract`: shears take its mushrooms.
    fn mob_interact_kind(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let item_stack = {
            let inventory = player.inventory.lock();
            inventory.get_item_in_hand(hand).clone()
        };
        if !item_stack.is(&vanilla_items::SHEARS) || !self.ready_for_shearing() {
            return InteractionResult::Pass;
        }
        if let Some(world) = self.level() {
            self.shear(&world, SoundSource::Players, &item_stack);
            self.game_event_with_source_entity(&vanilla_game_events::SHEAR, Some(player));
            player
                .inventory
                .lock()
                .hurt_item_in_hand(hand, 1, player.has_infinite_materials());
        }
        InteractionResult::Success
    }
}

impl Shearable for BoggedEntity {
    /// Vanilla `Bogged.shear`: drops its mushrooms from the top of its box.
    fn shear(&self, world: &Arc<World>, source: SoundSource, tool: &ItemStack) {
        world.play_sound_at(
            &sound_events::ENTITY_BOGGED_SHEAR,
            source,
            self.position(),
            1.0,
            1.0,
            None,
        );
        let bounding_box = self.bounding_box();
        let height = bounding_box.max_y() - bounding_box.min_y();
        let mut rng = rand::rng();
        for drop in shearing_loot_items_with_rng(
            self,
            &vanilla_loot_tables::SHEARING_BOGGED,
            tool,
            &mut rng,
        ) {
            let _ = self.spawn_at_location(drop, height);
        }
        self.set_sheared(true);
    }

    fn ready_for_shearing(&self) -> bool {
        !self.is_sheared()
    }
}
