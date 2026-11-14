//! Merchant (trading) menu.
//!
//! Slot layout (39 total), as vanilla `MerchantMenu`:
//! - Slots 0-1: Payments
//! - Slot 2: Result
//! - Slots 3-29: Main inventory (27)
//! - Slots 30-38: Hotbar (9)

use std::sync::Arc;

use steel_registry::{vanilla_custom_stats, vanilla_menu_types};
use steel_utils::locks::SyncMutex;

use crate::entity::SharedEntity;
use crate::entity::entities::mobs::villagers::trading::ItemCost;
use crate::entity::entities::mobs::villagers::{Merchant, send_merchant_offers};
use crate::inventory::container::{ResultContainer, SimpleContainer};
use crate::inventory::prelude::*;
use crate::player::player_inventory::PlayerInventory;

/// Vanilla `MerchantContainer` payment slots.
const PAYMENT_SLOTS: usize = 2;

/// The `MerchantContainer` fields both the menu and its result slot read.
#[derive(Debug, Default)]
struct TradeSelection {
    /// Vanilla `activeOffer`, as an index into the merchant's offers.
    active_offer: Option<usize>,
    /// Vanilla `selectionHint`.
    selection_hint: i32,
}

/// Builds the trading menu for `merchant`. `merchant_level` is the level sent
/// with the offers when the menu opens.
#[must_use]
pub fn merchant_menu(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    merchant: SharedEntity,
    merchant_level: i32,
) -> Menu {
    let payments = SimpleContainer::new(PAYMENT_SLOTS).into_shared();
    let result = ResultContainer::new().into_shared();
    let handler = Arc::new(MerchantResultHandler {
        payments: Arc::clone(&payments),
        result: Arc::clone(&result),
        merchant: Arc::clone(&merchant),
        selection: Arc::new(SyncMutex::new(TradeSelection::default())),
    });

    let mut builder = MenuBuilder::new(&vanilla_menu_types::MERCHANT, container_id);
    let payment = builder.section_all(Arc::clone(&payments));
    let result_section = builder.result_slot(Arc::clone(&handler));
    let player = builder.player_inventory(&inventory);

    builder.route_with_remainder_policy(
        result_section,
        player.all(),
        FillDirection::Backward,
        FakeResultRemainderPolicy::Discard,
    );
    builder.route(payment, player.all(), FillDirection::Forward);
    builder.route(player.main(), player.hotbar(), FillDirection::Forward);
    builder.route(player.hotbar(), player.main(), FillDirection::Forward);
    builder.drain(payment);

    builder.build(MerchantKind {
        handler,
        merchant,
        merchant_level,
        payment,
        player,
    })
}

/// The merchant's result slot: vanilla `MerchantContainer.updateSellItem` and
/// `MerchantResultSlot`.
struct MerchantResultHandler {
    payments: Shared<SimpleContainer>,
    result: Shared<ResultContainer>,
    merchant: SharedEntity,
    selection: Arc<SyncMutex<TradeSelection>>,
}

impl MerchantResultHandler {
    fn merchant(&self) -> Option<&dyn Merchant> {
        self.merchant.as_merchant()
    }

    /// The two payment stacks, cloned.
    fn payments(&self, guard: &ContainerLockGuard) -> Option<[ItemStack; PAYMENT_SLOTS]> {
        let payments = guard.get(ContainerId::from_arc(&self.payments))?;
        Some([payments.get_item(0).clone(), payments.get_item(1).clone()])
    }

    /// Vanilla `MerchantContainer.updateSellItem`.
    fn update_sell_item(&self, guard: &mut ContainerLockGuard) {
        let Some(merchant) = self.merchant() else {
            return;
        };
        let Some([first, second]) = self.payments(guard) else {
            log::warn!("payment container not locked while updating the trade result");
            return;
        };
        let (buy_a, buy_b) = if first.is_empty() {
            (second, ItemStack::empty())
        } else {
            (first, second)
        };

        let result_id = ContainerId::from_arc(&self.result);
        let hint = {
            let mut selection = self.selection.lock();
            selection.active_offer = None;
            selection.selection_hint
        };
        if buy_a.is_empty() {
            guard.set_item(result_id, 0, ItemStack::empty());
            return;
        }

        let mut assembled = None;
        let mut active_offer = None;
        merchant.with_merchant_offers(&mut |offers| {
            if offers.is_empty() {
                return;
            }
            let usable = |index: &usize| {
                offers
                    .get(*index)
                    .is_some_and(|offer| !offer.is_out_of_stock())
            };
            let mut found = offers.recipe_index_for(&buy_a, &buy_b, hint);
            if !found.as_ref().is_some_and(usable) {
                active_offer = found;
                found = offers.recipe_index_for(&buy_b, &buy_a, hint);
            }
            match found
                .filter(usable)
                .and_then(|index| Some((index, offers.get(index)?)))
            {
                Some((index, offer)) => {
                    active_offer = Some(index);
                    assembled = Some(offer.assemble());
                }
                None => assembled = Some(ItemStack::empty()),
            }
        });
        self.selection.lock().active_offer = active_offer;
        if let Some(stack) = assembled {
            guard.set_item(result_id, 0, stack);
        }

        let current = guard
            .get(result_id)
            .map_or_else(ItemStack::empty, |result| result.get_item(0).clone());
        merchant.notify_trade_updated(&current);
    }

    /// Vanilla `MerchantMenu.setSelectionHint`.
    fn set_selection_hint(&self, guard: &mut ContainerLockGuard, hint: i32) {
        self.selection.lock().selection_hint = hint;
        self.update_sell_item(guard);
    }

    /// Vanilla `MerchantContainer.setItem` for a payment slot.
    fn set_payment(&self, guard: &mut ContainerLockGuard, slot: usize, stack: ItemStack) {
        guard.set_item(ContainerId::from_arc(&self.payments), slot, stack);
        self.update_sell_item(guard);
    }
}

impl ResultHandler for MerchantResultHandler {
    fn result_container(&self) -> ContainerRef {
        ContainerRef::from(Arc::clone(&self.result))
    }

    fn dependencies(&self) -> Vec<ContainerRef> {
        vec![ContainerRef::from(Arc::clone(&self.payments))]
    }

    fn update_result(&self, guard: &mut ContainerLockGuard, _player: &Player) {
        self.update_sell_item(guard);
    }

    /// Vanilla `MerchantResultSlot.onTake`.
    fn on_result_taken(
        &self,
        guard: &mut ContainerLockGuard,
        player: &Player,
    ) -> Option<ItemStack> {
        let merchant = self.merchant()?;
        let index = self.selection.lock().active_offer?;
        let [mut buy_a, mut buy_b] = self.payments(guard)?;
        let mut took = false;
        merchant.with_merchant_offers(&mut |offers| {
            took = offers.get(index).is_some_and(|offer| {
                offer.take(&mut buy_a, &mut buy_b) || offer.take(&mut buy_b, &mut buy_a)
            });
        });
        if took {
            merchant.notify_trade(index);
            player.award_custom_stat(&vanilla_custom_stats::TRADED_WITH_VILLAGER);
            self.set_payment(guard, 0, buy_a);
            self.set_payment(guard, 1, buy_b);
        }
        // Vanilla's `overrideXp` here only moves the client-side merchant's xp.
        None
    }

    /// Vanilla `MerchantResultSlot.mayPickup`.
    fn is_result_valid(&self, guard: &ContainerLockGuard, _player: &Player) -> bool {
        let Some(merchant) = self.merchant() else {
            return false;
        };
        let Some(index) = self.selection.lock().active_offer else {
            return false;
        };
        let Some([buy_a, buy_b]) = self.payments(guard) else {
            return false;
        };
        let mut valid = false;
        merchant.with_merchant_offers(&mut |offers| {
            valid = offers.get(index).is_some_and(|offer| {
                offer.satisfied_by(&buy_a, &buy_b) || offer.satisfied_by(&buy_b, &buy_a)
            });
        });
        valid
    }
}

/// Per-menu trading state: the merchant and the payment/result slots.
pub struct MerchantKind {
    handler: Arc<MerchantResultHandler>,
    merchant: SharedEntity,
    merchant_level: i32,
    payment: Section,
    player: PlayerInventorySections,
}

// SAFETY: This Steel-owned key uniquely identifies the concrete menu kind
// within the process.
unsafe impl steel_utils::DowncastType for MerchantKind {
    const TYPE_KEY: steel_utils::DowncastTypeKey =
        steel_utils::DowncastTypeKey::new("steel:menu/merchant");
}

impl MerchantKind {
    /// Vanilla `MerchantMenu.updateSellItem`.
    pub(crate) fn update_sell_item(&self, guard: &mut ContainerLockGuard) {
        self.handler.update_sell_item(guard);
    }

    /// Vanilla `handleSelectTrade`: `setSelectionHint` then `tryMoveItems`.
    pub(crate) fn select_trade(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        index: i32,
    ) {
        self.handler.set_selection_hint(guard, index);
        self.try_move_items(behavior, guard, index);
    }

    /// Vanilla `MerchantMenu.tryMoveItems`: puts the old payment back and fills
    /// the payment slots with the selected offer's costs.
    fn try_move_items(&self, behavior: &MenuBehavior, guard: &mut ContainerLockGuard, index: i32) {
        let Some(merchant) = self.handler.merchant() else {
            return;
        };
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut costs = None;
        merchant.with_merchant_offers(&mut |offers| {
            costs = offers
                .get(index)
                .map(|offer| (offer.item_cost_a().clone(), offer.item_cost_b().cloned()));
        });
        let Some((cost_a, cost_b)) = costs else {
            return;
        };

        let inventory = self.player.all();
        for slot in 0..PAYMENT_SLOTS {
            let Some(payments) = self.handler.payments(guard) else {
                return;
            };
            let mut old_cost = payments[slot].clone();
            if old_cost.is_empty() {
                continue;
            }
            if !behavior.move_item_stack_to(
                guard,
                self.payment.start() + slot,
                &mut old_cost,
                inventory.start(),
                inventory.end(),
                FillDirection::Backward,
            ) {
                return;
            }
            self.handler.set_payment(guard, slot, old_cost);
        }

        if self
            .handler
            .payments(guard)
            .is_some_and(|payments| payments.iter().all(ItemStack::is_empty))
        {
            self.move_from_inventory_to_payment_slot(behavior, guard, 0, &cost_a);
            if let Some(cost_b) = cost_b {
                self.move_from_inventory_to_payment_slot(behavior, guard, 1, &cost_b);
            }
        }
    }

    /// Vanilla `MerchantMenu.moveFromInventoryToPaymentSlot`.
    fn move_from_inventory_to_payment_slot(
        &self,
        behavior: &MenuBehavior,
        guard: &mut ContainerLockGuard,
        payment_slot: usize,
        cost: &ItemCost,
    ) {
        for slot_index in self.player.all().range() {
            let slot = &behavior.slots()[slot_index];
            let inventory_item = slot.get_item(guard).clone();
            if inventory_item.is_empty() || !cost.test(&inventory_item) {
                continue;
            }
            let Some(payments) = self.handler.payments(guard) else {
                return;
            };
            let current = &payments[payment_slot];
            if !current.is_empty()
                && !ItemStack::is_same_item_same_components(&inventory_item, current)
            {
                continue;
            }
            let max_stack_size = inventory_item.max_stack_size();
            let move_count = (max_stack_size - current.count()).min(inventory_item.count());
            let new_payment = inventory_item.copy_with_count(current.count() + move_count);
            slot.get_item_mut(guard).shrink(move_count);
            slot.set_changed(guard);
            let full = new_payment.count() >= max_stack_size;
            self.handler.set_payment(guard, payment_slot, new_payment);
            if full {
                break;
            }
        }
    }
}

impl MenuKind for MerchantKind {
    fn slots_changed(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        self.handler.update_sell_item(guard);
    }

    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.merchant
            .as_merchant()
            .is_some_and(|merchant| merchant.merchant_still_valid(player))
    }

    /// Vanilla `MerchantMenu.removed`: the merchant stops trading. The payments
    /// are drained by [`Menu::removed`].
    fn removed(&mut self, _behavior: &mut MenuBehavior, _player: &Player) {
        self.handler.result.lock().set_item(0, ItemStack::empty());
        if let Some(merchant) = self.merchant.as_merchant() {
            merchant.set_trading_player(None);
        }
    }

    fn can_take_item_for_pick_all(&self, _carried: &ItemStack, _slot_index: usize) -> bool {
        false
    }

    /// Vanilla `Merchant.openTradingScreen`: sends the offers once the screen is open.
    fn on_open(
        &mut self,
        behavior: &mut MenuBehavior,
        _guard: &mut ContainerLockGuard,
        player: &Player,
    ) {
        let Some(merchant) = self.merchant.as_merchant() else {
            return;
        };
        let mut has_offers = false;
        merchant.with_merchant_offers(&mut |offers| has_offers = !offers.is_empty());
        if has_offers {
            send_merchant_offers(
                player,
                i32::from(behavior.container_id()),
                merchant,
                self.merchant_level,
            );
        }
    }
}

#[cfg(test)]
mod tests;
