//! Vanilla `SkeletonHorse`.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::SkeletonHorseEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{
    AbstractHorse, AbstractHorseBase, BABY_SCALE, EquineHooks, create_offspring,
    generate_jump_strength, impl_equine_entity,
};
use crate::behavior::InteractionResult;
use crate::entity::RemovalReason;
use crate::entity::SpawnRule;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, Entity, EntityBase, EntityBaseLoad,
    EntitySpawnReason, LivingEntity, LivingEntityBase, Mob, MobBase, SharedEntity, SpawnGroupData,
};
use crate::player::Player;
use crate::world::World;

/// Vanilla `SkeletonHorse.TRAP_MAX_LIFE`.
const TRAP_MAX_LIFE: i32 = 18000;

const BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(
    0.0,
    (1.6_f32 - 0.25_f32) as f64,
    0.0,
)];
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    1.396_484_4,
    1.6,
    1.52,
    EntityAttachments::new(&BABY_PASSENGER, &[], &[], &[]),
);

/// An undead horse; tamed ones can be ridden without taming.
#[entity_behavior(class = "SkeletonHorse", spawn_rule)]
pub struct SkeletonHorseEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    entity_data: SyncMutex<SkeletonHorseEntityData>,
    is_trap: AtomicBool,
    trap_time: AtomicI32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SkeletonHorseEntity`.
unsafe impl DowncastType for SkeletonHorseEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/skeleton_horse");
}

impl SkeletonHorseEntity {
    /// Vanilla `SkeletonHorse.checkSkeletonHorseSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        if !spawn_reason.is_spawner() {
            return <Self as Animal>::check_animal_spawn_rules(
                entity_type,
                level,
                spawn_reason,
                pos,
                random,
            );
        }
        spawn_reason.ignores_light_requirements()
            || <Self as Animal>::is_bright_enough_to_spawn(level, pos)
    };

    /// Creates a new skeleton horse at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a skeleton horse from persisted base entity state.
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
        let mut entity_data = SkeletonHorseEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let horse = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            animal_base: AnimalBase::new(),
            horse_base: AbstractHorseBase::new(0),
            entity_data: SyncMutex::new(entity_data),
            is_trap: AtomicBool::new(false),
            trap_time: AtomicI32::new(0),
        };
        // Vanilla `SkeletonHorse.addBehaviourGoals` adds nothing.
        horse.register_horse_goals(&horse.mob_base, |_| {});
        horse
    }

    /// Vanilla `isTrap`.
    #[must_use]
    pub fn is_trap(&self) -> bool {
        self.is_trap.load(Ordering::Relaxed)
    }

    /// Vanilla `setTrap`.
    ///
    /// Vanilla also adds or removes `SkeletonTrapGoal` here; that goal
    /// spawns skeleton riders, which Steel has no entities for yet.
    pub fn set_trap(&self, trap: bool) {
        self.is_trap.store(trap, Ordering::Relaxed);
    }
}

impl EquineHooks for SkeletonHorseEntity {
    fn baby_dimensions(&self) -> EntityDimensions {
        BABY_DIMENSIONS.scale(BABY_SCALE)
    }

    fn ambient_sound_for(&self) -> Option<SoundEventRef> {
        Some(if self.is_eye_in_water() {
            &sound_events::ENTITY_SKELETON_HORSE_AMBIENT_WATER
        } else {
            &sound_events::ENTITY_SKELETON_HORSE_AMBIENT
        })
    }

    fn hurt_sound_for(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SKELETON_HORSE_HURT)
    }

    fn death_sound_for(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SKELETON_HORSE_DEATH)
    }

    /// Vanilla `SkeletonHorse.mobInteract`: untamed ones ignore players.
    fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        if !self.is_tamed() {
            return InteractionResult::Pass;
        }
        self.horse_mob_interact(player, hand)
    }

    fn save_extra(&self, nbt: &mut NbtCompound) {
        nbt.insert("SkeletonTrap", i8::from(self.is_trap()));
        nbt.insert("SkeletonTrapTime", self.trap_time.load(Ordering::Relaxed));
    }

    fn load_extra(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_trap(nbt.byte("SkeletonTrap").is_some_and(|value| value != 0));
        self.trap_time
            .store(nbt.int("SkeletonTrapTime").unwrap_or(0), Ordering::Relaxed);
    }

    /// Vanilla `SkeletonHorse.aiStep`: an unclaimed trap despawns.
    fn ai_step_extra(&self) {
        if !self.is_persistence_required()
            && self.is_trap()
            && self.trap_time.fetch_add(1, Ordering::Relaxed) >= TRAP_MAX_LIFE
        {
            self.set_removed(RemovalReason::Discarded);
        }
    }

    fn drop_equipment_extra(&self) {}

    fn can_use_slot_for(&self, _slot: EquipmentSlot) -> bool {
        true
    }

    /// Vanilla `AbstractHorse.canMate`.
    fn can_mate_for(&self, _partner: &dyn Animal) -> bool {
        false
    }

    fn can_fall_in_love_for(&self) -> bool {
        self.in_love_time() <= 0
    }

    /// Vanilla `SkeletonHorse.canAgeUp`.
    fn can_age_up_for(&self) -> bool {
        false
    }

    fn is_food_for(&self, item_stack: &ItemStack) -> bool {
        self.is_horse_food(item_stack)
    }

    fn remove_when_far_away_for(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_animal(dist_sqr)
    }

    /// Vanilla `SkeletonHorse.getBreedOffspring`.
    fn breed_offspring_for(
        &self,
        world: &Arc<World>,
        _partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        create_offspring(&vanilla_entities::SKELETON_HORSE, self, world)
    }

    fn finalize_spawn_for(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_horse_spawn(world, spawn_reason, group_data)
    }
}

impl AbstractHorse for SkeletonHorseEntity {
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

    /// Vanilla `SkeletonHorse.playJumpSound`.
    fn play_jump_sound(&self) {
        if self.is_in_water() {
            self.play_sound(&sound_events::ENTITY_SKELETON_HORSE_JUMP_WATER, 0.4, 1.0);
        } else {
            self.play_sound(&sound_events::ENTITY_HORSE_JUMP, 0.4, 1.0);
        }
    }

    /// Vanilla `SkeletonHorse.randomizeAttributes`.
    fn randomize_attributes(&self) {
        self.attributes().lock().set_base_value(
            vanilla_attributes::JUMP_STRENGTH,
            generate_jump_strength(rand::random::<f64>),
        );
    }
}

impl_equine_entity!(
    SkeletonHorseEntity, abstract_horse, abstract_horse_mut,
    entity {
        /// Vanilla `SkeletonHorse.getSwimSound`.
        fn swim_sound(&self) -> SoundEventRef {
            if self.on_ground() {
                if !self.is_vehicle() {
                    return &sound_events::ENTITY_SKELETON_HORSE_STEP_WATER;
                }
                let counter = self
                    .horse_base
                    .gallop_sound_counter
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                if counter > 5 && counter % 3 == 0 {
                    return &sound_events::ENTITY_SKELETON_HORSE_GALLOP_WATER;
                }
                if counter <= 5 {
                    return &sound_events::ENTITY_SKELETON_HORSE_STEP_WATER;
                }
            }
            &sound_events::ENTITY_SKELETON_HORSE_SWIM
        }

        /// Vanilla `SkeletonHorse.playSwimSound`.
        fn play_swim_sound(&self, volume: f32) {
            let volume = if self.on_ground() {
                0.3
            } else {
                (volume * 25.0).min(0.1)
            };
            let pitch = 1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.4;
            self.play_sound(self.swim_sound(), volume, pitch);
        }
    }
    living {
        /// Vanilla `SkeletonHorse.getWaterSlowDown`.
        fn get_water_slow_down(&self) -> f32 {
            0.96
        }
    }
    mob {}
);
