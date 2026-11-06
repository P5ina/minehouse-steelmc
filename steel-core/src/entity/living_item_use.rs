//! Vanilla `LivingEntity` active item use for non-player living entities
//! (`startUsingItem`, `updatingUsingItem`, `releaseUsingItem`,
//! `stopUsingItem`). Players keep their own inventory-backed version in
//! `player/mod.rs`.

use steel_registry::data_components::vanilla_components::USE_EFFECTS;
use steel_registry::game_events::GameEventRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_game_events;
use steel_utils::types::InteractionHand;

use crate::behavior::ITEM_BEHAVIORS;
use crate::entity::{LivingEntity, LivingEntityRef, SharedEntity};
use crate::inventory::equipment::EquipmentSlot;

/// Vanilla `LivingEntity` flag bit 1: using an item.
pub(crate) const USING_ITEM_FLAG: i8 = 1;
/// Vanilla `LivingEntity` flag bit 2: the active hand is the off hand.
pub(crate) const OFF_HAND_ACTIVE_ITEM_FLAG: i8 = 1 << 1;

const fn hand_slot(hand: InteractionHand) -> EquipmentSlot {
    match hand {
        InteractionHand::MainHand => EquipmentSlot::MainHand,
        InteractionHand::OffHand => EquipmentSlot::OffHand,
    }
}

fn item_in_hand(living: &dyn LivingEntity, hand: InteractionHand) -> ItemStack {
    living
        .living_base()
        .equipment()
        .lock()
        .get_ref(hand_slot(hand))
        .clone()
}

fn set_item_in_hand(living: &dyn LivingEntity, hand: InteractionHand, stack: ItemStack) {
    living
        .living_base()
        .equipment()
        .lock()
        .set(hand_slot(hand), stack);
}

fn set_living_entity_flag(living: &dyn LivingEntity, flag: i8, value: bool) {
    if let Some(data) = living.living_synced_data() {
        data.set_living_entity_flag(flag, value);
    }
}

/// Vanilla `ItemStack.causeUseVibration`.
fn cause_use_vibration(living: &dyn LivingEntity, stack: &ItemStack, event: GameEventRef) {
    if stack
        .get(USE_EFFECTS)
        .is_some_and(|effects| effects.interact_vibrations)
    {
        living.game_event(event);
    }
}

/// Vanilla `LivingEntity.startUsingItem`.
pub(crate) fn start_using_item(living: &dyn LivingEntity, hand: InteractionHand) {
    let stack = item_in_hand(living, hand);
    if stack.is_empty() || living.living_base().is_using_item() {
        return;
    }
    let duration = ITEM_BEHAVIORS
        .get_behavior(stack.item())
        .get_use_duration(&stack, living);
    // VANILLA: a zero-duration use still starts; Steel's state rejects it,
    // and every item a mob uses here has a positive duration.
    if !living
        .living_base()
        .start_using_item(hand, &stack, duration)
    {
        return;
    }
    // TODO: `recentKineticEnemies` for kinetic weapons, once mobs use spears.
    set_living_entity_flag(living, USING_ITEM_FLAG, true);
    set_living_entity_flag(
        living,
        OFF_HAND_ACTIVE_ITEM_FLAG,
        hand == InteractionHand::OffHand,
    );
    cause_use_vibration(living, &stack, &vanilla_game_events::ITEM_INTERACT_START);
}

/// Vanilla `LivingEntity.getTicksUsingItem`.
#[must_use]
pub(crate) fn ticks_using_item(living: &dyn LivingEntity) -> i32 {
    let Some(active) = living.living_base().active_item_use() else {
        return 0;
    };
    let stack = item_in_hand(living, active.hand());
    ITEM_BEHAVIORS
        .get_behavior(active.item())
        .get_use_duration(&stack, living)
        - active.remaining_ticks()
}

/// Vanilla `LivingEntity.getUseItem`: the stack in the active hand, or empty.
#[must_use]
pub(crate) fn use_item(living: &dyn LivingEntity) -> ItemStack {
    living
        .living_base()
        .active_item_use()
        .map_or_else(ItemStack::empty, |active| {
            item_in_hand(living, active.hand())
        })
}

/// Vanilla `LivingEntity.updatingUsingItem` + `updateUsingItem`.
pub(crate) fn updating_using_item(living: &dyn LivingEntity, entity: &SharedEntity) {
    let Some(active) = living.living_base().active_item_use() else {
        return;
    };
    let hand = active.hand();
    let mut stack = item_in_hand(living, hand);
    if !stack.is(active.item()) {
        stop_using_item(living);
        return;
    }
    let Some(user) = LivingEntityRef::new(entity) else {
        return;
    };
    let Some(world) = living.level() else {
        return;
    };
    let behavior = ITEM_BEHAVIORS.get_behavior(active.item());
    behavior.on_use_tick(&world, user, &mut stack, active.remaining_ticks());
    set_item_in_hand(living, hand, stack.clone());
    let Some(active) = living.living_base().decrement_active_item_use() else {
        return;
    };
    if active.remaining_ticks() == 0 && !behavior.use_on_release(&stack) {
        complete_using_item(living, entity, hand, stack);
    }
}

/// Vanilla `LivingEntity.completeUsingItem`.
fn complete_using_item(
    living: &dyn LivingEntity,
    entity: &SharedEntity,
    hand: InteractionHand,
    mut stack: ItemStack,
) {
    if stack != item_in_hand(living, hand) {
        release_using_item(living, entity);
        return;
    }
    let Some(user) = LivingEntityRef::new(entity) else {
        return;
    };
    let Some(world) = living.level() else {
        return;
    };
    if !stack.is_empty() && living.living_base().is_using_item() {
        let behavior = ITEM_BEHAVIORS.get_behavior(stack.item());
        let result = behavior.finish_using(&mut stack, &world, user);
        set_item_in_hand(living, hand, result);
        stop_using_item(living);
    }
}

/// Vanilla `LivingEntity.releaseUsingItem`.
pub(crate) fn release_using_item(living: &dyn LivingEntity, entity: &SharedEntity) {
    if let Some(active) = living.living_base().active_item_use()
        && let Some(world) = living.level()
    {
        let hand = active.hand();
        let mut stack = item_in_hand(living, hand);
        if !stack.is_empty() && stack.is(active.item()) {
            let behavior = ITEM_BEHAVIORS.get_behavior(active.item());
            behavior.release_using(&mut stack, &world, living, active.remaining_ticks());
            let use_on_release = behavior.use_on_release(&stack);
            set_item_in_hand(living, hand, stack);
            if use_on_release {
                updating_using_item(living, entity);
            }
        }
    }
    stop_using_item(living);
}

/// Vanilla `LivingEntity.stopUsingItem`.
pub(crate) fn stop_using_item(living: &dyn LivingEntity) {
    let was_using = living.living_base().active_item_use();
    set_living_entity_flag(living, USING_ITEM_FLAG, false);
    if let Some(active) = was_using {
        let stack = item_in_hand(living, active.hand());
        if stack.is(active.item()) {
            cause_use_vibration(living, &stack, &vanilla_game_events::ITEM_INTERACT_FINISH);
        }
    }
    living.living_base().stop_using_item();
}
