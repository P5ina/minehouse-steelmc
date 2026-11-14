//! Vanilla menu kind implementations.

mod anvil_menu;
mod basic_menu;
mod beacon_menu;
mod brewing_stand_menu;
mod chest_menu;
mod crafting_menu;
mod enchantment_menu;
mod ender_chest_menu;
mod furnace_menu;
mod grindstone_menu;
mod inventory_menu;
mod merchant_menu;
mod shulker_box_menu;
mod simple_container_menu;
mod smithing_menu;
mod stonecutter_menu;

pub use anvil_menu::{AnvilKind, anvil};
pub use basic_menu::BasicKind;
pub use beacon_menu::{BeaconKind, beacon};
pub use brewing_stand_menu::{BrewingStandKind, brewing_stand};
pub(crate) use chest_menu::chest_with_kind;
pub use chest_menu::{ChestKind, chest, chest_halves};
pub use crafting_menu::{CraftingKind, crafting};
pub use enchantment_menu::{EnchantmentKind, enchantment};
pub use ender_chest_menu::{EnderChestKind, ender_chest};
pub use furnace_menu::{FurnaceMenuKind, furnace_menu};
pub use grindstone_menu::{GrindstoneKind, grindstone};
pub use inventory_menu::{INVENTORY_MENU_CONTAINER_ID, InventoryKind, inventory_menu};
pub use merchant_menu::{MerchantKind, merchant_menu};
pub use shulker_box_menu::{ShulkerBoxKind, shulker_box};
pub(crate) use simple_container_menu::hopper_with_kind;
pub use simple_container_menu::{dispenser, hopper};
pub use smithing_menu::{SmithingKind, smithing};
pub use stonecutter_menu::{StonecutterKind, stonecutter};
