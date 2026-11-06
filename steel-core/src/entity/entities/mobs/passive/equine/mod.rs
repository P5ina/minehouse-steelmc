//! Vanilla `AbstractHorse` and the equines built on it.
//!
//! Horses, donkeys, mules, skeleton horses and zombie horses share taming,
//! riding, feeding and breeding through the [`AbstractHorse`] trait; each
//! concrete entity only supplies its sounds, offspring and attributes.

mod chested;
mod goals;
mod horse;
mod skeleton_horse;
#[cfg(test)]
mod tests;
mod zombie_horse;

use std::f32::consts::PI;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::attribute::AttributeRef;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::enchantment_effect::EnchantmentEffectComponent;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::sound_types::{self, SoundType};
use steel_registry::vanilla_attributes;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt, sound_events, vanilla_blocks, vanilla_entities,
    vanilla_game_events, vanilla_items,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId};
use uuid::Uuid;

use crate::behavior::{ITEM_BEHAVIORS, InteractionResult};
use crate::entity::ai::goal::{
    BreedGoal, FloatGoal, FollowParentGoal, LookAtPlayerGoal, RandomLookAroundGoal, TemptGoal,
    WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AgeableMob, AgeableMobGroupData, Animal, ENTITIES, Entity, EntityReference, EntitySpawnReason,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, next_entity_id,
};
use crate::inventory::container::Container as _;
use crate::inventory::container::SimpleContainer;
use crate::player::Player;
use crate::world::World;

pub use chested::{DonkeyEntity, MuleEntity};
pub use horse::HorseEntity;
pub use skeleton_horse::SkeletonHorseEntity;
pub use zombie_horse::ZombieHorseEntity;

/// Vanilla `AbstractHorse.FLAG_TAME`.
const FLAG_TAME: i8 = 2;
/// Vanilla `AbstractHorse.FLAG_BRED`.
const FLAG_BRED: i8 = 8;
/// Vanilla `AbstractHorse.FLAG_EATING`.
const FLAG_EATING: i8 = 16;
/// Vanilla `AbstractHorse.FLAG_STANDING`.
const FLAG_STANDING: i8 = 32;
/// Vanilla `AbstractHorse.FLAG_OPEN_MOUTH`.
const FLAG_OPEN_MOUTH: i8 = 64;
/// Vanilla `AbstractHorse.BABY_SCALE`.
pub(super) const BABY_SCALE: f32 = 0.7;
/// Vanilla `AbstractHorse.BREEDING_CROSS_FACTOR`.
const BREEDING_CROSS_FACTOR: f64 = 0.15;
/// Vanilla `AbstractHorse.getMaxTemper`.
const MAX_TEMPER: i32 = 100;
/// Vanilla `AbstractMountInventoryMenu.getInventorySize` rows.
const INVENTORY_ROWS: usize = 3;
/// Vanilla `AbstractHorse.MOMMY_TARGETING` range.
const MOMMY_RANGE: f64 = 16.0;
/// Vanilla `AbstractHorse.getAmbientSoundInterval`.
pub(crate) const AMBIENT_SOUND_INTERVAL: i32 = 400;
/// Vanilla `AbstractHorse.getSoundVolume`.
pub(crate) const SOUND_VOLUME: f32 = 0.8;
/// Vanilla `AbstractHorse.getMaxSpawnClusterSize`.
pub(crate) const MAX_SPAWN_CLUSTER_SIZE: i32 = 6;
/// Vanilla `AbstractHorse.finalizeSpawn` baby chance for a fresh group.
const HORSE_BABY_SPAWN_CHANCE: f32 = 0.2;

/// Vanilla `AbstractHorse.generateMaxHealth`, given a bounded int source.
pub(super) fn generate_max_health(mut int_by_bound: impl FnMut(i32) -> i32) -> f32 {
    let first = int_by_bound(8);
    let second = int_by_bound(9);
    15.0 + first as f32 + second as f32
}

/// Vanilla `AbstractHorse.generateJumpStrength`, given a `[0, 1)` source.
pub(super) fn generate_jump_strength(mut probability: impl FnMut() -> f64) -> f64 {
    f64::from(0.4_f32) + probability() * 0.2 + probability() * 0.2 + probability() * 0.2
}

/// Vanilla `AbstractHorse.generateSpeed`, given a `[0, 1)` source.
pub(super) fn generate_speed(mut probability: impl FnMut() -> f64) -> f64 {
    (f64::from(0.45_f32) + probability() * 0.3 + probability() * 0.3 + probability() * 0.3) * 0.25
}

fn min_health() -> f64 {
    f64::from(generate_max_health(|_| 0))
}

fn max_health() -> f64 {
    f64::from(generate_max_health(|bound| bound - 1))
}

fn min_jump_strength() -> f64 {
    f64::from(generate_jump_strength(|| 0.0) as f32)
}

fn max_jump_strength() -> f64 {
    f64::from(generate_jump_strength(|| 1.0) as f32)
}

fn min_movement_speed() -> f64 {
    f64::from(generate_speed(|| 0.0) as f32)
}

fn max_movement_speed() -> f64 {
    f64::from(generate_speed(|| 1.0) as f32)
}

/// Vanilla `AbstractHorse.createOffspringAttribute`.
///
/// Returns `None` for an empty range, where vanilla throws.
#[must_use]
pub fn create_offspring_attribute(
    parent_a: f64,
    parent_b: f64,
    range_min: f64,
    range_max: f64,
) -> Option<f64> {
    if range_max <= range_min {
        return None;
    }
    let parent_a = parent_a.clamp(range_min, range_max);
    let parent_b = parent_b.clamp(range_min, range_max);
    let margin = BREEDING_CROSS_FACTOR * (range_max - range_min);
    let range = (parent_a - parent_b).abs() + margin * 2.0;
    let average = f64::midpoint(parent_a, parent_b);
    let baby_quality =
        (rand::random::<f64>() + rand::random::<f64>() + rand::random::<f64>()) / 3.0 - 0.5;
    let new_value = average + range * baby_quality;
    Some(if new_value > range_max {
        range_max - (new_value - range_max)
    } else if new_value < range_min {
        range_min + (range_min - new_value)
    } else {
        new_value
    })
}

/// Shared mutable state for vanilla `AbstractHorse`.
pub struct AbstractHorseBase {
    eating_counter: AtomicI32,
    mouth_counter: AtomicI32,
    stand_counter: AtomicI32,
    tail_counter: AtomicI32,
    sprint_counter: AtomicI32,
    temper: AtomicI32,
    gallop_sound_counter: AtomicI32,
    allow_stand_sliding: AtomicBool,
    player_jump_pending_scale: SyncMutex<f32>,
    /// Vanilla `standAnim` and `standAnimO`; the old value offsets riders.
    stand_anim: SyncMutex<(f32, f32)>,
    owner: SyncMutex<Option<EntityReference>>,
    inventory: SyncMutex<SimpleContainer>,
}

impl AbstractHorseBase {
    /// Creates horse state with vanilla defaults and an inventory of `columns`.
    #[must_use]
    pub fn new(columns: usize) -> Self {
        Self {
            eating_counter: AtomicI32::new(0),
            mouth_counter: AtomicI32::new(0),
            stand_counter: AtomicI32::new(0),
            tail_counter: AtomicI32::new(0),
            sprint_counter: AtomicI32::new(0),
            temper: AtomicI32::new(0),
            gallop_sound_counter: AtomicI32::new(0),
            allow_stand_sliding: AtomicBool::new(false),
            player_jump_pending_scale: SyncMutex::new(0.0),
            stand_anim: SyncMutex::new((0.0, 0.0)),
            owner: SyncMutex::new(None),
            inventory: SyncMutex::new(SimpleContainer::new(columns * INVENTORY_ROWS)),
        }
    }

    /// The horse's container, sized by vanilla `getInventorySize`.
    #[must_use]
    pub const fn inventory(&self) -> &SyncMutex<SimpleContainer> {
        &self.inventory
    }
}

fn is_wood_sound_type(sound_type: &SoundType) -> bool {
    [
        sound_types::WOOD,
        sound_types::NETHER_WOOD,
        sound_types::STEM,
        sound_types::CHERRY_WOOD,
        sound_types::BAMBOO_WOOD,
    ]
    .iter()
    .any(|wood| {
        wood.step_sound.key == sound_type.step_sound.key
            && wood.break_sound.key == sound_type.break_sound.key
    })
}

/// Vanilla `AbstractHorse`: a tameable, rideable animal.
pub trait AbstractHorse: Animal + PathfinderMob {
    /// Shared horse state.
    fn horse_base(&self) -> &AbstractHorseBase;

    /// Vanilla `DATA_ID_FLAGS`.
    fn horse_flags(&self) -> i8;

    /// Writes vanilla `DATA_ID_FLAGS`.
    fn set_horse_flags(&self, flags: i8);

    /// Vanilla `getFlag`.
    fn get_flag(&self, flag: i8) -> bool {
        self.horse_flags() & flag != 0
    }

    /// Vanilla `setFlag`.
    fn set_flag(&self, flag: i8, value: bool) {
        let current = self.horse_flags();
        self.set_horse_flags(if value {
            current | flag
        } else {
            current & !flag
        });
    }

    /// Vanilla `isTamed`.
    fn is_tamed(&self) -> bool {
        self.get_flag(FLAG_TAME)
    }

    /// Vanilla `setTamed`.
    fn set_tamed(&self, tamed: bool) {
        self.set_flag(FLAG_TAME, tamed);
    }

    /// Vanilla `isEating`.
    fn is_eating(&self) -> bool {
        self.get_flag(FLAG_EATING)
    }

    /// Vanilla `setEating`.
    fn set_eating(&self, eating: bool) {
        self.set_flag(FLAG_EATING, eating);
    }

    /// Vanilla `isStanding`.
    fn is_standing(&self) -> bool {
        self.get_flag(FLAG_STANDING)
    }

    /// Vanilla `isBred`.
    fn is_bred(&self) -> bool {
        self.get_flag(FLAG_BRED)
    }

    /// Vanilla `setBred`.
    fn set_bred(&self, bred: bool) {
        self.set_flag(FLAG_BRED, bred);
    }

    /// Vanilla `getOwnerReference` UUID.
    fn owner_uuid(&self) -> Option<Uuid> {
        self.horse_base()
            .owner
            .lock()
            .as_ref()
            .map(EntityReference::uuid)
    }

    /// Vanilla `setOwner`.
    fn set_owner(&self, owner: Option<&SharedEntity>) {
        *self.horse_base().owner.lock() = owner.map(EntityReference::from_entity);
    }

    /// Vanilla `getTemper`.
    fn temper(&self) -> i32 {
        self.horse_base().temper.load(Ordering::Relaxed)
    }

    /// Vanilla `setTemper`.
    fn set_temper(&self, temper: i32) {
        self.horse_base().temper.store(temper, Ordering::Relaxed);
    }

    /// Vanilla `getMaxTemper`.
    fn max_temper(&self) -> i32 {
        MAX_TEMPER
    }

    /// Vanilla `modifyTemper`.
    fn modify_temper(&self, amount: i32) -> i32 {
        let temper = (self.temper() + amount).clamp(0, self.max_temper());
        self.set_temper(temper);
        temper
    }

    /// Vanilla `canPerformRearing`.
    fn can_perform_rearing(&self) -> bool {
        true
    }

    /// Vanilla `canEatGrass`.
    fn can_eat_grass(&self) -> bool {
        true
    }

    /// Vanilla `isMobControlled`.
    fn is_mob_controlled(&self) -> bool {
        false
    }

    /// Vanilla `canGallop`: chested horses never gallop.
    fn can_gallop(&self) -> bool {
        true
    }

    /// Vanilla `getInventoryColumns`.
    fn inventory_columns(&self) -> usize {
        0
    }

    /// Vanilla `getEatingSound`.
    fn eating_sound(&self) -> Option<SoundEventRef> {
        None
    }

    /// Vanilla `getAngrySound`.
    fn angry_sound(&self) -> Option<SoundEventRef> {
        None
    }

    /// Vanilla `getAmbientStandSound`.
    fn ambient_stand_sound(&self) -> Option<SoundEventRef> {
        self.ambient_sound()
    }

    /// Vanilla `getAmbientStandInterval`.
    fn ambient_stand_interval(&self) -> i32 {
        self.ambient_sound_interval()
    }

    /// Vanilla `playJumpSound`.
    fn play_jump_sound(&self) {
        self.play_sound(&sound_events::ENTITY_HORSE_JUMP, 0.4, 1.0);
    }

    /// Vanilla `playGallopSound`.
    fn play_gallop_sound(&self, sound_type: &SoundType) {
        self.play_sound(
            &sound_events::ENTITY_HORSE_GALLOP,
            sound_type.volume * 0.15,
            sound_type.pitch,
        );
    }

    /// Vanilla `createInventory`: resizes the container, keeping what fits.
    fn create_inventory(&self) {
        let size = self.inventory_columns() * INVENTORY_ROWS;
        let mut inventory = self.horse_base().inventory.lock();
        let old = mem::replace(&mut *inventory, SimpleContainer::new(size));
        let max = old.items().len().min(size);
        for (slot, item) in old.items().iter().take(max).enumerate() {
            if !item.is_empty() {
                inventory.set_item(slot, item.copy_with_count(item.count()));
            }
        }
    }

    /// Vanilla `eating`.
    fn eating(&self) {
        self.open_mouth();
        if !self.is_silent()
            && let Some(eating_sound) = self.eating_sound()
            && let Some(world) = self.level()
        {
            world.play_sound_at(
                eating_sound,
                self.sound_source(),
                self.position(),
                1.0,
                1.0 + (rand::random::<f32>() - rand::random::<f32>()) * 0.2,
                None,
            );
        }
    }

    /// Vanilla `openMouth`.
    fn open_mouth(&self) {
        self.horse_base().mouth_counter.store(1, Ordering::Relaxed);
        self.set_flag(FLAG_OPEN_MOUTH, true);
    }

    /// Vanilla `moveTail`.
    fn move_tail(&self) {
        self.horse_base().tail_counter.store(1, Ordering::Relaxed);
    }

    /// Vanilla `setStanding`.
    fn set_standing(&self, ticks: i32) {
        self.set_eating(false);
        self.set_flag(FLAG_STANDING, true);
        self.horse_base()
            .stand_counter
            .store(ticks, Ordering::Relaxed);
    }

    /// Vanilla `clearStanding`.
    fn clear_standing(&self) {
        self.set_flag(FLAG_STANDING, false);
        self.horse_base().stand_counter.store(0, Ordering::Relaxed);
    }

    /// Vanilla `standIfPossible`.
    fn stand_if_possible(&self) {
        if self.can_perform_rearing() {
            self.set_standing(20);
        }
    }

    /// Vanilla `makeMad`.
    fn make_mad(&self) {
        if !self.is_standing() {
            self.stand_if_possible();
            self.make_sound(self.angry_sound());
        }
    }

    /// Vanilla `tameWithName`.
    fn tame_with_name(&self, player: &SharedEntity) -> bool {
        self.set_owner(Some(player));
        self.set_tamed(true);
        // TODO: Trigger the tame_animal advancement once criteria triggers exist.
        self.broadcast_entity_event(EntityStatus::TamingSucceeded);
        true
    }

    /// Vanilla `isImmobile`.
    fn is_horse_immobile(&self) -> bool {
        self.default_is_immobile() && self.is_vehicle() && self.is_saddled()
            || self.is_eating()
            || self.is_standing()
    }

    /// Vanilla `isPushable`.
    fn is_horse_pushable(&self) -> bool {
        !self.is_vehicle()
    }

    /// Vanilla `canUseSlot`.
    fn can_use_horse_slot(&self, slot: EquipmentSlot) -> bool {
        if slot == EquipmentSlot::Saddle {
            Entity::is_alive(self) && !AgeableMob::is_baby(self) && self.is_tamed()
        } else {
            true
        }
    }

    /// Vanilla `canDispenserEquipIntoSlot`.
    fn can_dispenser_equip_horse_slot(&self, slot: EquipmentSlot) -> bool {
        (slot == EquipmentSlot::Body || slot == EquipmentSlot::Saddle) && self.is_tamed()
            || Mob::can_pick_up_loot(self)
    }

    /// Vanilla `getEquipSound` for the saddle slot.
    fn horse_equip_sound(&self, slot: EquipmentSlot, stack: &ItemStack) -> Option<SoundEventRef> {
        if slot == EquipmentSlot::Saddle {
            return Some(&sound_events::ENTITY_HORSE_SADDLE);
        }
        let equippable = stack.get_equippable()?;
        (slot == equippable.slot)
            .then(|| equippable.equip_sound.registry_ref())
            .flatten()
    }

    /// Vanilla `isWearingBodyArmor`.
    fn is_wearing_body_armor(&self) -> bool {
        let mut wearing = false;
        self.with_equipment_slot(EquipmentSlot::Body, &mut |item_stack| {
            wearing = !item_stack.is_empty()
                && self.is_equippable_in_slot(item_stack, EquipmentSlot::Body);
        });
        wearing
    }

    /// Vanilla `equipBodyArmor`.
    fn equip_body_armor(&self, player: &Player, hand: InteractionHand) {
        let armor = {
            let mut inventory = player.inventory.lock();
            if !self.is_equippable_in_slot(inventory.get_item_in_hand(hand), EquipmentSlot::Body) {
                return;
            }
            if player.has_infinite_materials() {
                let held = inventory.get_item_in_hand(hand);
                held.copy_with_count(1)
            } else {
                inventory.split_item_in_hand(hand, 1)
            }
        };
        let previous = {
            let mut equipment = self.living_base().equipment().lock();
            let previous = equipment.take(EquipmentSlot::Body);
            equipment.set(EquipmentSlot::Body, armor.copy_with_count(armor.count()));
            previous
        };
        self.on_equip_item(EquipmentSlot::Body, &previous, &armor);
        self.set_guaranteed_drop(EquipmentSlot::Body);
    }

    /// Vanilla `causeFallDamage`.
    fn horse_cause_fall_damage(
        &self,
        fall_distance: f64,
        damage_modifier: f32,
        source: &DamageSource,
    ) -> bool {
        if fall_distance > 1.0 {
            let sound = if AgeableMob::is_baby(self) {
                &sound_events::ENTITY_BABY_HORSE_LAND
            } else {
                &sound_events::ENTITY_HORSE_LAND
            };
            self.play_sound(sound, 0.4, 1.0);
        }

        let damage = {
            let attributes = self.attributes().lock();
            let safe_fall_distance = attributes
                .get_value(vanilla_attributes::SAFE_FALL_DISTANCE)
                .unwrap_or(vanilla_attributes::SAFE_FALL_DISTANCE.default_value);
            let fall_damage_multiplier = attributes
                .get_value(vanilla_attributes::FALL_DAMAGE_MULTIPLIER)
                .unwrap_or(vanilla_attributes::FALL_DAMAGE_MULTIPLIER.default_value);
            LivingEntityBase::calculate_fall_damage(
                fall_distance,
                damage_modifier,
                safe_fall_distance,
                fall_damage_multiplier,
            )
        };
        if damage <= 0 {
            return false;
        }
        if let Some(world) = self.level() {
            self.hurt(&world, source, damage as f32);
        }
        self.propagate_fall_to_passengers(fall_distance, damage_modifier, source);
        self.play_block_fall_sound();
        true
    }

    /// Vanilla `hurtServer`: a hurt horse sometimes rears.
    fn horse_hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let was_hurt = self.default_hurt_server(world, source, amount);
        if was_hurt && rand::random_range(0..3) == 0 {
            self.stand_if_possible();
        }
        was_hurt
    }

    /// Vanilla `playStepSound`.
    fn horse_play_step_sound(&self, pos: BlockPos, block_state: BlockStateId) {
        let block = block_state.get_block();
        if block.config.liquid {
            return;
        }
        let mut sound_type = block.config.sound_type;
        if let Some(world) = self.level() {
            let above = world.get_block_state(pos.above());
            if above.get_block() == &vanilla_blocks::SNOW {
                sound_type = above.get_block().config.sound_type;
            }
        }

        if self.is_vehicle() && self.can_gallop() {
            let counter = self
                .horse_base()
                .gallop_sound_counter
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if counter > 5 && counter % 3 == 0 {
                self.play_gallop_sound(&sound_type);
            } else if counter <= 5 {
                self.play_sound(
                    &sound_events::ENTITY_HORSE_STEP_WOOD,
                    sound_type.volume * 0.15,
                    sound_type.pitch,
                );
            }
        } else if is_wood_sound_type(&sound_type) {
            self.play_sound(
                &sound_events::ENTITY_HORSE_STEP_WOOD,
                sound_type.volume * 0.15,
                sound_type.pitch,
            );
        } else {
            let sound = if AgeableMob::is_baby(self) {
                &sound_events::ENTITY_BABY_HORSE_STEP
            } else {
                &sound_events::ENTITY_HORSE_STEP
            };
            self.play_sound(sound, sound_type.volume * 0.15, sound_type.pitch);
        }
    }

    /// Vanilla `fedFood`.
    fn fed_food(
        &self,
        player: &Player,
        hand: InteractionHand,
        item_stack: &ItemStack,
    ) -> InteractionResult {
        let ate_food = self.handle_eating(player, item_stack);
        if ate_food && !player.has_infinite_materials() {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
        }
        if ate_food {
            InteractionResult::SuccessServer
        } else {
            InteractionResult::Pass
        }
    }

    /// Vanilla `handleEating`.
    fn handle_eating(&self, player: &Player, item_stack: &ItemStack) -> bool {
        let mut item_used = false;
        let (heal, age_up, temper, breeds) = if item_stack.is(&vanilla_items::WHEAT) {
            (2.0, 20, 3, false)
        } else if item_stack.is(&vanilla_items::SUGAR) {
            (1.0, 30, 3, false)
        } else if item_stack.is(&vanilla_items::HAY_BLOCK) {
            (20.0, 180, 0, false)
        } else if item_stack.is(&vanilla_items::APPLE) {
            (3.0, 60, 3, false)
        } else if item_stack.is(&vanilla_items::RED_MUSHROOM) {
            (3.0, 0, 3, false)
        } else if item_stack.is(&vanilla_items::CARROT) {
            (3.0, 60, 3, false)
        } else if item_stack.is(&vanilla_items::GOLDEN_CARROT) {
            (4.0, 60, 5, true)
        } else if item_stack.is(&vanilla_items::GOLDEN_APPLE)
            || item_stack.is(&vanilla_items::ENCHANTED_GOLDEN_APPLE)
        {
            (10.0, 240, 10, true)
        } else {
            (0.0, 0, 0, false)
        };

        if breeds && self.is_tamed() && self.get_age() == 0 && !self.is_in_love() {
            item_used = true;
            self.set_in_love(Some(player));
        }

        if self.get_health() < self.get_max_health() && heal > 0.0 {
            self.heal(heal);
            item_used = true;
        }

        if AgeableMob::is_baby(self) && age_up > 0 && !self.is_age_locked() {
            // Vanilla's HAPPY_VILLAGER particle here is client-only.
            self.age_up(age_up, false);
            item_used = true;
        }

        if temper > 0 && (item_used || !self.is_tamed()) && self.temper() < self.max_temper() {
            self.modify_temper(temper);
            item_used = true;
        }

        if item_used {
            self.eating();
            self.game_event(&vanilla_game_events::EAT);
        }

        item_used
    }

    /// Vanilla `doPlayerRide`.
    fn do_player_ride(&self, player: &Player) {
        self.set_eating(false);
        self.clear_standing();
        player.set_rotation(self.rotation());
        if let Some(world) = self.level()
            && let Some(vehicle) = world.get_entity_by_id(self.id())
        {
            let _ = player.start_riding(&vehicle);
        }
    }

    /// Vanilla `isFood`.
    fn is_horse_food(&self, item_stack: &ItemStack) -> bool {
        REGISTRY
            .items
            .is_in_tag(item_stack.item(), &ItemTag::HORSE_FOOD)
    }

    /// Vanilla `dropEquipment`: drops the inventory on death.
    fn drop_horse_inventory(&self) {
        let items: Vec<ItemStack> = {
            let mut inventory = self.horse_base().inventory.lock();
            (0..inventory.items().len())
                .filter_map(|slot| {
                    let item = inventory.get_item(slot);
                    (!item.is_empty()
                        && !item.has_enchantment_effect(
                            EnchantmentEffectComponent::PreventEquipmentDrop,
                        ))
                    .then(|| inventory.remove_item_no_update(slot))
                })
                .collect()
        };
        for item in items {
            let _ = self.spawn_at_location(item, 0.0);
        }
    }

    /// Vanilla `AbstractHorse.aiStep`, run after the shared animal step.
    ///
    /// `before` runs vanilla's pre-`super.aiStep` tail roll.
    fn horse_ai_step_before(&self) {
        if rand::random_range(0..200) == 0 {
            self.move_tail();
        }
    }

    /// The server half of vanilla `AbstractHorse.aiStep` after `super.aiStep`.
    fn horse_ai_step_after(&self) {
        if !Entity::is_alive(self) {
            return;
        }
        if rand::random_range(0..900) == 0 && self.living_base().death_time() == 0 {
            self.heal(1.0);
        }

        if self.can_eat_grass() {
            if !self.is_eating()
                && !self.is_vehicle()
                && rand::random_range(0..300) == 0
                && self.level().is_some_and(|world| {
                    world
                        .get_block_state(self.block_position().below())
                        .get_block()
                        == &vanilla_blocks::GRASS_BLOCK
                })
            {
                self.set_eating(true);
            }

            if self.is_eating() {
                let counter = self
                    .horse_base()
                    .eating_counter
                    .fetch_add(1, Ordering::Relaxed)
                    + 1;
                if counter > 50 {
                    self.horse_base().eating_counter.store(0, Ordering::Relaxed);
                    self.set_eating(false);
                }
            }
        }

        self.follow_mommy();
    }

    /// Vanilla `followMommy`.
    fn follow_mommy(&self) {
        if !(self.is_bred() && AgeableMob::is_baby(self) && !self.is_eating()) {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let targeting = TargetingConditions::for_non_combat()
            .range(MOMMY_RANGE)
            .ignore_line_of_sight()
            .selector(|target, _world| {
                target
                    .as_abstract_horse()
                    .is_some_and(<dyn AbstractHorse>::is_bred)
            });
        let search = self.bounding_box().inflate(MOMMY_RANGE);
        let Some(me) = self.as_living_entity() else {
            return;
        };
        let mommy = world.nearest_entity_in_aabb_matching(&search, self.position(), |entity| {
            entity.is_abstract_horse()
                && entity
                    .as_living_entity()
                    .is_some_and(|horse| targeting.test(world.as_ref(), Some(me), horse))
        });
        if let Some(mommy) = mommy
            && self.position().distance_squared(mommy.position()) > 4.0
        {
            // Vanilla only creates the path here and never follows it.
            let _ = self.create_path_to(mommy.block_position(), 0);
        }
    }

    /// Vanilla `AbstractHorse.tick`, after `super.tick`.
    fn horse_tick(&self) {
        let base = self.horse_base();
        let mouth = base.mouth_counter.load(Ordering::Relaxed);
        if mouth > 0 {
            base.mouth_counter.store(mouth + 1, Ordering::Relaxed);
            if mouth + 1 > 30 {
                base.mouth_counter.store(0, Ordering::Relaxed);
                self.set_flag(FLAG_OPEN_MOUTH, false);
            }
        }

        let stand = base.stand_counter.load(Ordering::Relaxed);
        if stand > 0 {
            base.stand_counter.store(stand - 1, Ordering::Relaxed);
            if stand - 1 <= 0 {
                self.clear_standing();
            }
        }

        let tail = base.tail_counter.load(Ordering::Relaxed);
        if tail > 0 {
            base.tail_counter.store(tail + 1, Ordering::Relaxed);
            if tail + 1 > 8 {
                base.tail_counter.store(0, Ordering::Relaxed);
            }
        }

        let sprint = base.sprint_counter.load(Ordering::Relaxed);
        if sprint > 0 {
            base.sprint_counter.store(sprint + 1, Ordering::Relaxed);
            if sprint + 1 > 300 {
                base.sprint_counter.store(0, Ordering::Relaxed);
            }
        }

        // Vanilla also animates eating and mouth here; only the stand
        // animation feeds back into server state (rider offset).
        let standing = self.is_standing();
        let mut stand_anim = base.stand_anim.lock();
        let (current, _) = *stand_anim;
        let next = if standing {
            (current + ((1.0 - current) * 0.4 + 0.05)).min(1.0)
        } else {
            base.allow_stand_sliding.store(false, Ordering::Relaxed);
            (current + ((0.8 * current * current * current - current) * 0.6 - 0.05)).max(0.0)
        };
        *stand_anim = (next, current);
    }

    /// Vanilla `standAnimO`.
    fn stand_anim_old(&self) -> f32 {
        self.horse_base().stand_anim.lock().1
    }

    /// Vanilla `getPassengerAttachmentPoint` stand offset, before yaw rotation.
    fn horse_passenger_offset(&self, scale: f32) -> DVec3 {
        let stand = f64::from(self.stand_anim_old());
        let scale = f64::from(scale);
        let offset = DVec3::new(0.0, 0.15 * stand * scale, -0.7 * stand * scale);
        let yaw = f64::from(-self.rotation().0 * (PI / 180.0));
        let (sin, cos) = yaw.sin_cos();
        DVec3::new(
            offset.x * cos + offset.z * sin,
            offset.y,
            offset.z * cos - offset.x * sin,
        )
    }

    /// Vanilla `mobInteract` for an adult, unridden horse.
    fn horse_mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        if self.is_vehicle() || AgeableMob::is_baby(self) {
            return self.mob_interact_animal(player, hand);
        }
        if self.is_tamed() && player.is_secondary_use_active() {
            // TODO: Open the horse inventory screen once mount menus exist.
            return InteractionResult::Success;
        }

        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        if !item_stack.is_empty() {
            let result = self.interact_living_entity_with_equippable(player, hand);
            if result.consumes_action() {
                return result;
            }
            if let Some(me) = self.as_living_entity() {
                let item = item_stack.item();
                let result = {
                    let mut inventory = player.inventory.lock();
                    let held = inventory.get_item_in_hand_mut(hand);
                    ITEM_BEHAVIORS
                        .get_behavior(item)
                        .interact_living_entity(held, player, me, hand)
                };
                if result.consumes_action() {
                    return result;
                }
            }

            if self.is_equippable_in_slot(&item_stack, EquipmentSlot::Body)
                && !self.is_wearing_body_armor()
            {
                self.equip_body_armor(player, hand);
                return InteractionResult::Success;
            }
        }

        self.do_player_ride(player);
        InteractionResult::Success
    }

    /// The feeding branch shared by vanilla `Horse`, `AbstractChestedHorse`
    /// and `ZombieHorse.mobInteract`.
    ///
    /// Returns `None` when the caller should fall through to its own items
    /// and then [`Self::horse_mob_interact`].
    fn equine_feed_interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        item_stack: &ItemStack,
    ) -> Option<InteractionResult> {
        if item_stack.is_empty() {
            return None;
        }
        if self.is_food(item_stack) {
            return Some(self.fed_food(player, hand, item_stack));
        }
        if !self.is_tamed() {
            self.make_mad();
            return Some(InteractionResult::Success);
        }
        None
    }

    /// Vanilla `getControllingPassenger`.
    fn horse_controlling_passenger(&self) -> Option<SharedEntity> {
        if self.is_saddled()
            && let Some(passenger) = self.first_passenger()
            && passenger.as_player().is_some()
        {
            return Some(passenger);
        }
        self.controlling_passenger_mob()
    }

    /// Vanilla `tickRidden`.
    fn horse_tick_ridden(&self, controller: &Player, ridden_input: DVec3) {
        let (yaw, pitch) = controller.rotation();
        self.set_rotation((yaw, pitch * 0.5));
        self.base().set_old_yaw_to_current();
        let yaw = self.rotation().0;
        self.set_y_body_rot(yaw);
        self.set_y_head_rot(yaw);
        if self.can_simulate_movement() {
            if ridden_input.z <= 0.0 {
                self.horse_base()
                    .gallop_sound_counter
                    .store(0, Ordering::Relaxed);
            }

            if self.on_ground() {
                let pending = *self.horse_base().player_jump_pending_scale.lock();
                if pending > 0.0 && !self.is_jumping() {
                    self.execute_riders_jump(pending, ridden_input);
                }
                *self.horse_base().player_jump_pending_scale.lock() = 0.0;
            }
        }
    }

    /// Vanilla `getRiddenInput`.
    fn horse_ridden_input(&self, controller: &Player) -> DVec3 {
        if self.on_ground()
            && *self.horse_base().player_jump_pending_scale.lock() == 0.0
            && self.is_standing()
            && !self
                .horse_base()
                .allow_stand_sliding
                .load(Ordering::Relaxed)
        {
            return DVec3::ZERO;
        }
        let input = controller.last_client_input();
        let sideways = input.left_intent() * 0.5;
        let mut forward = input.forward_intent();
        if forward <= 0.0 {
            forward *= 0.25;
        }
        DVec3::new(f64::from(sideways), 0.0, f64::from(forward))
    }

    /// Vanilla `getRiddenSpeed`.
    fn horse_ridden_speed(&self) -> f32 {
        self.attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED) as f32
    }

    /// Vanilla `executeRidersJump`.
    fn execute_riders_jump(&self, amount: f32, input: DVec3) {
        let impulse = f64::from(self.get_jump_power_with_multiplier(amount));
        let movement = self.velocity();
        self.set_velocity(DVec3::new(movement.x, impulse, movement.z));
        self.mark_hurt();
        if input.z > 0.0 {
            let yaw = self.rotation().0 * (PI / 180.0);
            let (sin, cos) = yaw.sin_cos();
            self.set_velocity(
                self.velocity()
                    + DVec3::new(
                        f64::from(-0.4 * sin * amount),
                        0.0,
                        f64::from(0.4 * cos * amount),
                    ),
            );
        }
    }

    /// Vanilla `onPlayerJump`.
    fn on_player_jump(&self, jump_amount: i32) {
        if !self.is_saddled() {
            return;
        }
        let jump_amount = if jump_amount < 0 {
            0
        } else {
            self.horse_base()
                .allow_stand_sliding
                .store(true, Ordering::Relaxed);
            self.stand_if_possible();
            jump_amount
        };
        *self.horse_base().player_jump_pending_scale.lock() =
            player_jump_pending_scale(jump_amount);
    }

    /// Vanilla `canMate` for `Horse` and `Donkey`, which cross-breed.
    fn can_mate_horse_or_donkey(&self, partner: &dyn Animal) -> bool {
        if partner.id() == self.id() {
            return false;
        }
        let partner_type = partner.entity_type();
        if partner_type != &vanilla_entities::DONKEY && partner_type != &vanilla_entities::HORSE {
            return false;
        }
        self.can_parent()
            && partner
                .as_abstract_horse()
                .is_some_and(<dyn AbstractHorse>::can_parent)
    }

    /// Vanilla `canParent`.
    fn can_parent(&self) -> bool {
        !self.is_vehicle()
            && !self.is_passenger()
            && self.is_tamed()
            && !AgeableMob::is_baby(self)
            && self.get_health() >= self.get_max_health()
            && self.is_in_love()
    }

    /// Vanilla `setOffspringAttributes`.
    fn set_offspring_attributes(&self, partner: &dyn AgeableMob, baby: &dyn AgeableMob) {
        self.set_offspring_attribute(
            partner,
            baby,
            vanilla_attributes::MAX_HEALTH,
            min_health(),
            max_health(),
        );
        self.set_offspring_attribute(
            partner,
            baby,
            vanilla_attributes::JUMP_STRENGTH,
            min_jump_strength(),
            max_jump_strength(),
        );
        self.set_offspring_attribute(
            partner,
            baby,
            vanilla_attributes::MOVEMENT_SPEED,
            min_movement_speed(),
            max_movement_speed(),
        );
    }

    /// Vanilla `setOffspringAttribute`.
    fn set_offspring_attribute(
        &self,
        partner: &dyn AgeableMob,
        baby: &dyn AgeableMob,
        attribute: AttributeRef,
        range_min: f64,
        range_max: f64,
    ) {
        let own = self.attributes().lock().get_base_value(attribute);
        let other = partner.attributes().lock().get_base_value(attribute);
        let (Some(own), Some(other)) = (own, other) else {
            return;
        };
        if let Some(value) = create_offspring_attribute(own, other, range_min, range_max) {
            baby.attributes().lock().set_base_value(attribute, value);
        }
    }

    /// Vanilla `randomizeAttributes`.
    fn randomize_attributes(&self) {}

    /// Vanilla `AbstractHorse.finalizeSpawn` ahead of `AgeableMob.finalizeSpawn`.
    fn finalize_horse_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = group_data.or(Some(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_baby_spawn_chance(HORSE_BABY_SPAWN_CHANCE),
        )));
        self.randomize_attributes();
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }

    /// Vanilla `AbstractHorse.addAdditionalSaveData`, after the animal layers.
    fn save_horse(&self, nbt: &mut NbtCompound) {
        nbt.insert("EatingHaystack", i8::from(self.is_eating()));
        nbt.insert("Bred", i8::from(self.is_bred()));
        nbt.insert("Temper", self.temper());
        nbt.insert("Tame", i8::from(self.is_tamed()));
        if let Some(owner) = self.horse_base().owner.lock().as_ref() {
            owner.store(nbt, "Owner");
        }
    }

    /// Vanilla `AbstractHorse.readAdditionalSaveData`, after the animal layers.
    fn load_horse(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_eating(nbt.byte("EatingHaystack").is_some_and(|value| value != 0));
        self.set_bred(nbt.byte("Bred").is_some_and(|value| value != 0));
        self.set_temper(nbt.int("Temper").unwrap_or(0));
        self.set_tamed(nbt.byte("Tame").is_some_and(|value| value != 0));
        // Vanilla's old-owner conversion also accepts a UUID string.
        let owner = EntityReference::read(&nbt, "Owner").or_else(|| {
            nbt.string("Owner")
                .and_then(|owner| Uuid::parse_str(owner.to_str().as_ref()).ok())
                .map(EntityReference::from_uuid)
        });
        *self.horse_base().owner.lock() = owner;
    }

    /// Registers vanilla `AbstractHorse.registerGoals`.
    fn register_horse_goals(&self, mob_base: &MobBase, behaviour_goals: impl FnOnce(&MobBase))
    where
        Self: Sized,
    {
        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, goals::RunAroundLikeCrazyGoal::new(1.2));
            goal_selector.add_goal(2, BreedGoal::new(1.0));
            goal_selector.add_goal(4, FollowParentGoal::new(1.0));
            goal_selector.add_goal(6, WaterAvoidingRandomStrollGoal::new(0.7));
            goal_selector.add_goal(7, LookAtPlayerGoal::new(6.0));
            goal_selector.add_goal(8, RandomLookAroundGoal::new());
            if self.can_perform_rearing() {
                goal_selector.add_goal(
                    9,
                    goals::RandomStandGoal::new(self.ambient_stand_interval()),
                );
            }
        }
        behaviour_goals(mob_base);
    }
}

/// Vanilla `AbstractHorse.addBehaviourGoals`.
pub(super) fn add_default_behaviour_goals(mob_base: &MobBase) {
    let mut goal_selector = mob_base.goal_selector().lock();
    goal_selector.add_goal(0, FloatGoal::new(mob_base));
    goal_selector.add_goal(1, goals::MountPanicGoal::new(1.2));
    goal_selector.add_goal(
        3,
        TemptGoal::new(
            1.25,
            |item_stack| {
                REGISTRY
                    .items
                    .is_in_tag(item_stack.item(), &ItemTag::HORSE_TEMPT_ITEMS)
            },
            false,
        ),
    );
}

/// Vanilla `PlayerRideableJumping.getPlayerJumpPendingScale`.
fn player_jump_pending_scale(jump_amount: i32) -> f32 {
    if jump_amount >= 90 {
        1.0
    } else {
        0.4 + 0.4 * jump_amount as f32 / 90.0
    }
}

/// Vanilla `PlayerRideableJumping`: a vehicle whose rider charges jumps.
pub trait PlayerRideableJumping: Send + Sync {
    /// Vanilla `canJump`.
    fn can_jump(&self) -> bool;

    /// Vanilla `handleStartJump`.
    fn handle_start_jump(&self, jump_scale: i32);

    /// Vanilla `handleStopJump`.
    fn handle_stop_jump(&self);
}

/// Creates a breeding offspring of `entity_type` at `parent`'s position.
pub(super) fn create_offspring(
    entity_type: EntityTypeRef,
    parent: &dyn Entity,
    world: &Arc<World>,
) -> Option<SharedEntity> {
    ENTITIES.create(
        entity_type,
        next_entity_id(),
        parent.position(),
        Arc::downgrade(world),
    )
}

/// Per-entity behaviour that [`impl_equine_entity`] wires into the shared
/// `Entity`, `LivingEntity`, `AgeableMob`, `Animal` and `Mob` impls.
trait EquineHooks {
    /// Dimensions before the baby scale is applied.
    fn baby_dimensions(&self) -> EntityDimensions;

    fn ambient_sound_for(&self) -> Option<SoundEventRef>;

    fn hurt_sound_for(&self) -> Option<SoundEventRef>;

    fn death_sound_for(&self) -> Option<SoundEventRef>;

    fn mob_interact_for(&self, player: &Player, hand: InteractionHand) -> InteractionResult;

    /// Saves the fields only this entity type has.
    fn save_extra(&self, _nbt: &mut NbtCompound) {}

    /// Loads the fields only this entity type has.
    fn load_extra(&self, _nbt: BorrowedNbtCompoundView<'_, '_>) {}

    /// Runs after the shared horse `aiStep`.
    fn ai_step_extra(&self) {}

    /// Runs at the start of every tick with this entity's shared handle.
    fn tick_extra(&self, _entity: &SharedEntity) {}

    /// Drops what this entity type carries beyond the horse inventory.
    fn drop_equipment_extra(&self) {}

    fn can_use_slot_for(&self, _slot: EquipmentSlot) -> bool {
        true
    }

    fn can_mate_for(&self, partner: &dyn Animal) -> bool;

    fn can_fall_in_love_for(&self) -> bool;

    fn can_age_up_for(&self) -> bool;

    fn is_food_for(&self, item_stack: &ItemStack) -> bool;

    fn remove_when_far_away_for(&self, dist_sqr: f64) -> bool;

    fn breed_offspring_for(
        &self,
        world: &Arc<World>,
        partner: &dyn AgeableMob,
    ) -> Option<SharedEntity>;

    fn finalize_spawn_for(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData>;
}

/// Implements the trait glue shared by every equine entity.
///
/// The entity type implements [`EquineHooks`] for its own behaviour.
/// `$layer` and `$layer_mut` name the `AbstractHorseEntityData` accessors.
/// Extra trait items go in the trailing `entity`, `living` and `mob` blocks.
macro_rules! impl_equine_entity {
    (
        $entity:ty, $layer:ident, $layer_mut:ident,
        entity { $($entity_items:item)* }
        living { $($living_items:item)* }
        mob { $($mob_items:item)* }
    ) => {
        impl $entity {
            fn update_dirty_mob_effect_entity_data(&self) {
                if let Some(display) =
                    $crate::entity::sync_dirty_mob_effects(&self.living_base, &self.entity_data)
                {
                    $crate::entity::EntitySyncedData::set_base_glowing_flag(
                        &self.entity_data,
                        self.has_glowing_tag() || display.glowing,
                    );
                }
            }
        }

        impl $crate::entity::DefaultEntityPush for $entity {}

        // A per-type impl, since `try_as_dyn` cannot see blanket impls.
        impl $crate::entity::entities::mobs::passive::equine::PlayerRideableJumping for $entity {
            fn can_jump(&self) -> bool {
                self.is_saddled()
            }

            fn handle_start_jump(&self, _jump_scale: i32) {
                self.horse_base()
                    .allow_stand_sliding
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                self.stand_if_possible();
                self.play_jump_sound();
            }

            fn handle_stop_jump(&self) {}
        }

        impl $crate::entity::Entity for $entity {
            fn as_default_entity_push(&self) -> Option<&dyn $crate::entity::DefaultEntityPush> {
                Some(self)
            }

            fn is_hard_collision_relevant(&self) -> bool {
                false
            }

            fn base(&self) -> &$crate::entity::EntityBase {
                &self.base
            }

            fn entity_type(&self) -> steel_registry::entity_type::EntityTypeRef {
                self.entity_type
            }

            fn tick(self: std::sync::Arc<Self>) {
                let entity = $crate::entity::EntityEventSource::into_shared_entity(std::sync::Arc::clone(&self));
                self.tick_extra(&entity);
                $crate::entity::LivingEntity::tick_living_entity(&*self, &entity);
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_tick(&*self);
            }

            fn base_tick(&self) {
                $crate::entity::Mob::base_tick_mob(self);
            }

            fn dimensions_for_pose(
                &self,
                _pose: $crate::entity::EntityPose,
            ) -> steel_registry::entity_type::EntityDimensions {
                let scale = $crate::entity::LivingEntity::get_scale(self);
                if $crate::entity::AgeableMob::is_baby(self) {
                    self.baby_dimensions().scale(scale)
                } else if self.entity_type.fixed {
                    self.entity_type.dimensions
                } else {
                    self.entity_type.dimensions.scale(scale)
                }
            }

            fn synced_data(&self) -> Option<&dyn $crate::entity::EntitySyncedData> {
                Some(&self.entity_data)
            }

            fn update_data_before_sync(&self) {
                self.update_dirty_mob_effect_entity_data();
            }

            fn max_up_step(&self) -> f32 {
                self.attributes()
                    .lock()
                    .get_value(steel_registry::vanilla_attributes::STEP_HEIGHT)
                    .unwrap_or(steel_registry::vanilla_attributes::STEP_HEIGHT.default_value)
                    as f32
            }

            fn sound_source(&self) -> steel_protocol::packets::game::SoundSource {
                steel_protocol::packets::game::SoundSource::Neutral
            }

            fn play_step_sound(&self, pos: steel_utils::BlockPos, block_state: steel_utils::BlockStateId) {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_play_step_sound(
                    self, pos, block_state,
                );
            }

            fn is_pushable(&self) -> bool {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::is_horse_pushable(self)
            }

            fn on_climbable(&self) -> bool {
                false
            }

            fn controlling_passenger(&self) -> Option<$crate::entity::SharedEntity> {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_controlling_passenger(self)
            }

            fn passenger_attachment_point(&self, passenger: &dyn $crate::entity::Entity) -> glam::DVec3 {
                let dimensions = self.base().dimensions();
                let passenger_index = self.passenger_index(passenger).unwrap_or_default();
                let point = dimensions.attachments.get_clamped(
                    steel_registry::entity_type::EntityAttachment::Passenger,
                    passenger_index,
                    self.rotation().0,
                    dimensions,
                );
                point
                    + $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_passenger_offset(
                        self,
                        $crate::entity::LivingEntity::get_scale(self),
                    )
            }

            fn cause_fall_damage(
                self: std::sync::Arc<Self>,
                fall_distance: f64,
                damage_modifier: f32,
                source: &$crate::entity::damage::DamageSource,
            ) -> bool {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_cause_fall_damage(
                    &*self,
                    fall_distance,
                    damage_modifier,
                    source,
                )
            }

            fn save_additional(&self, nbt: &mut simdnbt::owned::NbtCompound) {
                self.save_mob(nbt);
                self.save_ageable_mob(nbt);
                self.save_animal(nbt);
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::save_horse(self, nbt);
                self.save_extra(nbt);
            }

            fn load_additional(&self, nbt: simdnbt::borrow::NbtCompound<'_, '_>) {
                self.load_mob(nbt);
                self.load_ageable_mob(nbt);
                self.load_animal(nbt);
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::load_horse(self, nbt);
                self.load_extra(nbt);
            }

            $($entity_items)*
        }

        impl $crate::entity::LivingEntity for $entity {
            fn living_base(&self) -> &$crate::entity::LivingEntityBase {
                &self.living_base
            }

            fn get_health(&self) -> f32 {
                *self.entity_data.lock().living_entity().health.get()
            }

            fn set_health(&self, health: f32) {
                let clamped = health.clamp(0.0, self.get_max_health());
                self.entity_data.lock().living_entity_mut().health.set(clamped);
            }

            fn sound_volume(&self) -> f32 {
                $crate::entity::entities::mobs::passive::equine::SOUND_VOLUME
            }

            fn hurt_sound(
                &self,
                _source: &$crate::entity::damage::DamageSource,
            ) -> Option<steel_registry::sound_event::SoundEventRef> {
                self.hurt_sound_for()
            }

            fn death_sound(&self) -> Option<steel_registry::sound_event::SoundEventRef> {
                self.death_sound_for()
            }

            fn can_use_slot(&self, slot: steel_registry::equipment::EquipmentSlot) -> bool {
                self.can_use_slot_for(slot)
            }

            fn can_dispenser_equip_into_slot(&self, slot: steel_registry::equipment::EquipmentSlot) -> bool {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::can_dispenser_equip_horse_slot(self, slot)
            }

            fn equip_sound(
                &self,
                slot: steel_registry::equipment::EquipmentSlot,
                stack: &steel_registry::item_stack::ItemStack,
            ) -> Option<steel_registry::sound_event::SoundEventRef> {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_equip_sound(self, slot, stack)
            }

            fn is_immobile(&self) -> bool {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::is_horse_immobile(self)
            }

            fn hurt_server(
                &self,
                world: &$crate::world::World,
                source: &$crate::entity::damage::DamageSource,
                amount: f32,
            ) -> bool {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_hurt_server(
                    self, world, source, amount,
                )
            }

            fn drop_equipment(&self) {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::drop_horse_inventory(self);
                self.drop_equipment_extra();
            }

            fn server_ai_step(&self, entity: &$crate::entity::SharedEntity) {
                $crate::entity::Mob::mob_server_ai_step(self, entity);
            }

            fn tick_ridden(&self, controller: &$crate::player::Player, ridden_input: glam::DVec3) {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_tick_ridden(
                    self,
                    controller,
                    ridden_input,
                );
            }

            fn ridden_input(&self, controller: &$crate::player::Player, _self_input: glam::DVec3) -> glam::DVec3 {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_ridden_input(self, controller)
            }

            fn ridden_speed(&self, _controller: &$crate::player::Player) -> f32 {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_ridden_speed(self)
            }

            fn ai_step(&self, entity: &$crate::entity::SharedEntity) -> Option<$crate::physics::MoveResult> {
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_ai_step_before(self);
                let result = $crate::entity::Mob::mob_ai_step(self, entity);
                $crate::entity::AgeableMob::tick_ageable_mob(self);
                $crate::entity::Animal::tick_animal_love(self);
                $crate::entity::entities::mobs::passive::equine::AbstractHorse::horse_ai_step_after(self);
                self.ai_step_extra();
                result
            }

            $($living_items)*
        }

        impl $crate::entity::AgeableMob for $entity {
            fn ageable_base(&self) -> &$crate::entity::AgeableMobBase {
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

            fn can_age_up(&self) -> bool {
                self.can_age_up_for()
            }

            fn get_breed_offspring(
                &self,
                world: &std::sync::Arc<$crate::world::World>,
                partner: &dyn $crate::entity::AgeableMob,
            ) -> Option<$crate::entity::SharedEntity> {
                self.breed_offspring_for(world, partner)
            }
        }

        impl $crate::entity::Animal for $entity {
            fn animal_base(&self) -> &$crate::entity::AnimalBase {
                &self.animal_base
            }

            fn is_food(&self, item_stack: &steel_registry::item_stack::ItemStack) -> bool {
                self.is_food_for(item_stack)
            }

            fn can_fall_in_love(&self) -> bool {
                self.can_fall_in_love_for()
            }

            fn can_mate(&self, partner: &dyn $crate::entity::Animal) -> bool {
                self.can_mate_for(partner)
            }
        }

        impl $crate::entity::Mob for $entity {
            fn mob_base(&self) -> &$crate::entity::MobBase {
                &self.mob_base
            }

            fn tick_goal_selectors(&self, entity: &$crate::entity::SharedEntity) {
                $crate::entity::PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
            }

            fn tick_path_navigation(&self) {
                $crate::entity::PathfinderMob::tick_pathfinder_path_navigation(self);
            }

            fn custom_server_ai_step(&self) {
                $crate::entity::Animal::custom_server_ai_step_animal(self);
            }

            fn ambient_sound(&self) -> Option<steel_registry::sound_event::SoundEventRef> {
                self.ambient_sound_for()
            }

            fn ambient_sound_interval(&self) -> i32 {
                $crate::entity::entities::mobs::passive::equine::AMBIENT_SOUND_INTERVAL
            }

            fn max_spawn_cluster_size(&self) -> i32 {
                $crate::entity::entities::mobs::passive::equine::MAX_SPAWN_CLUSTER_SIZE
            }

            fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
                self.remove_when_far_away_for(dist_sqr)
            }

            fn finalize_spawn(
                &self,
                world: &std::sync::Arc<$crate::world::World>,
                spawn_reason: $crate::entity::EntitySpawnReason,
                group_data: Option<$crate::entity::SpawnGroupData>,
            ) -> Option<$crate::entity::SpawnGroupData> {
                self.finalize_spawn_for(world, spawn_reason, group_data)
            }

            fn mob_interact(
                &self,
                player: &$crate::player::Player,
                hand: steel_utils::types::InteractionHand,
            ) -> $crate::behavior::InteractionResult {
                self.mob_interact_for(player, hand)
            }

            fn mob_flags(&self) -> i8 {
                *self.entity_data.lock().mob().mob_flags.get()
            }

            fn set_mob_flags(&self, flags: i8) {
                self.entity_data.lock().mob_mut().mob_flags.set(flags);
            }

            $($mob_items)*
        }

        impl $crate::entity::PathfinderMob for $entity {}
    };
}

pub(super) use impl_equine_entity;
