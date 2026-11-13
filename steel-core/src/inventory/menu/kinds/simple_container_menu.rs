//! Hopper and dispenser menus: one container section plus the player inventory.

use steel_registry::menu_type::MenuTypeRef;
use steel_registry::vanilla_menu_types;

use super::ChestKind;
use crate::inventory::prelude::*;
use crate::player::player_inventory::PlayerInventory;

/// Builds a hopper menu over its 5 slots.
#[must_use]
pub fn hopper(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: impl Into<ContainerRef>,
) -> Menu {
    simple_container_menu(
        &vanilla_menu_types::HOPPER,
        inventory,
        container_id,
        container,
        5,
    )
}

/// Builds a hopper menu over 5 slots with a custom menu kind, such as a
/// hopper minecart's.
#[must_use]
pub(crate) fn hopper_with_kind(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: impl Into<ContainerRef>,
    kind: impl MenuKind + 'static,
) -> Menu {
    let container = container.into();
    let mut builder = MenuBuilder::new(&vanilla_menu_types::HOPPER, container_id);
    let section = builder.section(&container, 5);
    let player = builder.player_inventory(&inventory);

    builder.route(section, player.all(), FillDirection::Backward);
    builder.route(player.all(), section, FillDirection::Forward);

    builder.build(kind)
}

/// Builds a dispenser or dropper menu over its 9 slots.
#[must_use]
pub fn dispenser(
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: impl Into<ContainerRef>,
) -> Menu {
    simple_container_menu(
        &vanilla_menu_types::GENERIC_3X3,
        inventory,
        container_id,
        container,
        9,
    )
}

fn simple_container_menu(
    menu_type: MenuTypeRef,
    inventory: Shared<PlayerInventory>,
    container_id: u8,
    container: impl Into<ContainerRef>,
    slots: usize,
) -> Menu {
    let container = container.into();
    let mut builder = MenuBuilder::new(menu_type, container_id);
    let section = builder.section(&container, slots);
    let player = builder.player_inventory(&inventory);

    builder.route(section, player.all(), FillDirection::Backward);
    builder.route(player.all(), section, FillDirection::Forward);

    builder.build(ChestKind::new(vec![container]))
}
