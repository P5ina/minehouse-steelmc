mod anvil_block;
mod barrel_block;
mod beacon_block;
mod beehive_block;
mod brewing_stand_block;
mod cartography_table_block;
mod chest_block;
mod chiseled_bookshelf_block;
mod crafting_table_block;
mod decorated_pot_block;
mod dispenser_block;
mod enchanting_table_block;
mod ender_chest_block;
mod furnace_block;
mod grindstone_block;
mod hopper_block;
mod lectern_block;
mod loom_block;
mod shulker_box_block;
mod smithing_table_block;
mod stonecutter_block;

pub use anvil_block::*;
pub use barrel_block::*;
pub use beacon_block::*;
pub use beehive_block::*;
pub use brewing_stand_block::*;
pub use cartography_table_block::*;
pub use chest_block::*;
pub use chiseled_bookshelf_block::*;
pub use crafting_table_block::*;
pub use decorated_pot_block::*;
pub use dispenser_block::*;
pub use enchanting_table_block::*;
pub use ender_chest_block::*;
pub use furnace_block::*;
pub use grindstone_block::*;
pub use hopper_block::*;
pub use lectern_block::*;
pub use loom_block::*;
pub use shulker_box_block::*;
pub use smithing_table_block::*;
pub use stonecutter_block::*;

use crate::player::Player;
use crate::world::World;

/// Vanilla `PiglinAi.angerNearbyPiglins`, called when a player opens a
/// container. `require_line_of_sight` is Vanilla's `onlyIfSeen`.
// TODO: Steel has no piglins yet; anger the piglins near `player` once they exist.
pub const fn anger_nearby_piglins(_world: &World, _player: &Player, _require_line_of_sight: bool) {}
