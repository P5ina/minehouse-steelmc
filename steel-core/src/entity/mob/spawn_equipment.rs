//! Vanilla `Mob.populateDefaultEquipmentSlots` and
//! `Mob.populateDefaultEquipmentEnchantments`: the random armor and
//! enchantments a mob may spawn with.

use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_enchantment_tags::EnchantmentTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, vanilla_items};
use steel_utils::random::Random;
use steel_utils::types::Difficulty;

use super::Mob;
use crate::enchantment_helper::select_enchantment;
use crate::inventory::equipment::{EquipmentSlot, EquipmentSlotType};
use crate::world::{DifficultyInstance, World};

/// Vanilla `Mob.EQUIPMENT_POPULATION_ORDER`.
const EQUIPMENT_POPULATION_ORDER: [EquipmentSlot; 4] = [
    EquipmentSlot::Head,
    EquipmentSlot::Chest,
    EquipmentSlot::Legs,
    EquipmentSlot::Feet,
];
const ARMOR_CHANCE: f32 = 0.15;
const ARMOR_UPGRADE_CHANCE: f32 = 0.1087;
const ARMOR_UPGRADE_ROLLS: i32 = 3;
const PARTIAL_ARMOR_STOP_CHANCE_HARD: f32 = 0.1;
const PARTIAL_ARMOR_STOP_CHANCE: f32 = 0.25;
const WEAPON_ENCHANT_CHANCE: f32 = 0.25;
const ARMOR_ENCHANT_CHANCE: f32 = 0.5;
/// Vanilla `VanillaEnchantmentProviders.MOB_SPAWN_EQUIPMENT` cost range.
const MOB_SPAWN_EQUIPMENT_MIN_COST: i32 = 5;
const MOB_SPAWN_EQUIPMENT_MAX_COST_SPAN: f32 = 17.0;

/// Vanilla `Mob.getEquipmentForSlot`: the armor piece of tier `armor_type`
/// (leather, copper, gold, chainmail, iron, diamond) for `slot`.
#[must_use]
pub(crate) fn equipment_for_slot(slot: EquipmentSlot, armor_type: i32) -> Option<ItemRef> {
    let tiers: [ItemRef; 6] = match slot {
        EquipmentSlot::Head => [
            &vanilla_items::LEATHER_HELMET,
            &vanilla_items::COPPER_HELMET,
            &vanilla_items::GOLDEN_HELMET,
            &vanilla_items::CHAINMAIL_HELMET,
            &vanilla_items::IRON_HELMET,
            &vanilla_items::DIAMOND_HELMET,
        ],
        EquipmentSlot::Chest => [
            &vanilla_items::LEATHER_CHESTPLATE,
            &vanilla_items::COPPER_CHESTPLATE,
            &vanilla_items::GOLDEN_CHESTPLATE,
            &vanilla_items::CHAINMAIL_CHESTPLATE,
            &vanilla_items::IRON_CHESTPLATE,
            &vanilla_items::DIAMOND_CHESTPLATE,
        ],
        EquipmentSlot::Legs => [
            &vanilla_items::LEATHER_LEGGINGS,
            &vanilla_items::COPPER_LEGGINGS,
            &vanilla_items::GOLDEN_LEGGINGS,
            &vanilla_items::CHAINMAIL_LEGGINGS,
            &vanilla_items::IRON_LEGGINGS,
            &vanilla_items::DIAMOND_LEGGINGS,
        ],
        EquipmentSlot::Feet => [
            &vanilla_items::LEATHER_BOOTS,
            &vanilla_items::COPPER_BOOTS,
            &vanilla_items::GOLDEN_BOOTS,
            &vanilla_items::CHAINMAIL_BOOTS,
            &vanilla_items::IRON_BOOTS,
            &vanilla_items::DIAMOND_BOOTS,
        ],
        EquipmentSlot::MainHand
        | EquipmentSlot::OffHand
        | EquipmentSlot::Body
        | EquipmentSlot::Saddle => return None,
    };
    usize::try_from(armor_type)
        .ok()
        .and_then(|index| tiers.get(index).copied())
}

/// Vanilla `Mob.populateDefaultEquipmentSlots`: a `0.15 × special multiplier`
/// chance of a random armor tier, filling empty slots from the head down and
/// stopping early at random.
pub(crate) fn populate_default_equipment_slots<M: Mob + ?Sized>(
    mob: &M,
    world: &World,
    random: &mut impl Random,
    difficulty: DifficultyInstance,
) {
    if random.next_f32() >= ARMOR_CHANCE * difficulty.special_multiplier() {
        return;
    }

    let mut armor_type = random.next_i32_bounded(3);
    for _ in 0..ARMOR_UPGRADE_ROLLS {
        if random.next_f32() < ARMOR_UPGRADE_CHANCE {
            armor_type += 1;
        }
    }

    let partial_chance = if world.difficulty() == Difficulty::Hard {
        PARTIAL_ARMOR_STOP_CHANCE_HARD
    } else {
        PARTIAL_ARMOR_STOP_CHANCE
    };
    let mut first = true;
    for slot in EQUIPMENT_POPULATION_ORDER {
        if !first && random.next_f32() < partial_chance {
            break;
        }
        first = false;
        if mob.has_item_in_slot(slot) {
            continue;
        }
        if let Some(item) = equipment_for_slot(slot, armor_type) {
            let _ = mob
                .living_base()
                .equipment()
                .lock()
                .set(slot, ItemStack::new(item));
        }
    }
}

/// Vanilla `Mob.populateDefaultEquipmentEnchantments`: may enchant the main
/// hand item (`0.25 ×`) and each worn armor piece (`0.5 ×` the special multiplier).
pub(crate) fn populate_default_equipment_enchantments<M: Mob + ?Sized>(
    mob: &M,
    random: &mut impl Random,
    difficulty: DifficultyInstance,
) {
    enchant_spawned_equipment(
        mob,
        EquipmentSlot::MainHand,
        random,
        WEAPON_ENCHANT_CHANCE,
        difficulty,
    );
    for slot in EquipmentSlot::ALL {
        if slot.slot_type() == EquipmentSlotType::HumanoidArmor {
            enchant_spawned_equipment(mob, slot, random, ARMOR_ENCHANT_CHANCE, difficulty);
        }
    }
}

/// Vanilla `Mob.enchantSpawnedEquipment`.
fn enchant_spawned_equipment<M: Mob + ?Sized>(
    mob: &M,
    slot: EquipmentSlot,
    random: &mut impl Random,
    chance: f32,
    difficulty: DifficultyInstance,
) {
    let mut equipment = mob.living_base().equipment().lock();
    let item_stack = equipment.get_mut(slot);
    if !item_stack.is_empty() && random.next_f32() < chance * difficulty.special_multiplier() {
        enchant_with_mob_spawn_equipment_provider(item_stack, random, difficulty);
    }
}

/// Vanilla `EnchantmentHelper.enchantItemFromProvider` with the
/// `mob_spawn_equipment` provider (`EnchantmentsByCostWithDifficulty`, cost 5 + up to 17).
fn enchant_with_mob_spawn_equipment_provider(
    item_stack: &mut ItemStack,
    random: &mut impl Random,
    difficulty: DifficultyInstance,
) {
    let max_cost = MOB_SPAWN_EQUIPMENT_MIN_COST
        + (difficulty.special_multiplier() * MOB_SPAWN_EQUIPMENT_MAX_COST_SPAN) as i32;
    let cost = random.next_i32_between(MOB_SPAWN_EQUIPMENT_MIN_COST, max_cost);
    let enchantments = select_enchantment(
        random,
        item_stack,
        cost,
        REGISTRY
            .enchantments
            .iter_tag(&EnchantmentTag::ON_MOB_SPAWN_EQUIPMENT),
    );
    for instance in enchantments {
        item_stack.upgrade_enchantment(instance.enchantment.key.clone(), instance.level);
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::vanilla_items;

    use super::equipment_for_slot;
    use crate::inventory::equipment::EquipmentSlot;

    #[test]
    fn equipment_for_slot_follows_vanilla_tiers() {
        assert_eq!(
            equipment_for_slot(EquipmentSlot::Head, 0).map(|item| item.key.clone()),
            Some(vanilla_items::LEATHER_HELMET.key.clone())
        );
        assert_eq!(
            equipment_for_slot(EquipmentSlot::Chest, 1).map(|item| item.key.clone()),
            Some(vanilla_items::COPPER_CHESTPLATE.key.clone())
        );
        assert_eq!(
            equipment_for_slot(EquipmentSlot::Feet, 5).map(|item| item.key.clone()),
            Some(vanilla_items::DIAMOND_BOOTS.key.clone())
        );
        assert!(equipment_for_slot(EquipmentSlot::Legs, 6).is_none());
        assert!(equipment_for_slot(EquipmentSlot::MainHand, 0).is_none());
    }
}
