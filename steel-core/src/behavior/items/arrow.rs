//! Arrow item behaviors (`ArrowItem`, `SpectralArrowItem`).
//!
//! Bows and crossbows build their arrows in `projectile_weapon`; these
//! behaviors cover the arrows a dispenser or ominous item spawner fires.

use std::sync::Arc;

use glam::DVec3;
use steel_macros::item_behavior;
use steel_registry::blocks::properties::Direction;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entities;

use crate::behavior::item::ItemBehavior;
use crate::behavior::projectile_item::ProjectileItem;
use crate::entity::entities::{ArrowEntity, SpectralArrowEntity};
use crate::entity::{AbstractArrow as _, ArrowPickup, SharedEntity, next_entity_id};
use crate::world::World;

/// Vanilla `ArrowItem.asProjectile`: a pickable arrow carrying one of `stack`
/// and no weapon.
pub(crate) fn arrow_as_projectile(
    world: &Arc<World>,
    position: DVec3,
    stack: &ItemStack,
) -> SharedEntity {
    let arrow = ArrowEntity::with_items(
        &vanilla_entities::ARROW,
        next_entity_id(),
        position,
        Arc::downgrade(world),
        &stack.copy_with_count(1),
        None,
    );
    arrow.set_pickup(ArrowPickup::Allowed);
    Arc::new(arrow)
}

/// Behavior for the plain arrow item.
#[item_behavior(class = "ArrowItem")]
pub struct ArrowItem;

impl ItemBehavior for ArrowItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }
}

impl ProjectileItem for ArrowItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        _direction: Direction,
    ) -> SharedEntity {
        arrow_as_projectile(world, position, stack)
    }
}

/// Behavior for the spectral arrow item.
#[item_behavior(class = "SpectralArrowItem")]
pub struct SpectralArrowItem;

impl ItemBehavior for SpectralArrowItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }
}

impl ProjectileItem for SpectralArrowItem {
    fn as_projectile(
        &self,
        world: &Arc<World>,
        position: DVec3,
        stack: &ItemStack,
        _direction: Direction,
    ) -> SharedEntity {
        let arrow = SpectralArrowEntity::with_items(
            &vanilla_entities::SPECTRAL_ARROW,
            next_entity_id(),
            position,
            Arc::downgrade(world),
            &stack.copy_with_count(1),
            None,
        );
        arrow.set_pickup(ArrowPickup::Allowed);
        Arc::new(arrow)
    }
}
