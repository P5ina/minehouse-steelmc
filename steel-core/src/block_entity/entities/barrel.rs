//! Barrel block entity implementation.
//!
//! Barrels are container block entities with 27 slots (3x9 grid),
//! functioning similarly to chests but without double-chest behavior.

use std::{
    mem,
    sync::{Arc, Weak},
};

use glam::DVec3;
use simdnbt::ToNbtTag;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{sound_events, vanilla_block_entity_types};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};

use crate::block_entity::container_openers::{
    ContainerOpenersCounter, ContainerOpenersHooks, container_sound_pitch,
};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::Entity as _;
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::player::Player;
use crate::world::World;

/// Number of slots in a barrel (3 rows of 9).
pub const BARREL_SLOTS: usize = 27;

/// Barrel block entity.
///
/// A simple container with 27 slots, using the same menu as chests.
pub struct BarrelBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<BarrelContainer>>,
    container_ref: ContainerRef,
    /// Viewers, for the `open` state and the sounds.
    openers: ContainerOpenersCounter,
}

struct BarrelContainer {
    items: Vec<ItemStack>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `BarrelBlockEntity`.
unsafe impl DowncastType for BarrelBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/barrel");
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a barrel block entity.
unsafe impl DowncastType for BarrelContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/barrel");
}

impl BarrelBlockEntity {
    /// Creates a new barrel block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::BARREL,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(BarrelContainer {
            items: vec![ItemStack::empty(); BARREL_SLOTS],
        }));
        let shared_container: SharedContainer =
            Arc::<SyncMutex<BarrelContainer>>::clone(&container);
        Self {
            container_ref: ContainerRef::owned_by_block_entity(shared_container, Arc::clone(&base)),
            base,
            container,
            openers: ContainerOpenersCounter::new(),
        }
    }

    /// The number of players viewing this barrel.
    #[must_use]
    pub fn opener_count(&self) -> i32 {
        self.openers.opener_count()
    }

    /// Vanilla `BarrelBlockEntity.playSound`: half a block out of the lid.
    fn play_sound(world: &World, pos: BlockPos, state: BlockStateId, sound: SoundEventRef) {
        let (step_x, step_y, step_z) = state.get_value(&BlockStateProperties::FACING).offset();
        world.play_sound_at(
            sound,
            SoundSource::Blocks,
            DVec3::new(
                f64::from(pos.x()) + 0.5 + f64::from(step_x) / 2.0,
                f64::from(pos.y()) + 0.5 + f64::from(step_y) / 2.0,
                f64::from(pos.z()) + 0.5 + f64::from(step_z) / 2.0,
            ),
            0.5,
            container_sound_pitch(),
            None,
        );
    }

    /// Vanilla `updateBlockState`: `setBlockAndUpdate` with the given state's
    /// `open` value.
    fn update_block_state(world: &Arc<World>, pos: BlockPos, state: BlockStateId, open: bool) {
        world.set_block(
            pos,
            state.set_value(&BlockStateProperties::OPEN, open),
            UpdateFlags::UPDATE_ALL,
        );
    }
}

impl ContainerOpenersHooks for BarrelBlockEntity {
    fn on_open(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        Self::play_sound(world, pos, state, &sound_events::BLOCK_BARREL_OPEN);
        Self::update_block_state(world, pos, state, true);
    }

    fn on_close(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        Self::play_sound(world, pos, state, &sound_events::BLOCK_BARREL_CLOSE);
        Self::update_block_state(world, pos, state, false);
    }

    fn opener_count_changed(
        &self,
        _world: &Arc<World>,
        _pos: BlockPos,
        _state: BlockStateId,
        _previous: i32,
        _current: i32,
    ) {
    }

    /// The player's chest menu shows this barrel.
    fn is_own_container(&self, player: &Player) -> bool {
        player.has_chest_menu_open_over(self.container_ref.container_id())
    }
}

impl BlockEntity for BarrelBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        let items = {
            let mut container = self.container.lock();
            mem::replace(&mut container.items, vec![ItemStack::empty(); BARREL_SLOTS])
        };
        let Some(world) = self.get_level() else {
            return;
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        // Convert to NbtCompound view for accessing methods
        let nbt_view: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());

        // Load items from NBT using borrowed NBT for proper ItemStack parsing
        if let Some(items_list) = nbt_view.list("Items")
            && let Some(compounds) = items_list.compounds()
        {
            for compound in compounds {
                // Each item has a "Slot" byte and item data
                if let Some(slot) = compound.byte("Slot") {
                    let slot = slot as usize;
                    if slot < BARREL_SLOTS {
                        // Parse item directly from the borrowed compound
                        if let Some(item) = ItemStack::from_borrowed_compound(&compound) {
                            container.items[slot] = item;
                        }
                    }
                }
            }
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        // Save items to NBT (only non-empty slots)
        let container = self.container.lock();
        let mut items: Vec<NbtCompound> = Vec::new();
        for (slot, item) in container.items.iter().enumerate() {
            if !item.is_empty() {
                // Use ItemStack's ToNbtTag implementation for proper component serialization
                if let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() {
                    item_nbt.insert("Slot", slot as i8);
                    items.push(item_nbt);
                }
            }
        }
        nbt.insert("Items", NbtList::Compound(items));
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        // Barrels don't need to send inventory to clients on chunk load
        // (unlike signs which display text)
        None
    }

    fn start_open(&self, player: &Player) {
        if self.base.is_removed() || player.is_spectator() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers.increment_openers(
            self,
            player,
            &world,
            self.base.pos(),
            self.base.block_state(),
            player.block_interaction_range(),
        );
    }

    fn stop_open(&self, player: &Player) {
        if self.base.is_removed() || player.is_spectator() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers.decrement_openers(
            self,
            player,
            &world,
            self.base.pos(),
            self.base.block_state(),
        );
    }

    fn recheck_open(&self) {
        if self.base.is_removed() {
            return;
        }
        let Some(world) = self.get_level() else {
            return;
        };
        self.openers
            .recheck_openers(self, &world, self.base.pos(), self.base.block_state());
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

impl Container for BarrelContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        BARREL_SLOTS
    }

    fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        if slot < BARREL_SLOTS {
            let max_stack_size = self.get_max_stack_size_for_item(&stack);
            if !stack.is_empty() && stack.count() > max_stack_size {
                stack.set_count(max_stack_size);
            }
            self.items[slot] = stack;
        }
    }

    fn get_max_stack_size(&self) -> i32 {
        64
    }

    fn set_changed(&mut self) {}
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_items};

    use super::*;

    fn test_barrel() -> BarrelBlockEntity {
        init_vanilla_registry();
        BarrelBlockEntity::new(
            Weak::new(),
            BlockPos::new(1, 2, 3),
            vanilla_blocks::BARREL.default_state(),
        )
    }

    #[test]
    fn set_item_limits_stack_to_vanilla_container_maximum() {
        let barrel = test_barrel();
        barrel
            .container
            .lock()
            .set_item(0, ItemStack::with_count(&vanilla_items::STONE, 100));

        assert_eq!(barrel.container.lock().get_item(0).count(), 64);
    }

    #[test]
    fn pre_remove_preserves_slots_for_existing_menu_references() {
        let barrel = test_barrel();
        barrel
            .container
            .lock()
            .set_item(0, ItemStack::new(&vanilla_items::STONE));

        barrel.pre_remove_side_effects(
            BlockPos::new(1, 2, 3),
            vanilla_blocks::BARREL.default_state(),
        );

        let container = barrel.container.lock();
        assert_eq!(container.items.len(), BARREL_SLOTS);
        assert!(container.items.iter().all(ItemStack::is_empty));
    }
}
