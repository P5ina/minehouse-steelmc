//! The inventory shared by chest and hopper minecarts (vanilla
//! `AbstractMinecartContainer` and `ContainerEntity`).

use std::mem;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_game_events;
use steel_registry::vanilla_game_rules::ENTITY_DROPS;
use steel_utils::locks::{IntoShared as _, Shared};
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey, Identifier};

use crate::behavior::blocks::anger_nearby_piglins;
use crate::block_entity::container_loot::ContainerLoot;
use crate::entity::damage::DamageSource;
use crate::entity::{Entity, WeakEntity};
use crate::inventory::container::{Container, SimpleContainer};
use crate::inventory::lock::{ContainerId, ContainerRef};
use crate::inventory::menu::kinds::ChestKind;
use crate::inventory::menu::{MenuBehavior, MenuKind};
use crate::player::Player;
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `ContainerEntity.isChestVehicleStillValid` reach buffer.
const CONTAINER_REACH_BUFFER: f64 = 4.0;

/// A minecart's item slots with the loot table they may still have to roll.
pub(super) struct MinecartContainer {
    container: Shared<SimpleContainer>,
    loot: ContainerLoot,
}

impl MinecartContainer {
    pub(super) fn new(slots: usize) -> Self {
        Self {
            container: SimpleContainer::new(slots).into_shared(),
            loot: ContainerLoot::new(),
        }
    }

    /// Vanilla `setLootTable(table, seed)`.
    pub(super) fn set_loot_table(&self, loot_table: Identifier, seed: i64) {
        self.loot.set(loot_table, seed);
    }

    /// The slots without rolling a pending loot table.
    pub(super) const fn raw(&self) -> &Shared<SimpleContainer> {
        &self.container
    }

    /// The slots, rolling a pending loot table first.
    pub(super) fn container_ref(&self, cart: &dyn Entity) -> ContainerRef {
        self.unpack(cart, None);
        ContainerRef::from(&self.container)
    }

    /// Vanilla `ContainerEntity.unpackChestVehicleLootTable`.
    pub(super) fn unpack(&self, cart: &dyn Entity, player: Option<&Player>) {
        if !self.loot.is_pending() {
            return;
        }
        let pos = block_pos_of(cart);
        let mut container = self.container.lock();
        // Vanilla rolls with the cart's exact position as origin; the loot
        // origin only feeds location conditions, which use its block.
        self.loot.unpack(&mut *container, pos, player);
    }

    /// Vanilla `AbstractContainerMenu.getRedstoneSignalFromContainer` over
    /// these slots, rolling a pending loot table first.
    pub(super) fn redstone_signal(&self, cart: &dyn Entity) -> i32 {
        self.unpack(cart, None);
        redstone_signal_from_container(&*self.container.lock())
    }

    /// Vanilla `Containers.dropContents(level, entity, container)`.
    pub(super) fn drop_contents(&self, cart: &dyn Entity, world: &Arc<World>) {
        self.unpack(cart, None);
        let items: Vec<ItemStack> = {
            let mut container = self.container.lock();
            container.items_mut().iter_mut().map(mem::take).collect()
        };
        let pos = block_pos_of(cart);
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    /// Vanilla `ContainerEntity.chestVehicleDestroyed`.
    pub(super) fn destroyed(&self, cart: &dyn Entity, world: &World, source: &DamageSource) {
        if !world.get_game_rule(&ENTITY_DROPS) {
            return;
        }
        let Some(world) = cart.level() else {
            return;
        };
        self.drop_contents(cart, &world);
        if let Some(player) = source.direct_entity().and_then(|entity| entity.as_player()) {
            anger_nearby_piglins(&world, player, true);
        }
    }

    /// Vanilla `AbstractMinecartContainer.applyNaturalSlowdown`: a fuller
    /// cart rolls further.
    pub(super) fn apply_natural_slowdown(&self, movement: DVec3, in_water: bool) -> DVec3 {
        let mut keep = 0.98_f32;
        if !self.loot.is_pending() {
            let emptiness = 15 - redstone_signal_from_container(&*self.container.lock());
            #[expect(clippy::cast_precision_loss, reason = "emptiness is between 0 and 15")]
            {
                keep += emptiness as f32 * 0.001;
            }
        }
        if in_water {
            keep *= 0.95;
        }
        DVec3::new(
            movement.x * f64::from(keep),
            0.0,
            movement.z * f64::from(keep),
        )
    }

    /// Vanilla `AbstractMinecartContainer.createMenu(containerId, inventory, player)`:
    /// spectators cannot open an unrolled cart; anyone else rolls it with
    /// their luck. Returns whether the menu may open.
    pub(super) fn prepare_open(&self, cart: &dyn Entity, player: &Player) -> bool {
        if self.loot.is_pending() && player.is_spectator() {
            return false;
        }
        self.unpack(cart, Some(player));
        true
    }

    /// Vanilla `ContainerEntity.addChestVehicleSaveData`.
    pub(super) fn save(&self, nbt: &mut NbtCompound) {
        if self.loot.save(nbt) {
            return;
        }
        let container = self.container.lock();
        let items: Vec<NbtCompound> = container
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| !item.is_empty())
            .filter_map(|(slot, item)| match item.clone().to_nbt_tag() {
                NbtTag::Compound(mut item_nbt) => {
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "minecart container slots fit in a byte"
                    )]
                    item_nbt.insert("Slot", slot as i8);
                    Some(item_nbt)
                }
                _ => None,
            })
            .collect();
        nbt.insert("Items", NbtList::Compound(items));
    }

    /// Vanilla `ContainerEntity.readChestVehicleSaveData`.
    pub(super) fn load(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        let mut container = self.container.lock();
        let size = container.get_container_size();
        for item in container.items_mut() {
            *item = ItemStack::empty();
        }
        if self.loot.load(nbt) {
            return;
        }
        let Some(compounds) = nbt.list("Items").and_then(|list| list.compounds()) else {
            return;
        };
        for compound in compounds {
            let Some(slot) = compound.byte("Slot") else {
                continue;
            };
            let Ok(slot) = usize::try_from(slot) else {
                continue;
            };
            if slot < size
                && let Some(item) = ItemStack::from_borrowed_compound(&compound)
            {
                container.items_mut()[slot] = item;
            }
        }
    }
}

fn block_pos_of(cart: &dyn Entity) -> BlockPos {
    let position = cart.position();
    BlockPos::containing(position.x, position.y, position.z)
}

/// Vanilla `AbstractContainerMenu.getRedstoneSignalFromContainer`.
#[must_use]
pub fn redstone_signal_from_container(container: &dyn Container) -> i32 {
    let mut total = 0.0_f32;
    let size = container.get_container_size();
    for slot in 0..size {
        let item = container.get_item(slot);
        if !item.is_empty() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "stack counts and sizes are small"
            )]
            {
                total += item.count() as f32 / container.get_max_stack_size_for_item(item) as f32;
            }
        }
    }
    #[expect(clippy::cast_precision_loss, reason = "container sizes are small")]
    let total = total / size as f32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla's Mth.lerpDiscrete truncates"
    )]
    let signal = (total * 14.0).floor() as i32 + i32::from(total > 0.0);
    signal
}

/// The menu over a container minecart: valid while the cart is alive and in
/// reach, optionally emitting `container_close` when it closes (vanilla
/// `MinecartChest.stopOpen`).
pub(super) struct ContainerMinecartMenuKind {
    cart: WeakEntity,
    chest: ChestKind,
    emits_close_event: bool,
}

impl ContainerMinecartMenuKind {
    pub(super) fn new(cart: WeakEntity, container: ContainerRef, emits_close_event: bool) -> Self {
        Self {
            cart,
            chest: ChestKind::new(vec![container]),
            emits_close_event,
        }
    }
}

// SAFETY: This key is owned by Steel and uniquely identifies the container minecart menu kind.
unsafe impl DowncastType for ContainerMinecartMenuKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/container_minecart");
}

impl MenuKind for ContainerMinecartMenuKind {
    /// Vanilla `ContainerEntity.isChestVehicleStillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        self.cart.upgrade().is_some_and(|cart| {
            !cart.is_removed()
                && player.is_within_entity_interaction_range_with_buffer(
                    cart.bounding_box(),
                    CONTAINER_REACH_BUFFER,
                )
        })
    }

    fn start_open(&mut self, player: &Player) {
        self.chest.start_open(player);
    }

    fn chest_menu_containers(&self) -> Vec<ContainerId> {
        self.chest.chest_menu_containers()
    }

    fn removed(&mut self, behavior: &mut MenuBehavior, player: &Player) {
        self.chest.removed(behavior, player);
        if !self.emits_close_event {
            return;
        }
        let Some(cart) = self.cart.upgrade() else {
            return;
        };
        let Some(world) = cart.level() else {
            return;
        };
        world.game_event_at(
            &vanilla_game_events::CONTAINER_CLOSE,
            cart.position(),
            &GameEventContext::new(Some(player as &dyn Entity), None),
        );
    }
}
