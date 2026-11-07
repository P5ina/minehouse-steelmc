//! Clientbound packet listing a merchant's trades for an open trading screen.

use std::io::{Result, Write};

use steel_macros::ClientPacket;
use steel_registry::{
    data_components::DataComponentMap, item_stack::ItemStack, packets::play::C_MERCHANT_OFFERS,
};
use steel_utils::{codec::VarInt, serial::WriteTo};

/// Vanilla `ItemCost.STREAM_CODEC`: item id, count and the exact components it requires.
#[derive(Clone, Debug)]
pub struct MerchantItemCost {
    /// Raw item registry id.
    pub item_id: i32,
    /// Required count.
    pub count: i32,
    /// Components the paid stack must match exactly.
    pub components: DataComponentMap,
}

impl WriteTo for MerchantItemCost {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        VarInt(self.item_id).write(writer)?;
        VarInt(self.count).write(writer)?;
        self.components.write_typed_list(writer)
    }
}

/// Vanilla `MerchantOffer.writeToStream`.
#[derive(Clone, Debug)]
pub struct MerchantOfferEntry {
    pub cost_a: MerchantItemCost,
    pub result: ItemStack,
    pub cost_b: Option<MerchantItemCost>,
    pub out_of_stock: bool,
    pub uses: i32,
    pub max_uses: i32,
    pub xp: i32,
    pub special_price_diff: i32,
    pub price_multiplier: f32,
    pub demand: i32,
}

impl WriteTo for MerchantOfferEntry {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        self.cost_a.write(writer)?;
        self.result.write(writer)?;
        self.cost_b.write(writer)?;
        self.out_of_stock.write(writer)?;
        self.uses.write(writer)?;
        self.max_uses.write(writer)?;
        self.xp.write(writer)?;
        self.special_price_diff.write(writer)?;
        self.price_multiplier.write(writer)?;
        self.demand.write(writer)
    }
}

/// Vanilla `ClientboundMerchantOffersPacket`.
#[derive(ClientPacket, Clone, Debug)]
#[packet_id(Play = C_MERCHANT_OFFERS)]
pub struct CMerchantOffers {
    pub container_id: i32,
    pub offers: Vec<MerchantOfferEntry>,
    pub villager_level: i32,
    pub villager_xp: i32,
    pub show_progress: bool,
    pub can_restock: bool,
}

impl WriteTo for CMerchantOffers {
    fn write(&self, writer: &mut impl Write) -> Result<()> {
        VarInt(self.container_id).write(writer)?;
        self.offers.write(writer)?;
        VarInt(self.villager_level).write(writer)?;
        VarInt(self.villager_xp).write(writer)?;
        self.show_progress.write(writer)?;
        self.can_restock.write(writer)
    }
}
