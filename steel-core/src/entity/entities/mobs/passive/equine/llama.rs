//! Vanilla `Llama` and `TraderLlama`.
//!
//! Llamas are chested horses with a strength (chest columns), a coat
//! variant, spit at whoever hurt them, chase wild wolves and walk in
//! caravans behind a leashed llama. Trader llamas come with a wandering
//! trader, defend it and despawn on their own.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::data_components::vanilla_components::LLAMA_VARIANT;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::entity_variant::LlamaVariant;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::{LlamaEntityData, TraderLlamaEntityData};
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_entities,
    vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use super::chested::{AbstractChestedHorse, CHESTED_BABY_SCALE};
use super::{
    AbstractHorse, AbstractHorseBase, EquineHooks, create_offspring, goals, impl_equine_entity,
};
use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{
    BreedGoal, FloatGoal, FollowParentGoal, Goal, GoalControls, HurtByTargetGoal,
    LlamaFollowCaravanGoal, LookAtPlayerGoal, NearestAttackableTargetGoal, PanicGoal,
    RandomLookAroundGoal, RangedAttackGoal, TemptGoal, TraderLlamaDefendWanderingTraderGoal,
    WaterAvoidingRandomStrollGoal, is_leashed, wandering_trader_holder,
};
use crate::entity::damage::DamageSource;
use crate::entity::entities::LlamaSpitEntity;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, Entity, EntityBase,
    EntityBaseLoad, EntitySpawnReason, LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob,
    Projectile as _, RangedAttackMob, RemovalReason, SharedEntity, SpawnGroupData, WeakEntity,
    is_abstract_illager, next_entity_id,
};
use crate::entity::{SpawnRule, TamableAnimal};
use crate::player::Player;
use crate::world::World;

/// Vanilla `Llama.MAX_STRENGTH`.
const MAX_STRENGTH: i32 = 5;
/// Vanilla `setRandomStrength`: most llamas roll a strength of up to 3.
const COMMON_MAX_STRENGTH: i32 = 3;
/// Vanilla `random.nextFloat() < 0.04F` chance to roll up to full strength.
const STRONG_LLAMA_CHANCE: f32 = 0.04;
/// Vanilla `random.nextFloat() < 0.03F` chance a baby beats its parents.
const STRONGER_BABY_CHANCE: f32 = 0.03;
/// Vanilla `Llama.getMaxTemper`.
const MAX_TEMPER: i32 = 30;
/// Llamas only take fall damage from six blocks up.
const MIN_HURTING_FALL_DISTANCE: f64 = 6.0;
/// Vanilla `Llama.followLeashSpeed`.
const FOLLOW_LEASH_SPEED: f64 = 2.0;
/// Vanilla `getNavigation().setRequiredPathLength(40.0F)`.
const REQUIRED_PATH_LENGTH: f32 = 40.0;
/// Vanilla `LlamaFollowCaravanGoal(this, 2.1F)`, widened from a float.
const CARAVAN_SPEED: f32 = 2.1;
/// Vanilla `RangedAttackGoal(this, 1.25, 40, 20.0F)`.
const SPIT_GOAL_SPEED: f64 = 1.25;
const SPIT_GOAL_INTERVAL: i32 = 40;
const SPIT_GOAL_RADIUS: f32 = 20.0;
/// Vanilla `Projectile.spawnProjectileUsingShoot(..., 1.5F, ...)`.
const SPIT_POWER: f32 = 1.5;
/// Vanilla `Llama.rangedAttackUncertainty`.
const SPIT_UNCERTAINTY: f32 = 10.0;
/// Vanilla `Math.sqrt(xd * xd + zd * zd) * 0.2F` arc.
const SPIT_ARC: f32 = 0.2;
/// Vanilla `target.getY(0.3333333333333333)`.
const TARGET_HEIGHT_FRACTION: f64 = 0.333_333_333_333_333_3;
/// Vanilla `LlamaAttackWolfGoal`: `randomInterval` 16 and a quarter of the
/// follow range.
const WOLF_TARGET_INTERVAL: i32 = 16;
const WOLF_FOLLOW_DISTANCE_SCALE: f64 = 0.25;
/// Vanilla `TraderLlama.DEFAULT_DESPAWN_DELAY`.
const TRADER_DESPAWN_DELAY: i32 = 47_999;
/// Vanilla `TraderLlama.registerGoals` `PanicGoal(this, 2.0)`.
const TRADER_PANIC_SPEED: f64 = 2.0;

/// Vanilla `Llama.BABY_DIMENSIONS` before the 0.5 scale: the llama size with
/// the passenger seat at `height - 0.25`.
const BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(
    0.0,
    (1.87_f32 - 0.25_f32) as f64,
    -0.3,
)];
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    0.9,
    1.87,
    1.7765,
    EntityAttachments::new(&BABY_PASSENGER, &[], &[], &[]),
);

/// State every llama keeps beside its synced data.
pub struct LlamaBase {
    /// Vanilla `didSpit`: cleared by `LlamaHurtByTargetGoal`.
    did_spit: AtomicBool,
    /// Vanilla `caravanHead`: the llama this one follows.
    caravan_head: SyncMutex<Option<WeakEntity>>,
    /// Vanilla `caravanTail`: the llama following this one.
    caravan_tail: SyncMutex<Option<WeakEntity>>,
    /// Vanilla `TraderLlama.despawnDelay`, only counted by trader llamas.
    despawn_delay: AtomicI32,
}

impl LlamaBase {
    #[must_use]
    fn new() -> Self {
        Self {
            did_spit: AtomicBool::new(false),
            caravan_head: SyncMutex::new(None),
            caravan_tail: SyncMutex::new(None),
            despawn_delay: AtomicI32::new(TRADER_DESPAWN_DELAY),
        }
    }
}

/// Vanilla `Llama`: shared by llamas and trader llamas.
pub trait Llama: AbstractChestedHorse {
    /// The llama's unsynced state.
    fn llama_base(&self) -> &LlamaBase;

    /// Vanilla `getStrength`.
    fn strength(&self) -> i32;

    /// Writes the synced strength without clamping.
    fn set_strength_unclamped(&self, strength: i32);

    /// Vanilla `getVariant`.
    fn variant(&self) -> LlamaVariant;

    /// Vanilla `setVariant`.
    fn set_variant(&self, variant: LlamaVariant);

    /// Vanilla `isTraderLlama`.
    fn is_trader_llama(&self) -> bool {
        false
    }

    /// Vanilla `setStrength`: clamped to 1..=5.
    fn set_strength(&self, strength: i32) {
        self.set_strength_unclamped(strength.clamp(1, MAX_STRENGTH));
    }

    /// Vanilla `setRandomStrength`.
    fn set_random_strength(&self) {
        let max_strength = if rand::random::<f32>() < STRONG_LLAMA_CHANCE {
            MAX_STRENGTH
        } else {
            COMMON_MAX_STRENGTH
        };
        self.set_strength(1 + rand::random_range(0..max_strength));
    }

    /// Vanilla `inCaravan`.
    fn in_caravan(&self) -> bool {
        self.llama_base().caravan_head.lock().is_some()
    }

    /// Vanilla `hasCaravanTail`.
    fn has_caravan_tail(&self) -> bool {
        self.llama_base().caravan_tail.lock().is_some()
    }

    /// Vanilla `getCaravanHead`, `None` once the head left the world.
    fn caravan_head(&self) -> Option<SharedEntity> {
        self.llama_base()
            .caravan_head
            .lock()
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// Vanilla `joinCaravan`: `me` starts following `head`.
    fn join_caravan(&self, me: &SharedEntity, head: &SharedEntity) {
        *self.llama_base().caravan_head.lock() = Some(Arc::downgrade(head));
        if let Some(head) = head.as_llama() {
            *head.llama_base().caravan_tail.lock() = Some(Arc::downgrade(me));
        }
    }

    /// Vanilla `leaveCaravan`.
    fn leave_caravan(&self) {
        let head = self.llama_base().caravan_head.lock().take();
        if let Some(head) = head.as_ref().and_then(Weak::upgrade)
            && let Some(head) = head.as_llama()
        {
            *head.llama_base().caravan_tail.lock() = None;
        }
    }

    /// Whether a player may mount this llama right now.
    fn allows_player_ride(&self) -> bool {
        true
    }

    /// The entity type a breeding pair of this llama makes.
    fn offspring_type(&self) -> EntityTypeRef {
        &vanilla_entities::LLAMA
    }

    /// Spawn-group handling that runs ahead of `Llama.finalizeSpawn`.
    fn prepare_finalize_spawn(
        &self,
        _spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        group_data
    }

    /// Saves the fields only this llama type has.
    fn save_llama_extra(&self, _nbt: &mut NbtCompound) {}

    /// Loads the fields only this llama type has.
    fn load_llama_extra(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    /// Runs after the shared llama `aiStep`.
    fn llama_ai_step_extra(&self) {}
}

/// Vanilla `Llama.spit`.
fn spit<L: Llama + ?Sized>(llama: &L, entity: &SharedEntity, target: &SharedEntity) {
    if let Some(world) = llama.level() {
        let position = llama.position();
        let spawn = LlamaSpitEntity::spawn_position(
            position,
            llama.base().dimensions().width,
            llama.get_eye_y(),
            llama.y_body_rot(),
        );
        let spit = LlamaSpitEntity::new(
            &vanilla_entities::LLAMA_SPIT,
            next_entity_id(),
            spawn,
            Arc::downgrade(&world),
        );
        spit.set_owner_entity(Some(entity));
        let target_position = target.position();
        let target_box = target.bounding_box();
        let xd = target_position.x - position.x;
        let yd = target_position.y
            + (target_box.max_y() - target_box.min_y()) * TARGET_HEIGHT_FRACTION
            - spawn.y;
        let zd = target_position.z - position.z;
        let yo = (xd * xd + zd * zd).sqrt() * f64::from(SPIT_ARC);
        // Vanilla's `onProjectileSpawned` runs with an empty weapon and does nothing.
        spit.shoot(DVec3::new(xd, yd + yo, zd), SPIT_POWER, SPIT_UNCERTAINTY);
        if let Err(error) = world.try_add_entity(Arc::new(spit)) {
            log::debug!("world rejected llama spit: {error}");
        }
    }
    llama.play_sound(
        &sound_events::ENTITY_LLAMA_SPIT,
        1.0,
        1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2,
    );
    llama.llama_base().did_spit.store(true, Ordering::Relaxed);
}

/// Vanilla `Llama.LlamaHurtByTargetGoal`: drops the target once the llama spat.
struct LlamaHurtByTargetGoal {
    hurt_by: HurtByTargetGoal,
}

impl LlamaHurtByTargetGoal {
    const fn new() -> Self {
        Self {
            hurt_by: HurtByTargetGoal::new(),
        }
    }
}

impl Goal for LlamaHurtByTargetGoal {
    fn controls(&self) -> GoalControls {
        self.hurt_by.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.hurt_by.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if mob
            .as_llama()
            .is_some_and(|llama| llama.llama_base().did_spit.swap(false, Ordering::Relaxed))
        {
            return false;
        }
        self.hurt_by.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.stop(mob);
    }
}

/// Vanilla `Llama.isFood`.
fn is_llama_food(item_stack: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item_stack.item(), &ItemTag::LLAMA_FOOD)
}

/// Vanilla `Llama.registerGoals`.
fn register_llama_goals(mob_base: &MobBase) {
    {
        let mut goal_selector = mob_base.goal_selector().lock();
        goal_selector.add_goal(0, FloatGoal::new(mob_base));
        goal_selector.add_goal(1, goals::RunAroundLikeCrazyGoal::new(1.2));
        goal_selector.add_goal(2, LlamaFollowCaravanGoal::new(f64::from(CARAVAN_SPEED)));
        goal_selector.add_goal(
            3,
            RangedAttackGoal::new(SPIT_GOAL_SPEED, SPIT_GOAL_INTERVAL, SPIT_GOAL_RADIUS),
        );
        goal_selector.add_goal(3, PanicGoal::new(1.2));
        goal_selector.add_goal(4, BreedGoal::new(1.0));
        goal_selector.add_goal(
            5,
            TemptGoal::new(
                1.25,
                |item_stack| {
                    REGISTRY
                        .items
                        .is_in_tag(item_stack.item(), &ItemTag::LLAMA_TEMPT_ITEMS)
                },
                false,
            ),
        );
        goal_selector.add_goal(6, FollowParentGoal::new(1.0));
        goal_selector.add_goal(7, WaterAvoidingRandomStrollGoal::new(0.7));
        goal_selector.add_goal(8, LookAtPlayerGoal::new(6.0));
        goal_selector.add_goal(9, RandomLookAroundGoal::new());
    }
    let mut target_selector = mob_base.target_selector().lock();
    target_selector.add_goal(1, LlamaHurtByTargetGoal::new());
    target_selector.add_goal(
        2,
        NearestAttackableTargetGoal::new_with_interval(
            WOLF_TARGET_INTERVAL,
            false,
            true,
            |target, _world| {
                target.entity_type() == &vanilla_entities::WOLF
                    && !target
                        .as_tamable_animal()
                        .is_some_and(TamableAnimal::is_tame)
            },
        )
        .with_follow_distance_scale(WOLF_FOLLOW_DISTANCE_SCALE),
    );
}

/// Vanilla `instanceof Zombie` minus zombified piglins.
fn is_zombie_but_not_piglin(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::ZOMBIE
        || entity_type == &vanilla_entities::HUSK
        || entity_type == &vanilla_entities::DROWNED
        || entity_type == &vanilla_entities::ZOMBIE_VILLAGER
}

/// Vanilla `TraderLlama.registerGoals`, after the llama goals.
fn register_trader_llama_goals(mob_base: &MobBase) {
    register_llama_goals(mob_base);
    mob_base
        .goal_selector()
        .lock()
        .add_goal(1, PanicGoal::new(TRADER_PANIC_SPEED));
    let mut target_selector = mob_base.target_selector().lock();
    target_selector.add_goal(1, TraderLlamaDefendWanderingTraderGoal::new());
    target_selector.add_goal(
        2,
        NearestAttackableTargetGoal::new(true, |target, _world| {
            is_zombie_but_not_piglin(target.as_entity_event_source())
        }),
    );
    target_selector.add_goal(
        2,
        NearestAttackableTargetGoal::new(true, |target, _world| {
            is_abstract_illager(target.as_entity_event_source())
        }),
    );
}

/// A llama: a chested horse that spits and walks in caravans.
#[entity_behavior(class = "Llama", spawn_rule)]
pub struct LlamaEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    llama_base: LlamaBase,
    entity_data: SyncMutex<LlamaEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `LlamaEntity`.
unsafe impl DowncastType for LlamaEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/llama");
}

/// A trader llama: the llama a wandering trader leads.
#[entity_behavior(class = "TraderLlama")]
pub struct TraderLlamaEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    llama_base: LlamaBase,
    entity_data: SyncMutex<TraderLlamaEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TraderLlamaEntity`.
unsafe impl DowncastType for TraderLlamaEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/trader_llama");
}

impl LlamaEntity {
    /// Vanilla `SpawnPlacements` predicate: `Animal.checkAnimalSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Animal>::check_animal_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };
}

impl TraderLlamaEntity {
    /// Vanilla `TraderLlama.setDespawnDelay`.
    pub fn set_despawn_delay(&self, despawn_delay: i32) {
        self.llama_base
            .despawn_delay
            .store(despawn_delay, Ordering::Relaxed);
    }

    /// Vanilla `TraderLlama.canDespawn`.
    fn can_despawn(&self) -> bool {
        let leashed_to_other = is_leashed(self) && wandering_trader_holder(self).is_none();
        !self.is_tamed()
            && !leashed_to_other
            && !self.has_exactly_one_player_passenger()
            && !self.is_age_locked()
            && !self.is_persistence_required()
    }
}

macro_rules! llama_entity {
    ($entity:ident, $data:ty, $register_goals:path,
        llama { $($llama:item)* }
    ) => {
        impl $entity {
            /// Creates a new entity at runtime.
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

            /// Reconstructs the entity from persisted base entity state.
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
                let follow_range = living_base
                    .attributes()
                    .lock()
                    .get_value(vanilla_attributes::FOLLOW_RANGE)
                    .unwrap_or(vanilla_attributes::FOLLOW_RANGE.default_value);
                mob_base
                    .navigation()
                    .lock()
                    .set_required_path_length(REQUIRED_PATH_LENGTH, follow_range);
                let mut entity_data = <$data>::new();
                living_base.initialize_synced_data(&mut entity_data);
                let llama = Self {
                    base,
                    entity_type,
                    living_base,
                    mob_base,
                    ageable_base: AgeableMobBase::new(),
                    animal_base: AnimalBase::new(),
                    horse_base: AbstractHorseBase::new(0),
                    llama_base: LlamaBase::new(),
                    entity_data: SyncMutex::new(entity_data),
                };
                $register_goals(&llama.mob_base);
                llama
            }
        }

        impl EquineHooks for $entity {
            fn baby_dimensions(&self) -> EntityDimensions {
                BABY_DIMENSIONS.scale(CHESTED_BABY_SCALE)
            }

            fn ambient_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_LLAMA_AMBIENT)
            }

            fn hurt_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_LLAMA_HURT)
            }

            fn death_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_LLAMA_DEATH)
            }

            fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
                self.chested_mob_interact(player, hand)
            }

            /// Vanilla `Llama.addAdditionalSaveData`.
            fn save_extra(&self, nbt: &mut NbtCompound) {
                self.save_chest(nbt);
                nbt.insert("Variant", self.variant().id());
                nbt.insert("Strength", self.strength());
                self.save_llama_extra(nbt);
            }

            /// Vanilla `Llama.readAdditionalSaveData`: the strength comes
            /// first so the chest inventory gets its columns.
            fn load_extra(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.set_strength(nbt.int("Strength").unwrap_or(0));
                self.load_chest(nbt);
                self.set_variant(LlamaVariant::by_id(nbt.int("Variant").unwrap_or(0)));
                self.load_llama_extra(nbt);
            }

            fn ai_step_extra(&self) {
                self.llama_ai_step_extra();
            }

            fn drop_equipment_extra(&self) {
                self.drop_chest();
            }

            /// Vanilla `Llama.canUseSlot`.
            fn can_use_slot_for(&self, _slot: EquipmentSlot) -> bool {
                true
            }

            fn follow_leash_speed_for(&self) -> f64 {
                FOLLOW_LEASH_SPEED
            }

            /// Vanilla `Llama.canMate`.
            fn can_mate_for(&self, partner: &dyn Animal) -> bool {
                partner.id() != self.id()
                    && partner.as_llama().is_some_and(AbstractHorse::can_parent)
                    && self.can_parent()
            }

            fn can_fall_in_love_for(&self) -> bool {
                self.in_love_time() <= 0
            }

            fn can_age_up_for(&self) -> bool {
                AgeableMob::is_baby(self) && !self.is_age_locked()
            }

            fn is_food_for(&self, item_stack: &ItemStack) -> bool {
                is_llama_food(item_stack)
            }

            fn remove_when_far_away_for(&self, dist_sqr: f64) -> bool {
                self.remove_when_far_away_animal(dist_sqr)
            }

            /// Vanilla `Llama.getBreedOffspring`.
            fn breed_offspring_for(
                &self,
                world: &Arc<World>,
                partner: &dyn AgeableMob,
            ) -> Option<SharedEntity> {
                let partner_llama = partner.as_llama()?;
                let baby = create_offspring(self.offspring_type(), self, world)?;
                let baby_llama = baby.as_llama()?;
                if self.is_trader_llama() {
                    baby_llama.set_persistence_required();
                }
                self.set_offspring_attributes(partner, baby_llama);
                let mut baby_strength =
                    rand::random_range(0..self.strength().max(partner_llama.strength())) + 1;
                if rand::random::<f32>() < STRONGER_BABY_CHANCE {
                    baby_strength += 1;
                }
                baby_llama.set_strength(baby_strength);
                baby_llama.set_variant(if rand::random::<bool>() {
                    self.variant()
                } else {
                    partner_llama.variant()
                });
                Some(baby)
            }

            /// Vanilla `Llama.finalizeSpawn`.
            fn finalize_spawn_for(
                &self,
                world: &Arc<World>,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                let group_data = self.prepare_finalize_spawn(spawn_reason, group_data);
                self.set_random_strength();
                let (variant, ageable) = match group_data {
                    Some(SpawnGroupData::Llama(ageable, variant)) => (variant, ageable),
                    _ => (
                        LlamaVariant::VALUES[rand::random_range(0..LlamaVariant::VALUES.len())],
                        AgeableMobGroupData::with_should_spawn_baby(true),
                    ),
                };
                self.set_variant(variant);
                match self.finalize_horse_spawn(
                    world,
                    spawn_reason,
                    Some(SpawnGroupData::AgeableMob(ageable)),
                ) {
                    Some(SpawnGroupData::AgeableMob(ageable)) => {
                        Some(SpawnGroupData::Llama(ageable, variant))
                    }
                    other => other,
                }
            }
        }

        impl AbstractChestedHorse for $entity {
            fn has_chest(&self) -> bool {
                *self.entity_data.lock().abstract_chested_horse().id_chest.get()
            }

            fn set_chest(&self, chest: bool) {
                self.entity_data
                    .lock()
                    .abstract_chested_horse_mut()
                    .id_chest
                    .set(chest);
            }

            /// Vanilla `Llama.playChestEquipsSound`.
            fn play_chest_equips_sound(&self) {
                self.play_sound(
                    &sound_events::ENTITY_LLAMA_CHEST,
                    1.0,
                    (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0,
                );
            }
        }

        impl AbstractHorse for $entity {
            fn horse_base(&self) -> &AbstractHorseBase {
                &self.horse_base
            }

            fn horse_flags(&self) -> i8 {
                *self.entity_data.lock().abstract_horse().id_flags.get()
            }

            fn set_horse_flags(&self, flags: i8) {
                self.entity_data
                    .lock()
                    .abstract_horse_mut()
                    .id_flags
                    .set(flags);
            }

            fn can_gallop(&self) -> bool {
                false
            }

            /// Vanilla `Llama.getInventoryColumns`.
            fn inventory_columns(&self) -> usize {
                if self.has_chest() {
                    usize::try_from(self.strength()).unwrap_or(0)
                } else {
                    0
                }
            }

            fn max_temper(&self) -> i32 {
                MAX_TEMPER
            }

            fn can_perform_rearing(&self) -> bool {
                false
            }

            fn can_eat_grass(&self) -> bool {
                false
            }

            fn eating_sound(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_LLAMA_EAT)
            }

            fn angry_sound(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_LLAMA_ANGRY)
            }

            fn randomize_attributes(&self) {
                self.randomize_chested_attributes();
            }

            /// Vanilla `Llama.isImmobile`.
            fn is_horse_immobile(&self) -> bool {
                self.default_is_immobile() || self.is_eating()
            }

            /// Vanilla `Llama.playStepSound`.
            fn horse_play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
                self.play_sound(&sound_events::ENTITY_LLAMA_STEP, 0.15, 1.0);
            }

            /// Vanilla `Llama.causeFallDamage`: no landing sound, and no damage
            /// below six blocks.
            fn horse_cause_fall_damage(
                &self,
                fall_distance: f64,
                damage_modifier: f32,
                source: &DamageSource,
            ) -> bool {
                let damage = self.calculate_horse_fall_damage(fall_distance, damage_modifier);
                if damage <= 0 {
                    return false;
                }
                if fall_distance >= MIN_HURTING_FALL_DISTANCE {
                    if let Some(world) = self.level() {
                        self.hurt(&world, source, damage as f32);
                    }
                    self.propagate_fall_to_passengers(fall_distance, damage_modifier, source);
                }
                self.play_block_fall_sound();
                true
            }

            /// Vanilla `Llama.followMommy`: caravan llamas follow the caravan.
            fn follow_mommy(&self) {
                if !self.in_caravan() && AgeableMob::is_baby(self) {
                    self.default_follow_mommy();
                }
            }

            fn do_player_ride(&self, player: &Player) {
                if self.allows_player_ride() {
                    self.default_do_player_ride(player);
                }
            }

            /// Vanilla `Llama.handleEating`.
            fn handle_eating(&self, player: &Player, item_stack: &ItemStack) -> bool {
                let mut item_used = false;
                let (age_up, temper, heal) = if item_stack.is(&vanilla_items::WHEAT) {
                    (10, 3, 2.0)
                } else if item_stack.is(&vanilla_items::HAY_BLOCK) {
                    if self.is_tamed() && self.get_age() == 0 && self.can_fall_in_love() {
                        item_used = true;
                        self.set_in_love(Some(player));
                    }
                    (90, 6, 10.0)
                } else {
                    (0, 0, 0.0)
                };

                if self.get_health() < self.get_max_health() && heal > 0.0 {
                    self.heal(heal);
                    item_used = true;
                }

                if AgeableMob::is_baby(self) && age_up > 0 && !self.is_age_locked() {
                    // Vanilla's HAPPY_VILLAGER particle here is client-only.
                    self.age_up(age_up, false);
                    item_used = true;
                }

                if temper > 0
                    && (item_used || !self.is_tamed())
                    && self.temper() < self.max_temper()
                {
                    self.modify_temper(temper);
                    item_used = true;
                }

                if item_used && let Some(sound) = self.eating_sound() {
                    self.play_sound(
                        sound,
                        1.0,
                        1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2,
                    );
                }
                item_used
            }
        }

        impl Llama for $entity {
            fn llama_base(&self) -> &LlamaBase {
                &self.llama_base
            }

            fn strength(&self) -> i32 {
                *self.entity_data.lock().llama().strength.get()
            }

            fn set_strength_unclamped(&self, strength: i32) {
                self.entity_data.lock().llama_mut().strength.set(strength);
            }

            fn variant(&self) -> LlamaVariant {
                LlamaVariant::by_id(*self.entity_data.lock().llama().variant.get())
            }

            fn set_variant(&self, variant: LlamaVariant) {
                self.entity_data
                    .lock()
                    .llama_mut()
                    .variant
                    .set(variant.id());
            }

            $($llama)*
        }

        impl RangedAttackMob for $entity {
            fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, _power: f32) {
                spit(self, entity, target);
            }

            fn ranged_attack_uncertainty(&self, _world: &World) -> f32 {
                SPIT_UNCERTAINTY
            }
        }

        impl_equine_entity!(
            $entity, abstract_horse, abstract_horse_mut,
            entity {
                fn llama_strength(&self) -> Option<i32> {
                    Some(self.strength())
                }

                fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
                    if let Some(variant) = item_stack.get(LLAMA_VARIANT) {
                        self.set_variant(*variant);
                    }
                }
            }
            living {}
            mob {}
        );
    };
}

llama_entity!(LlamaEntity, LlamaEntityData, register_llama_goals, llama {});

llama_entity!(
    TraderLlamaEntity, TraderLlamaEntityData, register_trader_llama_goals,
    llama {
        fn is_trader_llama(&self) -> bool {
            true
        }

        /// Vanilla `TraderLlama.doPlayerRide`: not while a trader leads it.
        fn allows_player_ride(&self) -> bool {
            wandering_trader_holder(self).is_none()
        }

        /// Vanilla `TraderLlama.makeNewLlama`.
        fn offspring_type(&self) -> EntityTypeRef {
            &vanilla_entities::TRADER_LLAMA
        }

        /// Vanilla `TraderLlama.finalizeSpawn`, ahead of `Llama.finalizeSpawn`.
        fn prepare_finalize_spawn(
            &self,
            spawn_reason: EntitySpawnReason,
            group_data: Option<SpawnGroupData>,
        ) -> Option<SpawnGroupData> {
            if spawn_reason == EntitySpawnReason::Event {
                self.set_age(0);
            }
            group_data.or(Some(SpawnGroupData::AgeableMob(
                AgeableMobGroupData::with_should_spawn_baby(false),
            )))
        }

        fn save_llama_extra(&self, nbt: &mut NbtCompound) {
            nbt.insert(
                "DespawnDelay",
                self.llama_base.despawn_delay.load(Ordering::Relaxed),
            );
        }

        fn load_llama_extra(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
            self.set_despawn_delay(nbt.int("DespawnDelay").unwrap_or(TRADER_DESPAWN_DELAY));
        }

        /// Vanilla `TraderLlama.maybeDespawn`. Vanilla copies the despawn
        /// delay of the wandering trader that leads the llama; Steel has no
        /// wandering trader yet, so the llama counts down its own.
        fn llama_ai_step_extra(&self) {
            if !self.can_despawn() {
                return;
            }
            let delay = self.llama_base.despawn_delay.fetch_sub(1, Ordering::Relaxed) - 1;
            if delay <= 0 {
                if let Some(leashable) = self.as_leashable() {
                    leashable.remove_leash();
                }
                self.set_removed(RemovalReason::Discarded);
            }
        }
    }
);
