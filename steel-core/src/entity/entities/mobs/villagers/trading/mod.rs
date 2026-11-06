//! Vanilla merchant trading: offers (`MerchantOffer`, `MerchantOffers`) and how
//! villagers and wandering traders roll them from trade sets.

mod generation;
mod offer;
#[cfg(test)]
mod tests;

pub use generation::{TradeContext, add_offers_from_trade_set, get_offer, profession_trade_set};
pub use offer::{ItemCost, MerchantOffer, MerchantOffers};
