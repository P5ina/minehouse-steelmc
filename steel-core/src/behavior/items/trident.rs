//! Trident item behavior (`TridentItem`).

use std::sync::Arc;

use glam::DVec3;
use steel_macros::item_behavior;
use steel_registry::blocks::properties::Direction;
use steel_registry::item_stack::ItemStack;

use crate::behavior::item::ItemBehavior;
use crate::behavior::projectile_item::ProjectileItem;
use crate::entity::entities::ThrownTridentEntity;
use crate::entity::{AbstractArrow as _, ArrowPickup, SharedEntity};
use crate::world::World;

/// Behavior for the trident item.
// TODO: Port the charged throw and riptide of `TridentItem.use` /
// `releaseUsing`.
#[item_behavior(class = "TridentItem")]
pub struct TridentItem;

impl ItemBehavior for TridentItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }
}

impl ProjectileItem for TridentItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        _direction: Direction,
    ) -> SharedEntity {
        let trident = ThrownTridentEntity::at_position(world, position, &stack.copy_with_count(1));
        trident.set_pickup(ArrowPickup::Allowed);
        Arc::new(trident)
    }
}
