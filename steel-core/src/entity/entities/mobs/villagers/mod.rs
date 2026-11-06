//! Villagers (Vanilla `AbstractVillager` and its subclasses).

pub mod trading;
mod villager;
pub(crate) mod villager_data;

pub use villager::VillagerEntity;

use std::sync::Arc;

use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_protocol::packets::game::CTakeItemEntity;
use steel_registry::item_stack::ItemStack;
use steel_utils::ChunkPos;
use steel_utils::locks::SyncMutex;

use crate::entity::entities::ItemEntity;
use crate::entity::{Entity, Mob, RemovalReason};
use crate::inventory::container::SimpleContainer;
use crate::world::World;

/// Vanilla `AbstractVillager.inventory` size.
pub(crate) const INVENTORY_SIZE: usize = 8;
/// Vanilla `InventoryCarrier.TAG_INVENTORY`.
const TAG_INVENTORY: &str = "Inventory";
/// Vanilla `AbstractVillager` path costs next to and inside fire.
pub(crate) const FIRE_IN_NEIGHBOR_MALUS: f32 = 16.0;
pub(crate) const FIRE_MALUS: f32 = -1.0;

/// Vanilla `InventoryCarrier.writeInventoryToTag`.
pub(crate) fn save_inventory(inventory: &SyncMutex<SimpleContainer>, nbt: &mut NbtCompound) {
    let items = inventory
        .lock()
        .store_as_item_list()
        .into_iter()
        .filter_map(|stack| match stack.to_nbt_tag() {
            NbtTag::Compound(compound) => Some(compound),
            _ => None,
        })
        .collect();
    nbt.insert(TAG_INVENTORY, NbtList::Compound(items));
}

/// Vanilla `InventoryCarrier.readInventoryFromTag`.
pub(crate) fn load_inventory(
    inventory: &SyncMutex<SimpleContainer>,
    nbt: BorrowedNbtCompoundView<'_, '_>,
) {
    let Some(compounds) = nbt.list(TAG_INVENTORY).and_then(|list| list.compounds()) else {
        return;
    };
    let items: Vec<ItemStack> = compounds
        .into_iter()
        .filter_map(|compound| ItemStack::from_borrowed_compound(&compound))
        .collect();
    inventory.lock().load_item_list(items);
}

/// Vanilla `InventoryCarrier.pickUpItem`: puts what fits of the item into the
/// inventory instead of equipping it.
pub(crate) fn pick_up_item_into_inventory(
    mob: &dyn Mob,
    inventory: &SyncMutex<SimpleContainer>,
    world: &Arc<World>,
    item_entity: &ItemEntity,
) {
    let item_stack = item_entity.get_item();
    if !mob.wants_to_pick_up(&item_stack) {
        return;
    }
    let remainder = {
        let mut inventory = inventory.lock();
        if !inventory.can_add_item(&item_stack) {
            return;
        }
        inventory.add_item(&item_stack)
    };
    // TODO(advancements): vanilla `onItemPickup` fires
    // THROWN_ITEM_PICKED_UP_BY_ENTITY here, like `Mob::default_pick_up_item`.
    let taken = item_stack.count() - remainder.count();
    world.broadcast_to_nearby(
        ChunkPos::from_entity_pos(item_entity.position()),
        CTakeItemEntity::new(item_entity.id(), mob.id(), taken),
        None,
    );
    if remainder.is_empty() {
        item_entity.set_removed(RemovalReason::Discarded);
    } else {
        item_entity.set_item(remainder);
    }
}
