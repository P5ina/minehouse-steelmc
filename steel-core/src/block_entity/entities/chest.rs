//! Chest block entity implementation.
//!
//! Used by chests, trapped chests and copper chests. Each half of a double chest
//! is its own 27-slot block entity; the halves are combined by the chest block.

use std::{
    mem,
    sync::{Arc, Weak},
};

use glam::DVec3;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, ChestType};
use steel_registry::sound_event::SoundEventRef;

use simdnbt::ToNbtTag;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::block_entity_type::BlockEntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_entity_types;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};
use text_components::TextComponent;

use crate::behavior::blocks::{chest_lid_sounds, connected_direction};
use crate::block_entity::container_loot::{ContainerLoot, LOOT_TABLE_SEED_TAG, LOOT_TABLE_TAG};
use crate::block_entity::container_openers::{
    ContainerOpenersCounter, ContainerOpenersHooks, container_sound_pitch,
};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::Entity as _;
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::player::Player;
use crate::world::World;

/// Number of slots in one chest half (3 rows of 9).
pub const CHEST_SLOTS: usize = 27;

/// Chest block entity.
///
/// Steel does not yet apply container locks: `lock` and `CustomName` are kept
/// as loaded.
pub struct ChestBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<ChestContainer>>,
    container_ref: ContainerRef,
    /// Vanilla `RandomizableContainerBlockEntity` loot table.
    loot: Arc<ContainerLoot>,
    /// Saved fields other than `Items`, written back unchanged.
    preserved: SyncMutex<NbtCompound>,
    /// Viewers, for the lid, the sounds and a trapped chest's power.
    openers: ContainerOpenersCounter,
}

struct ChestContainer {
    items: Vec<ItemStack>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ChestBlockEntity`.
unsafe impl DowncastType for ChestBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/chest");
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a chest block entity.
unsafe impl DowncastType for ChestContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/chest");
}

impl ChestBlockEntity {
    /// Creates a chest block entity of the `chest` type.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self::with_type(&vanilla_block_entity_types::CHEST, level, pos, state)
    }

    /// Creates a chest block entity of the `trapped_chest` type.
    #[must_use]
    pub fn new_trapped(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self::with_type(
            &vanilla_block_entity_types::TRAPPED_CHEST,
            level,
            pos,
            state,
        )
    }

    fn with_type(
        block_entity_type: BlockEntityTypeRef,
        level: Weak<World>,
        pos: BlockPos,
        state: BlockStateId,
    ) -> Self {
        let base = Arc::new(BlockEntityBase::new(block_entity_type, level, pos, state));
        let container = Arc::new(SyncMutex::new(ChestContainer {
            items: vec![ItemStack::empty(); CHEST_SLOTS],
        }));
        let shared_container: SharedContainer = Arc::<SyncMutex<ChestContainer>>::clone(&container);
        let loot = Arc::new(ContainerLoot::new());
        Self {
            container_ref: ContainerRef::owned_by_randomizable_block_entity(
                shared_container,
                Arc::clone(&base),
                Arc::clone(&loot),
            ),
            base,
            container,
            loot,
            preserved: SyncMutex::new(NbtCompound::new()),
            openers: ContainerOpenersCounter::new(),
        }
    }

    /// Returns the saved custom name, if any.
    #[must_use]
    pub fn custom_name(&self) -> Option<TextComponent> {
        let preserved = self.preserved.lock();
        let tag = preserved.get("CustomName")?;
        TextComponent::from_nbt(tag)
    }

    /// The number of players viewing this half. Vanilla `getOpenCount`, a
    /// trapped chest's redstone power.
    #[must_use]
    pub fn opener_count(&self) -> i32 {
        self.openers.opener_count()
    }

    /// Whether a player near the chest still has it open, Vanilla
    /// `!getEntitiesWithContainerOpen().isEmpty()`.
    #[must_use]
    pub fn has_viewers(&self, world: &World) -> bool {
        !self
            .openers
            .players_with_container_open(self, world, self.base.pos())
            .is_empty()
    }

    fn is_trapped(&self) -> bool {
        self.base.block_entity_type() == &vanilla_block_entity_types::TRAPPED_CHEST
    }

    /// Vanilla `ChestBlockEntity.playSound`: a double chest sounds once, from
    /// its `RIGHT` half, halfway towards the `LEFT` one.
    fn play_sound(world: &World, pos: BlockPos, state: BlockStateId, sound: SoundEventRef) {
        let chest_type = state.get_value(&BlockStateProperties::CHEST_TYPE);
        if chest_type == ChestType::Left {
            return;
        }
        let mut x = f64::from(pos.x()) + 0.5;
        let y = f64::from(pos.y()) + 0.5;
        let mut z = f64::from(pos.z()) + 0.5;
        if chest_type == ChestType::Right {
            let (step_x, _, step_z) = connected_direction(state).offset();
            x += f64::from(step_x) * 0.5;
            z += f64::from(step_z) * 0.5;
        }
        world.play_sound_at(
            sound,
            SoundSource::Blocks,
            DVec3::new(x, y, z),
            0.5,
            container_sound_pitch(),
            None,
        );
    }
}

impl ContainerOpenersHooks for ChestBlockEntity {
    /// Only blocks that are a Vanilla `ChestBlock` have a chest sound; copper
    /// chests sound by their oxidation.
    fn on_open(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        if let Some((open, _)) = chest_lid_sounds(state.get_block()) {
            Self::play_sound(world, pos, state, open);
        }
    }

    fn on_close(&self, world: &Arc<World>, pos: BlockPos, state: BlockStateId) {
        if let Some((_, close)) = chest_lid_sounds(state.get_block()) {
            Self::play_sound(world, pos, state, close);
        }
    }

    /// `signalOpenCount`: block event 1 carries the count to clients' lids. A
    /// trapped chest then updates its neighbors and the block below for its
    /// power, Vanilla `TrappedChestBlockEntity.signalOpenCount`.
    fn opener_count_changed(
        &self,
        world: &Arc<World>,
        pos: BlockPos,
        state: BlockStateId,
        previous: i32,
        current: i32,
    ) {
        let block = state.get_block();
        world.block_event(pos, block, EVENT_SET_OPEN_COUNT, current);
        if self.is_trapped() && previous != current {
            world.update_neighbors_at(pos, block);
            world.update_neighbors_at(pos.below(), block);
        }
    }

    /// The player's chest menu shows this half, alone or in a double chest.
    fn is_own_container(&self, player: &Player) -> bool {
        player.has_chest_menu_open_over(self.container_ref.container_id())
    }
}

/// Block event that sets a chest's open count on clients.
const EVENT_SET_OPEN_COUNT: i32 = 1;

impl BlockEntity for ChestBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        // Vanilla `Containers.dropContents` reads every slot, rolling the loot.
        self.container_ref.unpack_loot_table(None);
        let items = {
            let mut container = self.container.lock();
            mem::replace(&mut container.items, vec![ItemStack::empty(); CHEST_SLOTS])
        };
        let Some(world) = self.get_level() else {
            return;
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt_view: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());

        if !self.loot.load(nbt_view)
            && let Some(items_list) = nbt_view.list("Items")
            && let Some(compounds) = items_list.compounds()
        {
            for compound in compounds {
                let Some(slot) = compound.byte("Slot") else {
                    continue;
                };
                let slot = slot as usize;
                if slot < CHEST_SLOTS
                    && let Some(item) = ItemStack::from_borrowed_compound(&compound)
                {
                    container.items[slot] = item;
                }
            }
        }

        let mut preserved = nbt_view.to_owned();
        while preserved.remove("Items").is_some() {}
        while preserved.remove("components").is_some() {}
        while preserved.remove(LOOT_TABLE_TAG).is_some() {}
        while preserved.remove(LOOT_TABLE_SEED_TAG).is_some() {}
        *self.preserved.lock() = preserved;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        for (key, value) in self.preserved.lock().iter() {
            nbt.insert(key.to_string(), value.clone());
        }
        if self.loot.save(nbt) {
            return;
        }
        let container = self.container.lock();
        let mut items: Vec<NbtCompound> = Vec::new();
        for (slot, item) in container.items.iter().enumerate() {
            if item.is_empty() {
                continue;
            }
            if let NbtTag::Compound(mut item_nbt) = item.clone().to_nbt_tag() {
                item_nbt.insert("Slot", slot as i8);
                items.push(item_nbt);
            }
        }
        nbt.insert("Items", NbtList::Compound(items));
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        None
    }

    /// Clients animate the lid from this event; the server only accepts it.
    fn trigger_event(&self, param_a: i32, _param_b: i32) -> bool {
        param_a == EVENT_SET_OPEN_COUNT
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

impl Container for ChestContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        CHEST_SLOTS
    }

    fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        if slot < CHEST_SLOTS {
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
    use steel_registry::{init_vanilla_registry, vanilla_blocks};

    use super::*;

    #[test]
    fn save_keeps_an_unrolled_loot_table() {
        init_vanilla_registry();
        let chest = ChestBlockEntity::new(
            Weak::new(),
            BlockPos::new(0, 64, 0),
            vanilla_blocks::CHEST.default_state(),
        );
        let mut nbt = NbtCompound::new();
        nbt.insert("LootTable", "minecraft:chests/simple_dungeon");
        nbt.insert("LootTableSeed", 42_i64);
        if let Err(error) = chest.load_with_owned_components(&nbt) {
            panic!("test NBT must load: {error}");
        }

        let mut saved = NbtCompound::new();
        chest.save_additional(&mut saved);

        assert_eq!(
            saved.string("LootTable").map(ToString::to_string),
            Some("minecraft:chests/simple_dungeon".to_owned())
        );
        assert_eq!(saved.long("LootTableSeed"), Some(42));
    }
}
