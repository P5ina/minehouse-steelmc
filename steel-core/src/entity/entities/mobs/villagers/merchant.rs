//! Vanilla `Merchant`, with the trading parts `AbstractVillager` shares between
//! villagers and wandering traders.

use std::sync::Arc;

use glam::DVec3;

use steel_protocol::packets::game::{CMerchantOffers, MerchantItemCost, MerchantOfferEntry};
use steel_registry::RegistryEntry as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entities;
use text_components::TextComponent;
use uuid::Uuid;

use super::trading::{ItemCost, MerchantOffer, MerchantOffers};
use crate::entity::entities::ExperienceOrbEntity;
use crate::entity::{Entity as _, LivingEntity, Mob, SharedEntity, next_entity_id};
use crate::inventory::menu::kinds::merchant_menu;
use crate::player::Player;
use crate::world::World;

/// Vanilla `AbstractVillager.stillValid` reach, in blocks.
const TRADING_RANGE: f64 = 4.0;

/// Vanilla `Merchant`: something a player can trade with.
pub trait Merchant: Mob {
    /// Vanilla `getTradingPlayer`, by UUID.
    fn trading_player(&self) -> Option<Uuid>;

    /// Vanilla `setTradingPlayer`.
    fn set_trading_player(&self, player: Option<Uuid>);

    /// Vanilla `getOffers`: runs `f` on the offers, rolling them first if needed.
    fn with_merchant_offers(&self, f: &mut dyn FnMut(&mut MerchantOffers));

    /// Vanilla `notifyTrade`: the offer at `offer` was just traded.
    fn notify_trade(&self, offer: usize);

    /// Vanilla `notifyTradeUpdated`: the trade result changed to `result`.
    fn notify_trade_updated(&self, result: &ItemStack);

    /// Vanilla `getVillagerXp`.
    fn merchant_xp(&self) -> i32;

    /// Vanilla `showProgressBar`.
    fn show_progress_bar(&self) -> bool {
        true
    }

    /// Vanilla `canRestock`.
    fn can_restock(&self) -> bool {
        false
    }

    /// Vanilla `AbstractVillager.isTrading`.
    fn is_trading(&self) -> bool {
        self.trading_player().is_some()
    }

    /// Vanilla `AbstractVillager.stillValid`.
    fn merchant_still_valid(&self, player: &Player) -> bool {
        self.trading_player() == Some(player.uuid())
            && LivingEntity::is_alive(self)
            && player.is_within_entity_interaction_range(self.bounding_box(), TRADING_RANGE)
    }
}

/// The online player trading with `merchant`, if any.
pub(crate) fn trading_player_of(merchant: &dyn Merchant) -> Option<Arc<Player>> {
    let uuid = merchant.trading_player()?;
    merchant.level()?.players.get_by_uuid(&uuid)
}

/// Vanilla `Merchant.openTradingScreen`: opens the menu; the offers follow it
/// once it is open.
pub(crate) fn open_trading_screen(
    merchant: SharedEntity,
    player: &Player,
    title: TextComponent,
    level: i32,
) {
    let inventory = Arc::clone(&player.inventory);
    player.open_menu(title, move |context| {
        merchant_menu(inventory, context.container_id, merchant, level)
    });
}

/// Vanilla `ServerPlayer.sendMerchantOffers` with the merchant's current offers.
pub(crate) fn send_merchant_offers(
    player: &Player,
    container_id: i32,
    merchant: &dyn Merchant,
    level: i32,
) {
    let mut offers = Vec::new();
    merchant.with_merchant_offers(&mut |list| {
        offers = list.iter().map(offer_entry).collect();
    });
    player.send_packet(CMerchantOffers {
        container_id,
        offers,
        villager_level: level,
        villager_xp: merchant.merchant_xp(),
        show_progress: merchant.show_progress_bar(),
        can_restock: merchant.can_restock(),
    });
}

fn item_cost_entry(cost: &ItemCost) -> MerchantItemCost {
    MerchantItemCost {
        item_id: cost.item().id() as i32,
        count: cost.count(),
        components: cost.components().clone(),
    }
}

/// Vanilla `MerchantOffer.writeToStream`.
fn offer_entry(offer: &MerchantOffer) -> MerchantOfferEntry {
    MerchantOfferEntry {
        cost_a: item_cost_entry(offer.item_cost_a()),
        result: offer.result().clone(),
        cost_b: offer.item_cost_b().map(item_cost_entry),
        out_of_stock: offer.is_out_of_stock(),
        uses: offer.uses(),
        max_uses: offer.max_uses(),
        xp: offer.xp(),
        special_price_diff: offer.special_price_diff(),
        price_multiplier: offer.price_multiplier(),
        demand: offer.demand(),
    }
}

/// The first half of vanilla `AbstractVillager.notifyTrade`: uses the offer up
/// and holds back the ambient sound. Returns the offer's xp and whether it
/// rewards experience, for `rewardTradeXp`.
pub(crate) fn record_trade(
    mob: &dyn Mob,
    offers: &mut MerchantOffers,
    offer: usize,
) -> Option<(i32, bool)> {
    let offer = offers.get_mut(offer)?;
    offer.increase_uses();
    mob.mob_base()
        .set_ambient_sound_time(-mob.ambient_sound_interval());
    Some((offer.xp(), offer.should_reward_exp()))
}

/// Vanilla `AbstractVillager.notifyTradeUpdated`: says yes or no to the
/// current result, at most once a second.
pub(crate) fn play_trade_updated_sound(
    mob: &dyn Mob,
    result: &ItemStack,
    yes: SoundEventRef,
    no: SoundEventRef,
) {
    let interval = mob.ambient_sound_interval();
    if mob.mob_base().ambient_sound_time() > -interval + 20 {
        mob.mob_base().set_ambient_sound_time(-interval);
        mob.make_sound(Some(if result.is_empty() { no } else { yes }));
    }
}

/// The orb `rewardTradeXp` adds: one orb worth `xp`, half a block above `position`.
pub(crate) fn spawn_trade_experience(world: &Arc<World>, position: DVec3, xp: i32) {
    let orb: SharedEntity = Arc::new(ExperienceOrbEntity::with_value(
        &vanilla_entities::EXPERIENCE_ORB,
        next_entity_id(),
        position + DVec3::new(0.0, 0.5, 0.0),
        xp,
        Arc::downgrade(world),
    ));
    if let Err(error) = world.try_add_entity(orb) {
        log::debug!("failed to add trade experience orb: {error}");
    }
}
