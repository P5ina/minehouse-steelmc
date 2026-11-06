//! Vanilla `AbstractChestedHorse`, `Donkey` and `Mule`.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_macros::entity_behavior;
use steel_registry::entity_type::{
    EntityAttachmentPoint, EntityAttachments, EntityDimensions, EntityTypeRef,
};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::{DonkeyEntityData, MuleEntityData};
use steel_registry::{sound_events, vanilla_attributes, vanilla_entities, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{
    AbstractHorse, AbstractHorseBase, EquineHooks, add_default_behaviour_goals, create_offspring,
    generate_max_health, impl_equine_entity,
};
use crate::behavior::InteractionResult;
use crate::entity::{
    AgeableMob, AgeableMobBase, Animal, AnimalBase, Entity, EntityBase, EntityBaseLoad,
    EntitySpawnReason, LivingEntity, LivingEntityBase, Mob, MobBase, SharedEntity, SpawnGroupData,
};
use crate::inventory::container::Container as _;
use crate::player::Player;
use crate::world::World;

/// Vanilla `AbstractChestedHorse` baby scale.
pub(super) const CHESTED_BABY_SCALE: f32 = 0.5;
/// Vanilla `AbstractChestedHorse.getInventoryColumns` with a chest.
const CHEST_COLUMNS: usize = 5;

const DONKEY_BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(
    0.0,
    (1.5_f32 + 0.031_25_f32) as f64,
    -0.3125,
)];
const DONKEY_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    1.396_484_4,
    1.5,
    1.425,
    EntityAttachments::new(&DONKEY_BABY_PASSENGER, &[], &[], &[]),
);
const MULE_BABY_PASSENGER: [EntityAttachmentPoint; 1] = [EntityAttachmentPoint::new(
    0.0,
    (1.6_f32 + 0.031_25_f32) as f64,
    -0.3125,
)];
const MULE_BABY_DIMENSIONS: EntityDimensions = EntityDimensions::new_with_attachments(
    1.396_484_4,
    1.6,
    1.52,
    EntityAttachments::new(&MULE_BABY_PASSENGER, &[], &[], &[]),
);

/// Vanilla `AbstractChestedHorse`: a horse that can carry a chest.
pub trait AbstractChestedHorse: AbstractHorse {
    /// Vanilla `hasChest`.
    fn has_chest(&self) -> bool;

    /// Vanilla `setChest`.
    fn set_chest(&self, chest: bool);

    /// Vanilla `playChestEquipsSound`.
    fn play_chest_equips_sound(&self) {
        self.play_sound(
            &sound_events::ENTITY_DONKEY_CHEST,
            1.0,
            (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0,
        );
    }

    /// Vanilla `equipChest`.
    fn equip_chest(&self, player: &Player, hand: InteractionHand) {
        self.set_chest(true);
        self.play_chest_equips_sound();
        if !player.has_infinite_materials() {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
        }
        self.create_inventory();
    }

    /// Vanilla `AbstractChestedHorse.mobInteract`.
    fn chested_mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
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
            if !item_stack.is_empty() && !self.has_chest() && item_stack.is(&vanilla_items::CHEST) {
                self.equip_chest(player, hand);
                return InteractionResult::Success;
            }
        }
        self.horse_mob_interact(player, hand)
    }

    /// Vanilla `AbstractChestedHorse.dropEquipment`, after the inventory.
    fn drop_chest(&self) {
        if self.has_chest() {
            let _ = self.spawn_at_location(ItemStack::new(&vanilla_items::CHEST), 0.0);
            self.set_chest(false);
        }
    }

    /// Vanilla `AbstractChestedHorse.addAdditionalSaveData`.
    fn save_chest(&self, nbt: &mut NbtCompound) {
        nbt.insert("ChestedHorse", i8::from(self.has_chest()));
        if !self.has_chest() {
            return;
        }
        let inventory = self.horse_base().inventory();
        let inventory = inventory.lock();
        let items = inventory
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_empty())
            .filter_map(|(slot, item)| {
                let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() else {
                    return None;
                };
                item_nbt.insert("Slot", slot as i8);
                Some(item_nbt)
            })
            .collect();
        nbt.insert("Items", NbtList::Compound(items));
    }

    /// Vanilla `AbstractChestedHorse.readAdditionalSaveData`.
    fn load_chest(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_chest(nbt.byte("ChestedHorse").is_some_and(|value| value != 0));
        self.create_inventory();
        if !self.has_chest() {
            return;
        }
        let Some(compounds) = nbt.list("Items").and_then(|items| items.compounds()) else {
            return;
        };
        let inventory = self.horse_base().inventory();
        let mut inventory = inventory.lock();
        let size = inventory.get_container_size();
        for compound in compounds {
            let Some(slot) = compound.byte("Slot") else {
                continue;
            };
            let slot = i32::from(slot) & 0xFF;
            if (0..size as i32).contains(&slot)
                && let Some(item) = ItemStack::from_borrowed_compound(&compound)
            {
                inventory.set_item(slot as usize, item);
            }
        }
    }

    /// Vanilla `AbstractChestedHorse.randomizeAttributes`.
    fn randomize_chested_attributes(&self) {
        self.attributes().lock().set_base_value(
            vanilla_attributes::MAX_HEALTH,
            f64::from(generate_max_health(|bound| rand::random_range(0..bound))),
        );
    }
}

/// A donkey: a slow horse that carries a chest and breeds mules with horses.
#[entity_behavior(class = "Donkey", spawn_rule)]
pub struct DonkeyEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    entity_data: SyncMutex<DonkeyEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `DonkeyEntity`.
unsafe impl DowncastType for DonkeyEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/donkey");
}

/// A mule: the sterile offspring of a horse and a donkey.
#[entity_behavior(class = "Mule", spawn_rule)]
pub struct MuleEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    animal_base: AnimalBase,
    horse_base: AbstractHorseBase,
    entity_data: SyncMutex<MuleEntityData>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `MuleEntity`.
unsafe impl DowncastType for MuleEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/mule");
}

macro_rules! chested_entity {
    ($entity:ident, $data:ty, $baby_dimensions:expr,
        hooks { $($hook:item)* }
        chested { $($chested:item)* }
    ) => {
        impl $entity {
            /// Vanilla `SpawnPlacements` predicate: `Animal.checkAnimalSpawnRules`.
            pub(crate) const SPAWN_RULE: crate::entity::SpawnRule =
                |entity_type, level, spawn_reason, pos, random| {
                    <Self as Animal>::check_animal_spawn_rules(
                        entity_type,
                        level,
                        spawn_reason,
                        pos,
                        random,
                    )
                };

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
                let mut entity_data = <$data>::new();
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
        }

        impl EquineHooks for $entity {
            fn baby_dimensions(&self) -> EntityDimensions {
                $baby_dimensions.scale(CHESTED_BABY_SCALE)
            }

            fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
                self.chested_mob_interact(player, hand)
            }

            fn save_extra(&self, nbt: &mut NbtCompound) {
                self.save_chest(nbt);
            }

            fn load_extra(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
                self.load_chest(nbt);
            }

            fn ai_step_extra(&self) {}

            fn drop_equipment_extra(&self) {
                self.drop_chest();
            }

            fn can_use_slot_for(&self, slot: EquipmentSlot) -> bool {
                self.can_use_horse_slot(slot)
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

            fn finalize_spawn_for(
                &self,
                world: &Arc<World>,
                spawn_reason: EntitySpawnReason,
                group_data: Option<SpawnGroupData>,
            ) -> Option<SpawnGroupData> {
                self.finalize_horse_spawn(world, spawn_reason, group_data)
            }

            $($hook)*
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

            $($chested)*
        }

        impl_equine_entity!(
            $entity, abstract_horse, abstract_horse_mut,
            entity {}
            living {}
            mob {}
        );
    };
}

chested_entity!(
    DonkeyEntity, DonkeyEntityData, DONKEY_BABY_DIMENSIONS,
    hooks {
            fn ambient_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_DONKEY_AMBIENT)
            }

            fn hurt_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_DONKEY_HURT)
            }

            fn death_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_DONKEY_DEATH)
            }


            fn can_mate_for(&self, partner: &dyn Animal) -> bool {
                self.can_mate_horse_or_donkey(partner)
            }

            /// Vanilla `Donkey.getBreedOffspring`: a horse partner makes a mule.
            fn breed_offspring_for(
                &self,
                world: &Arc<World>,
                partner: &dyn AgeableMob,
            ) -> Option<SharedEntity> {
                let baby_type = if partner.entity_type() == &vanilla_entities::HORSE {
                    &vanilla_entities::MULE
                } else {
                    &vanilla_entities::DONKEY
                };
                let baby = create_offspring(baby_type, self, world)?;
                if let Some(baby_ageable) = baby.as_ageable_mob() {
                    self.set_offspring_attributes(partner, baby_ageable);
                }
                Some(baby)
            }
    }
    chested {}
);

chested_entity!(
    MuleEntity, MuleEntityData, MULE_BABY_DIMENSIONS,
    hooks {
            fn ambient_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_MULE_AMBIENT)
            }

            fn hurt_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_MULE_HURT)
            }

            fn death_sound_for(&self) -> Option<SoundEventRef> {
                Some(&sound_events::ENTITY_MULE_DEATH)
            }


            /// Vanilla `AbstractHorse.canMate`: mules never mate.
            fn can_mate_for(&self, _partner: &dyn Animal) -> bool {
                false
            }

            /// Vanilla `Mule.getBreedOffspring`.
            fn breed_offspring_for(
                &self,
                world: &Arc<World>,
                _partner: &dyn AgeableMob,
            ) -> Option<SharedEntity> {
                create_offspring(&vanilla_entities::MULE, self, world)
            }
    }
    chested {
            /// Vanilla `Mule.playChestEquipsSound`.
            fn play_chest_equips_sound(&self) {
                self.play_sound(
                    &sound_events::ENTITY_MULE_CHEST,
                    1.0,
                    (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0,
                );
            }
    }
);

impl AbstractHorse for DonkeyEntity {
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

    fn inventory_columns(&self) -> usize {
        if self.has_chest() { CHEST_COLUMNS } else { 0 }
    }

    fn eating_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_DONKEY_EAT)
    }

    fn angry_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_DONKEY_ANGRY)
    }

    fn play_jump_sound(&self) {
        self.play_sound(&sound_events::ENTITY_DONKEY_JUMP, 0.4, 1.0);
    }

    fn randomize_attributes(&self) {
        self.randomize_chested_attributes();
    }
}

impl AbstractHorse for MuleEntity {
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

    fn inventory_columns(&self) -> usize {
        if self.has_chest() { CHEST_COLUMNS } else { 0 }
    }

    fn eating_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_MULE_EAT)
    }

    fn angry_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_MULE_ANGRY)
    }

    fn play_jump_sound(&self) {
        self.play_sound(&sound_events::ENTITY_MULE_JUMP, 0.4, 1.0);
    }

    fn randomize_attributes(&self) {
        self.randomize_chested_attributes();
    }
}
