//! Vanilla `HorseInventoryMenu` and `AbstractMountInventoryMenu`.

use std::sync::{Arc, Weak};

use steel_registry::enchantment_effect::EnchantmentEffectComponent;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, vanilla_entities};
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{AbstractHorse, INVENTORY_ROWS};
use crate::entity::{Entity, SharedEntity, WeakEntity};
use crate::inventory::prelude::*;
use crate::inventory::slots::SlotStorage;
use crate::player::player_inventory::{PlayerInventory, Prediction};

/// Vanilla `AbstractMountInventoryMenu`'s reach: a rider or a player within
/// this many blocks of entity interaction range may keep the menu open.
const MOUNT_MENU_REACH_BUFFER: f64 = 4.0;

/// Vanilla `Mob.createEquipmentSlotContainer`: one equipment slot of a mob
/// exposed as a container.
///
/// Steel containers hand out item references, so this keeps a copy that is
/// written through to the mob on every change and refreshed from it each tick.
pub struct MountEquipmentContainer {
    mount: WeakEntity,
    slot: EquipmentSlot,
    item: [ItemStack; 1],
}

// SAFETY: This key is owned by Steel and uniquely identifies `MountEquipmentContainer`.
unsafe impl DowncastType for MountEquipmentContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/mount_equipment");
}

impl MountEquipmentContainer {
    /// Mirrors `slot` of `mount`.
    #[must_use]
    pub fn new(mount: &SharedEntity, slot: EquipmentSlot) -> Self {
        let item = mount
            .as_living_entity()
            .map_or_else(ItemStack::empty, |living| {
                living
                    .living_base()
                    .equipment()
                    .lock()
                    .get_ref(slot)
                    .clone()
            });
        Self {
            mount: Arc::downgrade(mount),
            slot,
            item: [item],
        }
    }

    /// Re-reads the mob's equipment, picking up changes made outside the menu.
    fn refresh(&mut self) {
        if let Some(mount) = self.mount.upgrade()
            && let Some(living) = mount.as_living_entity()
        {
            self.item[0] = living
                .living_base()
                .equipment()
                .lock()
                .get_ref(self.slot)
                .clone();
        }
    }

    /// Vanilla `setTheItem`: equips the mob and keeps it from despawning.
    fn write_through(&self) {
        let Some(mount) = self.mount.upgrade() else {
            return;
        };
        let Some(mob) = mount.as_mob() else {
            return;
        };
        let item = &self.item[0];
        let previous = {
            let mut equipment = mob.living_base().equipment().lock();
            if equipment.get_ref(self.slot) == item {
                return;
            }
            equipment.set(self.slot, item.clone())
        };
        mob.on_equip_item(self.slot, &previous, item);
        if !item.is_empty() {
            mob.set_guaranteed_drop(self.slot);
            mob.set_persistence_required();
        }
    }
}

impl Container for MountEquipmentContainer {
    fn items(&self) -> &[ItemStack] {
        &self.item
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.item
    }

    fn set_item(&mut self, slot: usize, stack: ItemStack) {
        if let Some(item) = self.item.get_mut(slot) {
            *item = stack;
        }
        self.write_through();
    }

    fn get_max_stack_size(&self) -> i32 {
        1
    }

    fn set_changed(&mut self) {
        self.write_through();
    }
}

/// Vanilla `ArmorSlot` as the horse menu builds it: placement follows the
/// mount's equippable rules, and `isActive` gates the whole slot.
struct MountEquipmentSlot {
    base: NormalSlot,
    mount: WeakEntity,
    slot: EquipmentSlot,
    active: bool,
}

// SAFETY: This key is owned by Steel and uniquely identifies `MountEquipmentSlot`.
unsafe impl DowncastType for MountEquipmentSlot {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:slot/mount_equipment");
}

impl Slot for MountEquipmentSlot {
    fn storage(&self) -> &SlotStorage {
        self.base.storage()
    }

    fn get_item<'a>(&self, guard: &'a ContainerLockGuard) -> &'a ItemStack {
        self.base.get_item(guard)
    }

    fn get_item_mut<'a>(&self, guard: &'a mut ContainerLockGuard) -> &'a mut ItemStack {
        self.base.get_item_mut(guard)
    }

    fn set_item(&self, guard: &mut ContainerLockGuard, stack: ItemStack) {
        self.base.set_item(guard, stack);
    }

    /// Vanilla `ArmorSlot.mayPlace`.
    fn may_place(&self, stack: &ItemStack) -> bool {
        self.active
            && self.mount.upgrade().is_some_and(|mount| {
                mount
                    .as_living_entity()
                    .is_some_and(|living| living.is_equippable_in_slot(stack, self.slot))
            })
    }

    /// Vanilla `ArmorSlot.mayPickup`.
    fn may_pickup(&self, guard: &ContainerLockGuard, player: &Player) -> bool {
        let item = self.get_item(guard);
        self.active
            && (item.is_empty()
                || player.has_infinite_materials()
                || !item.has_enchantment_effect(EnchantmentEffectComponent::PreventArmorChange))
    }

    fn get_max_stack_size(&self, _guard: &ContainerLockGuard) -> i32 {
        1
    }

    fn set_changed(&self, guard: &mut ContainerLockGuard) {
        self.base.set_changed(guard);
    }

    fn get_container_slot(&self) -> usize {
        self.base.get_container_slot()
    }
}

fn equipment_section(
    builder: &mut MenuBuilder,
    mount: &SharedEntity,
    slot: EquipmentSlot,
    active: bool,
) -> (ContainerRef, Section) {
    let container: ContainerRef = MountEquipmentContainer::new(mount, slot)
        .into_shared()
        .into();
    let weak = Arc::downgrade(mount);
    let section = builder.section_with(
        &container,
        1,
        SectionKind::custom(move |container, index| {
            Box::new(MountEquipmentSlot {
                base: NormalSlot::new(container.clone(), index),
                mount: Weak::clone(&weak),
                slot,
                active,
            })
        }),
    );
    (container, section)
}

/// Builds vanilla `HorseInventoryMenu` for `mount`.
///
/// Slot 0 is the saddle, slot 1 the body armor, then the mount's storage in
/// `inventory_columns` columns of three rows, then the player inventory.
#[must_use]
pub fn horse_inventory_menu(
    player_inventory: &Shared<PlayerInventory>,
    container_id: u8,
    mount: &SharedEntity,
    mount_inventory: Shared<SimpleContainer>,
    inventory_columns: usize,
) -> Menu {
    let mut builder = MenuBuilder::new(None, container_id);
    let living = mount.as_living_entity();
    // Vanilla checks `instanceof Llama`, which trader llamas extend.
    let is_llama = mount.entity_type() == &vanilla_entities::LLAMA
        || mount.entity_type() == &vanilla_entities::TRADER_LLAMA;
    let saddle_active = living.is_some_and(|living| living.can_use_slot(EquipmentSlot::Saddle))
        && REGISTRY
            .entity_types
            .is_in_tag(mount.entity_type(), &EntityTypeTag::CAN_EQUIP_SADDLE);
    let body_active = living.is_some_and(|living| living.can_use_slot(EquipmentSlot::Body))
        && (is_llama
            || REGISTRY
                .entity_types
                .is_in_tag(mount.entity_type(), &EntityTypeTag::CAN_WEAR_HORSE_ARMOR));

    let (saddle_container, saddle) =
        equipment_section(&mut builder, mount, EquipmentSlot::Saddle, saddle_active);
    let (body_container, body) =
        equipment_section(&mut builder, mount, EquipmentSlot::Body, body_active);
    let storage_size = inventory_columns * INVENTORY_ROWS;
    let storage = builder.section(&mount_inventory, storage_size);
    let player = builder.player_inventory(player_inventory);

    builder.build(MountMenuKind {
        mount: Arc::downgrade(mount),
        mount_inventory,
        equipment: [saddle_container, body_container],
        saddle,
        body,
        storage,
        player,
    })
}

/// Vanilla `AbstractMountInventoryMenu` behavior.
struct MountMenuKind {
    mount: WeakEntity,
    mount_inventory: Shared<SimpleContainer>,
    equipment: [ContainerRef; 2],
    saddle: Section,
    body: Section,
    storage: Section,
    player: PlayerInventorySections,
}

// SAFETY: This key is owned by Steel and uniquely identifies the mount menu kind.
unsafe impl DowncastType for MountMenuKind {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:menu/mount_inventory");
}

impl MountMenuKind {
    fn equipment_slot_accepts(
        behavior: &MenuBehavior,
        guard: &ContainerLockGuard,
        section: Section,
        stack: &ItemStack,
    ) -> bool {
        let slot = &behavior.slots()[section.start()];
        slot.may_place(stack) && !slot.has_item(guard)
    }
}

impl MenuKind for MountMenuKind {
    /// Vanilla `AbstractMountInventoryMenu.stillValid`, with the equipment
    /// containers' `stillValid`.
    fn still_valid(&self, _behavior: &MenuBehavior, player: &Player) -> bool {
        let Some(mount) = self.mount.upgrade() else {
            return false;
        };
        let inventory_changed = mount.as_abstract_horse().is_none_or(|horse| {
            horse
                .horse_base()
                .has_inventory_changed(&self.mount_inventory)
        });
        let rides_mount = player
            .vehicle()
            .is_some_and(|vehicle| vehicle.id() == mount.id());
        !inventory_changed
            && mount.is_alive()
            && (rides_mount
                || player.is_within_entity_interaction_range_with_buffer(
                    mount.bounding_box(),
                    MOUNT_MENU_REACH_BUFFER,
                ))
    }

    fn on_tick(
        &mut self,
        _behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        _player: &Player,
    ) {
        for container in &self.equipment {
            if let Some(equipment) =
                guard.get_typed_mut::<MountEquipmentContainer>(container.container_id())
            {
                equipment.refresh();
            }
        }
    }

    /// Vanilla `AbstractMountInventoryMenu.quickMoveStack`.
    fn quick_move(
        &mut self,
        behavior: &mut MenuBehavior,
        guard: &mut ContainerLockGuard,
        slot_index: usize,
        player: &Player,
    ) -> Option<ItemStack> {
        if slot_index >= behavior.slots().len() {
            return Some(ItemStack::empty());
        }
        let clicked = behavior.slots()[slot_index].get_item(guard).clone();
        if clicked.is_empty() {
            return Some(ItemStack::empty());
        }
        let mut remaining = clicked.clone();
        let all = self.player.all();

        let from_mount = slot_index < all.start();
        let moved = if from_mount {
            behavior.move_item_stack_to(
                guard,
                slot_index,
                &mut remaining,
                all.start(),
                all.end(),
                FillDirection::Backward,
            )
        } else if Self::equipment_slot_accepts(behavior, guard, self.body, &clicked) {
            behavior.move_item_stack_to(
                guard,
                slot_index,
                &mut remaining,
                self.body.start(),
                self.body.end(),
                FillDirection::Forward,
            )
        } else if Self::equipment_slot_accepts(behavior, guard, self.saddle, &clicked) {
            behavior.move_item_stack_to(
                guard,
                slot_index,
                &mut remaining,
                self.saddle.start(),
                self.saddle.end(),
                FillDirection::Forward,
            )
        } else if !self.storage.is_empty()
            && behavior.move_item_stack_to(
                guard,
                slot_index,
                &mut remaining,
                self.storage.start(),
                self.storage.end(),
                FillDirection::Forward,
            )
        {
            true
        } else {
            // Vanilla swaps between the main inventory and the hotbar here,
            // then ends the quick-move loop whatever happened.
            let (start, end) = if self.player.hotbar().contains(slot_index) {
                (self.player.main().start(), self.player.main().end())
            } else {
                (self.player.hotbar().start(), self.player.hotbar().end())
            };
            if behavior.move_item_stack_to(
                guard,
                slot_index,
                &mut remaining,
                start,
                end,
                FillDirection::Forward,
            ) {
                behavior.update_quick_move_source(guard, slot_index, &remaining, &clicked);
            }
            return Some(ItemStack::empty());
        };

        if !moved {
            return Some(ItemStack::empty());
        }
        behavior.update_quick_move_source(guard, slot_index, &remaining, &clicked);
        if remaining.count() == clicked.count() {
            return Some(ItemStack::empty());
        }
        if let Some(remainder) = behavior.slots()[slot_index].on_take(guard, &remaining, player) {
            player.add_item_or_drop_with_guard(guard, remainder, Prediction::Predicted);
        }
        Some(clicked)
    }
}

/// Vanilla `AbstractHorse.openCustomInventoryScreen`: a tamed mount opens its
/// inventory for a player who rides it or stands by while it is unridden.
pub(super) fn open_horse_inventory<H: AbstractHorse + ?Sized>(horse: &H, player: &Player) {
    if (horse.is_vehicle()
        && !horse
            .passengers()
            .iter()
            .any(|passenger| passenger.id() == player.id()))
        || !horse.is_tamed()
    {
        return;
    }
    let Some(world) = horse.level() else {
        return;
    };
    let Some(mount) = world.get_entity_by_id(horse.id()) else {
        return;
    };
    let columns = horse.inventory_columns();
    let mount_inventory = horse.horse_base().inventory();
    let entity_id = horse.id();
    player.open_mount_menu(
        i32::try_from(columns).unwrap_or(0),
        entity_id,
        move |context| {
            horse_inventory_menu(
                &context.player.inventory,
                context.container_id,
                &mount,
                mount_inventory,
                columns,
            )
        },
    );
}
