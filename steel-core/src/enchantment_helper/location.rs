//! Vanilla `minecraft:location_changed` enchantment effects (Frost Walker,
//! Soul Speed): `Enchantment.runLocationChangedEffects` and
//! `Enchantment.stopLocationBasedEffects`, with the per-slot active state
//! vanilla keeps in `LivingEntity.activeLocationDependentEnchantments`.

use std::ptr;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::REGISTRY;
use steel_registry::RegistryExt as _;
use steel_registry::enchantment::EnchantmentRef;
use steel_registry::enchantment_effect::{
    EnchantmentAttributeEffect, EnchantmentEffectRequirements, EnchantmentEntityEffect,
    EnchantmentLocationBasedEffect,
};
use steel_registry::item_stack::ItemStack;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, Identifier};

use super::item_in_use::EnchantedItemInUse;
use super::{apply_entity_effect, entity_requirements_state};
use crate::block_state_provider::BlockStateProviderEvaluator;
use crate::entity::attribute::AttributeModifier;
use crate::entity::{ActiveLocationEnchantment, Entity, LivingEntity};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `EnchantmentHelper.runLocationChangedEffects(ServerLevel, LivingEntity)`.
pub(crate) fn run_location_changed_effects(world: &Arc<World>, entity: &dyn LivingEntity) {
    for slot in EquipmentSlot::ALL {
        run_location_changed_effects_in_slot(world, entity, slot);
    }
}

/// Vanilla `EnchantmentHelper.runLocationChangedEffects(ServerLevel, ItemStack,
/// LivingEntity, EquipmentSlot)` for the item currently in `slot`.
pub(crate) fn run_location_changed_effects_in_slot(
    world: &Arc<World>,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
) {
    for (enchantment, level) in slot_enchantments(entity, slot) {
        run_enchantment_location_changed_effects(world, entity, slot, enchantment, level);
    }
}

/// Vanilla `EnchantmentHelper.stopLocationBasedEffects(LivingEntity)`.
pub(crate) fn stop_location_based_effects(entity: &dyn LivingEntity) {
    for slot in EquipmentSlot::ALL {
        stop_location_based_effects_in_slot(entity, slot);
    }
}

/// Vanilla `EnchantmentHelper.stopLocationBasedEffects(ItemStack, LivingEntity,
/// EquipmentSlot)` for the item that occupied `slot`.
///
/// Vanilla walks the previous item's enchantments; only they can have active
/// entries in that slot, so this deactivates the slot's entries, which also
/// works after the item is gone.
pub(crate) fn stop_location_based_effects_in_slot(entity: &dyn LivingEntity, slot: EquipmentSlot) {
    let stopped: Vec<ActiveLocationEnchantment> = {
        let mut active = entity.living_base().active_location_enchantments().lock();
        let (stopped, kept) = active.drain(..).partition(|entry| entry.slot == slot);
        *active = kept;
        stopped
    };
    for entry in stopped {
        for effect in entry.effects {
            on_deactivated(effect, entity, slot);
        }
    }
}

/// The enchantments of the item in `slot` that apply in that slot, as
/// vanilla's `runIterationOnItem(stack, slot, owner, ...)` visits them.
pub(super) fn slot_enchantments(
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
) -> Vec<(EnchantmentRef, i32)> {
    let mut enchantments = Vec::new();
    entity.with_equipment_slot(slot, &mut |stack: &ItemStack| {
        if stack.is_empty() {
            return;
        }
        let Some(item_enchantments) = stack.get_enchantments() else {
            return;
        };
        for (key, level) in item_enchantments.iter() {
            if let Some(enchantment) = REGISTRY.enchantments.by_key(key)
                && enchantment.matching_slot(slot)
            {
                enchantments.push((enchantment, *level as i32));
            }
        }
    });
    enchantments
}

/// Vanilla `Enchantment.runLocationChangedEffects`. The `matchingSlot` branch
/// is unreachable here because callers already filter by slot.
fn run_enchantment_location_changed_effects(
    world: &Arc<World>,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    enchantment: EnchantmentRef,
    level: i32,
) {
    for conditional in enchantment.effects.location_changed {
        let effect = &conditional.effect;
        let was_active = is_active(entity, slot, enchantment, effect);
        if location_requirements_match(conditional.requirements, entity, level, was_active) {
            if !was_active {
                activate(entity, slot, enchantment, effect);
            }
            on_changed_block(world, effect, entity, slot, level, !was_active);
        } else if deactivate(entity, slot, enchantment, effect) {
            on_deactivated(effect, entity, slot);
        }
    }
}

fn location_requirements_match(
    requirements: Option<&'static EnchantmentEffectRequirements>,
    entity: &dyn LivingEntity,
    level: i32,
    active: bool,
) -> bool {
    requirements.is_none_or(|requirements| {
        entity_requirements_state(requirements, entity, level, Some(active)) == Some(true)
    })
}

fn is_active(
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    enchantment: EnchantmentRef,
    effect: &'static EnchantmentLocationBasedEffect,
) -> bool {
    entity
        .living_base()
        .active_location_enchantments()
        .lock()
        .iter()
        .any(|entry| {
            entry.slot == slot
                && ptr::eq(entry.enchantment, enchantment)
                && entry.effects.iter().any(|active| ptr::eq(*active, effect))
        })
}

fn activate(
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    enchantment: EnchantmentRef,
    effect: &'static EnchantmentLocationBasedEffect,
) {
    let mut active = entity.living_base().active_location_enchantments().lock();
    if let Some(entry) = active
        .iter_mut()
        .find(|entry| entry.slot == slot && ptr::eq(entry.enchantment, enchantment))
    {
        entry.effects.push(effect);
    } else {
        active.push(ActiveLocationEnchantment {
            slot,
            enchantment,
            effects: vec![effect],
        });
    }
}

/// Removes `effect` from the slot's active set; drops the enchantment's entry
/// once it holds no effects, as vanilla does.
fn deactivate(
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    enchantment: EnchantmentRef,
    effect: &'static EnchantmentLocationBasedEffect,
) -> bool {
    let mut active = entity.living_base().active_location_enchantments().lock();
    let Some(index) = active
        .iter()
        .position(|entry| entry.slot == slot && ptr::eq(entry.enchantment, enchantment))
    else {
        return false;
    };
    let entry = &mut active[index];
    let before = entry.effects.len();
    entry.effects.retain(|active| !ptr::eq(*active, effect));
    let removed = entry.effects.len() != before;
    if entry.effects.is_empty() {
        active.remove(index);
    }
    removed
}

/// Vanilla `EnchantmentLocationBasedEffect.onChangedBlock`.
fn on_changed_block(
    world: &Arc<World>,
    effect: &EnchantmentLocationBasedEffect,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    level: i32,
    became_active: bool,
) {
    match effect {
        EnchantmentLocationBasedEffect::AllOf(effects) => {
            for effect in *effects {
                on_changed_block(world, effect, entity, slot, level, became_active);
            }
        }
        EnchantmentLocationBasedEffect::Attribute(attribute) => {
            if became_active {
                let mut attributes = entity.living_base().attributes().lock();
                attributes.remove_modifier(attribute.attribute, &modifier_id(attribute, slot));
                attributes.add_modifier(
                    attribute.attribute,
                    attribute_modifier(attribute, level, slot),
                    false,
                );
            }
        }
        // Vanilla `EnchantmentEntityEffect.onChangedBlock` applies every time.
        EnchantmentLocationBasedEffect::Entity(effect) => {
            apply_equipped_entity_effect(world, effect, entity, slot, level);
        }
    }
}

/// Vanilla `EnchantmentLocationBasedEffect.onDeactivated`; only attribute
/// effects undo anything, and removing a modifier needs only its id.
fn on_deactivated(
    effect: &EnchantmentLocationBasedEffect,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
) {
    match effect {
        EnchantmentLocationBasedEffect::AllOf(effects) => {
            for effect in *effects {
                on_deactivated(effect, entity, slot);
            }
        }
        EnchantmentLocationBasedEffect::Attribute(attribute) => {
            entity
                .living_base()
                .attributes()
                .lock()
                .remove_modifier(attribute.attribute, &modifier_id(attribute, slot));
        }
        EnchantmentLocationBasedEffect::Entity(_) => {}
    }
}

/// Vanilla `EnchantmentAttributeEffect.idForSlot`.
fn modifier_id(effect: &EnchantmentAttributeEffect, slot: EquipmentSlot) -> Identifier {
    Identifier::new(
        effect.id.namespace.clone(),
        format!("{}/{}", effect.id.path, slot.name()),
    )
}

/// Vanilla `EnchantmentAttributeEffect.getModifier`.
fn attribute_modifier(
    effect: &EnchantmentAttributeEffect,
    level: i32,
    slot: EquipmentSlot,
) -> AttributeModifier {
    AttributeModifier {
        id: modifier_id(effect, slot),
        amount: f64::from(effect.amount.calculate(level)),
        operation: effect.operation,
    }
}

/// Applies an entity effect with the enchanted item in `slot` as vanilla's
/// `EnchantedItemInUse(stack, slot, owner)`, whose break callback is
/// `owner.onEquippedItemBroken`.
pub(super) fn apply_equipped_entity_effect(
    world: &Arc<World>,
    effect: &EnchantmentEntityEffect,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    level: i32,
) {
    let mut item_ref = None;
    entity.with_equipment_slot(slot, &mut |stack: &ItemStack| {
        item_ref = Some(stack.item);
    });
    if apply_location_entity_effect(world, effect, entity, slot, level)
        && let Some(item_ref) = item_ref
    {
        entity.on_equipped_item_broken(item_ref, slot);
    }
}

/// Applies an entity effect with the enchanted item in `slot`; returns whether
/// that item broke.
fn apply_location_entity_effect(
    world: &Arc<World>,
    effect: &EnchantmentEntityEffect,
    entity: &dyn LivingEntity,
    slot: EquipmentSlot,
    level: i32,
) -> bool {
    match effect {
        EnchantmentEntityEffect::AllOf(effects) => {
            let mut broke = false;
            for effect in *effects {
                broke |= apply_location_entity_effect(world, effect, entity, slot, level);
            }
            broke
        }
        // Vanilla `ChangeItemDamage.apply`: owners with infinite materials
        // keep their durability.
        EnchantmentEntityEffect::ChangeItemDamage { amount } => {
            let amount = amount.calculate(level) as i32;
            EnchantedItemInUse::Equipped {
                owner: entity,
                slot,
            }
            .hurt_and_break(amount, entity.has_infinite_materials())
        }
        EnchantmentEntityEffect::ReplaceDisk { .. } => {
            replace_disk(world, effect, level, entity, entity.position());
            false
        }
        _ => apply_entity_effect(
            world,
            effect,
            level,
            entity,
            None,
            &mut EnchantedItemInUse::Equipped {
                owner: entity,
                slot,
            },
        ),
    }
}

/// Vanilla `ReplaceDisk.apply`: replaces the blocks of a horizontal disk around
/// `position` that pass the predicate.
pub(super) fn replace_disk(
    world: &Arc<World>,
    effect: &EnchantmentEntityEffect,
    level: i32,
    entity: &dyn Entity,
    position: DVec3,
) {
    let EnchantmentEntityEffect::ReplaceDisk {
        radius,
        height,
        offset,
        predicate,
        block_state,
        trigger_game_event,
    } = effect
    else {
        return;
    };
    let center = BlockPos(BlockPos::containing(position.x, position.y, position.z).0 + *offset);
    let dist = radius.calculate(level) as i32;
    let height = height.calculate(level) as i32;
    let state = BlockStateProviderEvaluator::block_state_from_data(&REGISTRY, block_state);
    let predicate = predicate.map(|predicate| predicate());
    // `BlockPos.betweenClosed` takes the component-wise min and max and walks
    // x fastest, then y, then z.
    let top = (height - 1).min(0);
    let (min_y, max_y) = (center.y() + top.min(0), center.y() + top.max(0));
    let dist_sqr = f64::from(dist * dist);
    for z in center.z() - dist..=center.z() + dist {
        for y in min_y..=max_y {
            for x in center.x() - dist..=center.x() + dist {
                let pos = BlockPos::new(x, y, z);
                let dx = f64::from(x) + 0.5 - position.x;
                let dz = f64::from(z) + 0.5 - position.z;
                if dx * dx + dz * dz >= dist_sqr {
                    continue;
                }
                if let Some(predicate) = predicate
                    && !BlockStateProviderEvaluator::test_block_predicate(
                        world.as_ref(),
                        &REGISTRY,
                        predicate,
                        pos,
                    )
                {
                    continue;
                }
                if world.set_block(pos, state, UpdateFlags::UPDATE_ALL)
                    && let Some(event) = trigger_game_event
                {
                    world.game_event(event, pos, &GameEventContext::new(Some(entity), None));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
