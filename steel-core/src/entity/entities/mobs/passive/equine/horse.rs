//! Vanilla `Horse`.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::data_components::vanilla_components::HORSE_VARIANT;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::entity_variant::HorseVariant;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::sound_types::SoundType;
use steel_registry::vanilla_entity_data::HorseEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{Downcast as _, DowncastType, DowncastTypeKey};

use super::{
    AbstractHorse, AbstractHorseBase, BABY_SCALE, EquineHooks, add_default_behaviour_goals,
    create_offspring, generate_jump_strength, generate_max_health, generate_speed,
    impl_equine_entity,
};
use crate::behavior::InteractionResult;
use crate::entity::SpawnRule;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobBase, AgeableMobGroupData, Animal, AnimalBase, Entity, EntityBase,
    EntityBaseLoad, EntitySpawnReason, LivingEntity, LivingEntityBase, Mob, MobBase, SharedEntity,
    SpawnGroupData,
};
use crate::player::Player;
use crate::world::World;

/// Vanilla `Horse.BABY_DIMENSIONS` passenger attachment, before the baby scale.
const BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(
    0.0,
    (1.6_f32 - 0.125_f32) as f64,
    0.0,
)];
const BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    1.396_484_4,
    1.6,
    1.52,
    EntityAttachments::new(&BABY_PASSENGER, &[], &[], &[]),
);

/// Vanilla `Markings`.
const MARKINGS_COUNT: i32 = 5;

/// A rideable, tameable horse.
#[entity_behavior(class = "Horse", spawn_rule)]
pub struct HorseEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    entity_data: SyncMutex<HorseEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `HorseEntity`.
unsafe impl DowncastType for HorseEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/horse");
}

impl HorseEntity {
    /// Vanilla `SpawnPlacements` predicate: `Animal.checkAnimalSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Animal>::check_animal_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new horse at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs a horse from persisted base entity state.
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
        let mut entity_data = HorseEntityData::new();
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
        };
        horse.register_horse_goals(&horse.mob_base, add_default_behaviour_goals);
        horse
    }

    fn type_variant(&self) -> i32 {
        *self.entity_data.lock().id_type_variant.get()
    }

    fn set_type_variant(&self, value: i32) {
        self.entity_data.lock().id_type_variant.set(value);
    }

    /// Vanilla `getVariant`.
    #[must_use]
    pub fn variant(&self) -> HorseVariant {
        HorseVariant::by_id(self.type_variant() & 0xFF)
    }

    /// Vanilla `setVariant`: keeps the markings.
    pub fn set_variant(&self, variant: HorseVariant) {
        self.set_type_variant(variant.id() & 0xFF | self.type_variant() & !0xFF);
    }

    /// Vanilla `getMarkings` id (`Markings.byId` wraps).
    #[must_use]
    pub fn markings(&self) -> i32 {
        ((self.type_variant() & 0xFF00) >> 8).rem_euclid(MARKINGS_COUNT)
    }

    /// Vanilla `setVariantAndMarkings`.
    pub fn set_variant_and_markings(&self, variant: HorseVariant, markings: i32) {
        self.set_type_variant(variant.id() & 0xFF | markings << 8 & 0xFF00);
    }

    fn baby_or(&self, baby: SoundEventRef, adult: SoundEventRef) -> SoundEventRef {
        if AgeableMob::is_baby(self) {
            baby
        } else {
            adult
        }
    }
}

impl EquineHooks for HorseEntity {
    fn baby_dimensions(&self) -> EntityDimensions {
        BABY_DIMENSIONS.scale(BABY_SCALE)
    }

    fn ambient_sound_for(&self) -> Option<SoundEventRef> {
        Some(self.baby_or(
            &sound_events::ENTITY_BABY_HORSE_AMBIENT,
            &sound_events::ENTITY_HORSE_AMBIENT,
        ))
    }

    fn hurt_sound_for(&self) -> Option<SoundEventRef> {
        Some(self.baby_or(
            &sound_events::ENTITY_BABY_HORSE_HURT,
            &sound_events::ENTITY_HORSE_HURT,
        ))
    }

    fn death_sound_for(&self) -> Option<SoundEventRef> {
        Some(self.baby_or(
            &sound_events::ENTITY_BABY_HORSE_DEATH,
            &sound_events::ENTITY_HORSE_DEATH,
        ))
    }

    /// Vanilla `Horse.mobInteract`.
    fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let should_open_inventory =
            !AgeableMob::is_baby(self) && self.is_tamed() && player.is_secondary_use_active();
        let holding_golden_dandelion = player.is_holding(&mut |item_stack: &ItemStack| {
            item_stack.is(&vanilla_items::GOLDEN_DANDELION)
        });
        if !self.is_vehicle()
            && !should_open_inventory
            && (!AgeableMob::is_baby(self) || !holding_golden_dandelion)
        {
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

    fn save_extra(&self, nbt: &mut NbtCompound) {
        nbt.insert("Variant", self.type_variant());
    }

    fn load_extra(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_type_variant(nbt.int("Variant").unwrap_or(0));
    }

    fn ai_step_extra(&self) {}

    fn drop_equipment_extra(&self) {}

    fn can_use_slot_for(&self, _slot: EquipmentSlot) -> bool {
        true
    }

    fn can_mate_for(&self, partner: &dyn Animal) -> bool {
        self.can_mate_horse_or_donkey(partner)
    }

    fn can_fall_in_love_for(&self) -> bool {
        self.in_love_time() <= 0
    }

    fn can_age_up_for(&self) -> bool {
        AgeableMob::is_baby(self) && !self.is_age_locked()
    }

    fn is_food_for(&self, item_stack: &ItemStack) -> bool {
        self.is_horse_food(item_stack)
    }

    fn remove_when_far_away_for(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_animal(dist_sqr)
    }

    /// Vanilla `Horse.getBreedOffspring`.
    fn breed_offspring_for(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity> {
        if partner.entity_type() == &vanilla_entities::DONKEY {
            let baby = create_offspring(&vanilla_entities::MULE, self, world)?;
            if let Some(baby_ageable) = baby.as_ageable_mob() {
                self.set_offspring_attributes(partner, baby_ageable);
            }
            return Some(baby);
        }

        let partner_horse = partner.downcast_ref::<Self>()?;
        let baby = create_offspring(&vanilla_entities::HORSE, self, world)?;
        let baby_horse = baby.downcast_ref::<Self>()?;
        let select_skin = rand::random_range(0..9);
        let variant = if select_skin < 4 {
            self.variant()
        } else if select_skin < 8 {
            partner_horse.variant()
        } else {
            HorseVariant::VALUES[rand::random_range(0..HorseVariant::VALUES.len())]
        };
        let select_marking = rand::random_range(0..5);
        let markings = if select_marking < 2 {
            self.markings()
        } else if select_marking < 4 {
            partner_horse.markings()
        } else {
            rand::random_range(0..MARKINGS_COUNT)
        };
        baby_horse.set_variant_and_markings(variant, markings);
        self.set_offspring_attributes(partner, baby_horse);
        Some(baby)
    }

    /// Vanilla `Horse.finalizeSpawn`.
    fn finalize_spawn_for(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let (variant, ageable) = match group_data {
            Some(SpawnGroupData::Horse(ageable, variant)) => (variant, ageable),
            _ => (
                HorseVariant::VALUES[rand::random_range(0..HorseVariant::VALUES.len())],
                AgeableMobGroupData::with_should_spawn_baby(true),
            ),
        };
        self.set_variant_and_markings(variant, rand::random_range(0..MARKINGS_COUNT));
        let result = self.finalize_horse_spawn(
            world,
            spawn_reason,
            Some(SpawnGroupData::AgeableMob(ageable)),
        );
        match result {
            Some(SpawnGroupData::AgeableMob(ageable)) => {
                Some(SpawnGroupData::Horse(ageable, variant))
            }
            other => other,
        }
    }
}

impl AbstractHorse for HorseEntity {
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
        Some(self.baby_or(
            &sound_events::ENTITY_BABY_HORSE_EAT,
            &sound_events::ENTITY_HORSE_EAT,
        ))
    }

    fn angry_sound(&self) -> Option<SoundEventRef> {
        Some(self.baby_or(
            &sound_events::ENTITY_BABY_HORSE_ANGRY,
            &sound_events::ENTITY_HORSE_ANGRY,
        ))
    }

    /// Vanilla `Horse.playGallopSound`: sometimes breathes too.
    fn play_gallop_sound(&self, sound_type: &SoundType) {
        self.play_sound(
            &sound_events::ENTITY_HORSE_GALLOP,
            sound_type.volume * 0.15,
            sound_type.pitch,
        );
        if rand::random_range(0..10) == 0 {
            let sound = self.baby_or(
                &sound_events::ENTITY_BABY_HORSE_BREATHE,
                &sound_events::ENTITY_HORSE_BREATHE,
            );
            self.play_sound(sound, sound_type.volume * 0.6, sound_type.pitch);
        }
    }

    /// Vanilla `Horse.randomizeAttributes`.
    fn randomize_attributes(&self) {
        let mut attributes = self.attributes().lock();
        attributes.set_base_value(
            vanilla_attributes::MAX_HEALTH,
            f64::from(generate_max_health(|bound| rand::random_range(0..bound))),
        );
        attributes.set_base_value(
            vanilla_attributes::MOVEMENT_SPEED,
            generate_speed(rand::random::<f64>),
        );
        attributes.set_base_value(
            vanilla_attributes::JUMP_STRENGTH,
            generate_jump_strength(rand::random::<f64>),
        );
    }
}

impl_equine_entity!(
    HorseEntity, abstract_horse, abstract_horse_mut,
    entity {
        fn apply_implicit_item_components(&self, item_stack: &ItemStack) {
            if let Some(variant) = item_stack.get(HORSE_VARIANT) {
                self.set_variant(*variant);
            }
        }
    }
    living {
        /// Vanilla `Horse.hurtArmor`: only body armor takes damage.
        fn hurt_armor(&self, source: &DamageSource, damage: f32) {
            self.do_hurt_equipment(source, damage, &[EquipmentSlot::Body]);
        }
    }
    mob {}
);
