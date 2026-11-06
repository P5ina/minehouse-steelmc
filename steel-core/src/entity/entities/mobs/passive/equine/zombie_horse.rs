//! Vanilla `ZombieHorse`.

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
use steel_registry::vanilla_entity_data::ZombieHorseEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{
    AbstractHorse, AbstractHorseBase, BABY_SCALE, EquineHooks, create_offspring, impl_equine_entity,
};
use crate::behavior::InteractionResult;
use crate::entity::ai::goal::{FloatGoal, TemptGoal};
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, Entity, EntityBase, EntityBaseLoad,
    EntitySpawnReason, LivingEntity, LivingEntityBase, Mob, MobBase, SharedEntity, SpawnGroupData,
    SpawnRule, create_entity_instance, is_dark_enough_to_spawn, is_spawn_allowed_on_difficulty,
    start_riding_entities,
};
use crate::player::Player;
use crate::world::World;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{REGISTRY, TaggedRegistryExt};

/// Vanilla `ZombieHorse.SPEED_FACTOR`.
const SPEED_FACTOR: f32 = 42.16;
/// Vanilla `ZombieHorse.BASE_JUMP_STRENGTH`.
const BASE_JUMP_STRENGTH: f64 = 0.5;
/// Vanilla `ZombieHorse.PER_RANDOM_JUMP_STRENGTH`.
const PER_RANDOM_JUMP_STRENGTH: f64 = 0.066_666_666_666_666_67;
/// Vanilla `ZombieHorse.BASE_SPEED`.
const BASE_SPEED: f64 = 9.0;
/// Vanilla `ZombieHorse.PER_RANDOM_SPEED`.
const PER_RANDOM_SPEED: f64 = 1.0;

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

/// An undead horse that zombies ride.
#[entity_behavior(class = "ZombieHorse", spawn_rule)]
pub struct ZombieHorseEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    entity_data: SyncMutex<ZombieHorseEntityData>,
    /// The jockey from `finalizeSpawn`, seated on the first tick once this
    /// horse has a shared handle in the world.
    pending_rider: SyncMutex<Option<SharedEntity>>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ZombieHorseEntity`.
unsafe impl DowncastType for ZombieHorseEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/zombie_horse");
}

impl ZombieHorseEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        is_spawn_allowed_on_difficulty(entity_type, level)
            && (spawn_reason.ignores_light_requirements()
                || is_dark_enough_to_spawn(level, pos, random))
            && <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Vanilla `ZombieHorse.generateZombieHorseJumpStrength`.
    fn generate_jump_strength(mut probability: impl FnMut() -> f64) -> f64 {
        BASE_JUMP_STRENGTH
            + probability() * PER_RANDOM_JUMP_STRENGTH
            + probability() * PER_RANDOM_JUMP_STRENGTH
            + probability() * PER_RANDOM_JUMP_STRENGTH
    }

    /// Vanilla `ZombieHorse.generateZombieHorseSpeed`.
    fn generate_speed(mut probability: impl FnMut() -> f64) -> f64 {
        (BASE_SPEED
            + probability() * PER_RANDOM_SPEED
            + probability() * PER_RANDOM_SPEED
            + probability() * PER_RANDOM_SPEED)
            / f64::from(SPEED_FACTOR)
    }

    /// Creates a new zombie horse at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a zombie horse from persisted base entity state.
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
        let mut entity_data = ZombieHorseEntityData::new();
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
            pending_rider: SyncMutex::new(None),
        };
        horse.register_horse_goals(&horse.mob_base, add_zombie_horse_behaviour_goals);
        horse
    }
}

impl EquineHooks for ZombieHorseEntity {
    fn baby_dimensions(&self) -> EntityDimensions {
        BABY_DIMENSIONS.scale(BABY_SCALE)
    }

    fn ambient_sound_for(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOMBIE_HORSE_AMBIENT)
    }

    fn hurt_sound_for(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOMBIE_HORSE_HURT)
    }

    fn death_sound_for(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOMBIE_HORSE_DEATH)
    }

    /// Vanilla `ZombieHorse.mobInteract`.
    fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let should_open_inventory =
            !AgeableMob::is_baby(self) && self.is_tamed() && player.is_secondary_use_active();
        if !self.is_vehicle() && !should_open_inventory {
            let item_stack = {
                let inventory = player.inventory.lock();
                let item_stack = inventory.get_item_in_hand(hand);
                item_stack.copy_with_count(item_stack.count())
            };
            if let Some(result) = self.equine_feed_interact(player, hand, &item_stack) {
                return result;
            }
        }
        self.horse_mob_interact(player, hand)
    }

    fn save_extra(&self, _nbt: &mut NbtCompound) {}

    fn load_extra(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    fn ai_step_extra(&self) {}

    fn drop_equipment_extra(&self) {}

    fn can_use_slot_for(&self, _slot: EquipmentSlot) -> bool {
        true
    }

    /// Vanilla `AbstractHorse.canMate`.
    fn can_mate_for(&self, _partner: &dyn Animal) -> bool {
        false
    }

    /// Vanilla `ZombieHorse.canFallInLove`.
    fn can_fall_in_love_for(&self) -> bool {
        false
    }

    /// Vanilla `ZombieHorse.canAgeUp`.
    fn can_age_up_for(&self) -> bool {
        false
    }

    /// Vanilla `ZombieHorse.isFood`.
    fn is_food_for(&self, item_stack: &ItemStack) -> bool {
        is_zombie_horse_food(item_stack)
    }

    /// Vanilla `ZombieHorse.removeWhenFarAway`.
    fn remove_when_far_away_for(&self, _dist_sqr: f64) -> bool {
        true
    }

    /// Vanilla `ZombieHorse.getBreedOffspring`.
    fn breed_offspring_for(
        &self,
        world: &Arc<World>,
        _partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        create_offspring(&vanilla_entities::ZOMBIE_HORSE, self, world)
    }

    /// Vanilla `ZombieHorse.finalizeSpawn`: a natural spawn brings a
    /// spear-wielding zombie rider.
    fn finalize_spawn_for(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        if spawn_reason == EntitySpawnReason::Natural
            && let Ok(zombie) =
                create_entity_instance(world, &vanilla_entities::ZOMBIE, self.position())
        {
            zombie.set_rotation((self.rotation().0, 0.0));
            if let Some(zombie_mob) = zombie.as_mob() {
                let _ = zombie_mob.finalize_spawn(world, spawn_reason, None);
            }
            if let Some(zombie_living) = zombie.as_living_entity() {
                let _ = zombie_living.living_base().equipment().lock().set(
                    EquipmentSlot::MainHand,
                    ItemStack::new(&vanilla_items::IRON_SPEAR),
                );
            }
            *self.pending_rider.lock() = Some(zombie);
        }
        self.finalize_horse_spawn(world, spawn_reason, group_data)
    }

    /// Adds and seats the jockey from `finalizeSpawn`.
    fn tick_extra(&self, entity: &SharedEntity) {
        let Some(rider) = self.pending_rider.lock().take() else {
            return;
        };
        if self.is_removed() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        if world.try_add_entity(Arc::clone(&rider)).is_ok() {
            let _ = start_riding_entities(&rider, entity);
        }
    }
}

impl AbstractHorse for ZombieHorseEntity {
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

    fn eating_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOMBIE_HORSE_EAT)
    }

    fn angry_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ZOMBIE_HORSE_ANGRY)
    }

    /// Vanilla `ZombieHorse.isMobControlled`.
    fn is_mob_controlled(&self) -> bool {
        self.first_passenger()
            .is_some_and(|passenger| passenger.is_mob())
    }

    /// Vanilla `ZombieHorse.randomizeAttributes`.
    fn randomize_attributes(&self) {
        let mut attributes = self.attributes().lock();
        attributes.set_base_value(
            vanilla_attributes::JUMP_STRENGTH,
            Self::generate_jump_strength(rand::random::<f64>),
        );
        attributes.set_base_value(
            vanilla_attributes::MOVEMENT_SPEED,
            Self::generate_speed(rand::random::<f64>),
        );
    }
}

impl_equine_entity!(
    ZombieHorseEntity, abstract_horse, abstract_horse_mut,
    entity {
        /// Vanilla `ZombieHorse.interact`: any interaction keeps it around.
        fn interact(
            &self,
            player: &Player,
            hand: InteractionHand,
            location: DVec3,
        ) -> InteractionResult {
            self.set_persistence_required();
            self.interact_mob(player, hand, location)
        }
    }
    living {}
    mob {
        /// Vanilla `ZombieHorse.canBeLeashed`.
        fn mob_can_be_leashed(&self) -> bool {
            self.is_tamed() || !self.is_mob_controlled()
        }

        /// Vanilla `ZombieHorse.sunProtectionSlot`.
        fn sun_protection_slot(&self) -> EquipmentSlot {
            EquipmentSlot::Body
        }
    }
);

/// Vanilla `ZombieHorse.isFood`: the zombie horse food tag.
fn is_zombie_horse_food(item_stack: &ItemStack) -> bool {
    REGISTRY
        .items
        .is_in_tag(item_stack.item(), &ItemTag::ZOMBIE_HORSE_FOOD)
}

/// Vanilla `ZombieHorse.addBehaviourGoals`.
fn add_zombie_horse_behaviour_goals(mob_base: &MobBase) {
    let mut goal_selector = mob_base.goal_selector().lock();
    goal_selector.add_goal(0, FloatGoal::new(mob_base));
    goal_selector.add_goal(3, TemptGoal::new(1.25, is_zombie_horse_food, false));
}
