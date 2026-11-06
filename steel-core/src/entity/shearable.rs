//! Mobs that drop loot when sheared (vanilla `Shearable`).

use std::sync::Arc;

use steel_protocol::packets::game::SoundSource;
use steel_registry::item_stack::ItemStack;

use crate::world::World;

/// A mob that shears can be used on, by a player or a dispenser.
pub trait Shearable {
    /// Shears the mob. `source` is the category of the shear sound:
    /// players for a player, blocks for a dispenser.
    fn shear(&self, world: &Arc<World>, source: SoundSource, tool: &ItemStack);

    /// Returns whether the mob can be sheared right now.
    fn ready_for_shearing(&self) -> bool;
}
