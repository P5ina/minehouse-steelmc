use std::borrow::Cow;
use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::properties::Direction;

use steel_macros::item_behavior;
use steel_registry::data_components::PotionContents;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_potions;
use text_components::TextComponent;

use crate::behavior::ItemBehavior;
use crate::behavior::projectile_item::ProjectileItem;
use crate::entity::SharedEntity;
use crate::world::World;

use super::arrow::arrow_as_projectile;

use super::dynamic_name::potion_name;

/// Tipped-arrow behavior providing Vanilla's potion-content-dependent name and
/// poison default instance.
#[item_behavior]
pub struct TippedArrowItem;

impl ItemBehavior for TippedArrowItem {
    fn as_projectile_item(&self) -> Option<&dyn ProjectileItem> {
        Some(self)
    }

    fn get_name<'a>(&self, stack: &'a ItemStack) -> Cow<'a, TextComponent> {
        potion_name(stack)
    }

    fn get_default_instance(&self, item: ItemRef) -> ItemStack {
        PotionContents::create_item_stack(item, &vanilla_potions::POISON)
    }
}

impl ProjectileItem for TippedArrowItem {
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
