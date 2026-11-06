use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_utils::types::InteractionHand;

use super::selector::{Goal, GoalControls};
use crate::entity::PathfinderMob;
use crate::inventory::prelude::EquipmentSlot;

type UseItemSelector = Box<dyn Fn(&dyn PathfinderMob) -> bool + Send + Sync>;

/// Vanilla `UseItemGoal`: puts an item in the main hand and uses it while the
/// selector allows it.
pub struct UseItemGoal {
    item: ItemStack,
    finish_using_sound: Option<SoundEventRef>,
    can_use_selector: UseItemSelector,
}

impl UseItemGoal {
    #[must_use]
    pub(crate) fn new(
        item: ItemStack,
        finish_using_sound: Option<SoundEventRef>,
        can_use_selector: impl Fn(&dyn PathfinderMob) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            item,
            finish_using_sound,
            can_use_selector: Box::new(can_use_selector),
        }
    }
}

impl Goal for UseItemGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        (self.can_use_selector)(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.is_using_item()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.set_item_slot(
            EquipmentSlot::MainHand,
            self.item.copy_with_count(self.item.count()),
        );
        mob.start_using_held_item(InteractionHand::MainHand);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        mob.set_item_slot(EquipmentSlot::MainHand, ItemStack::empty());
        if let Some(sound) = self.finish_using_sound {
            mob.play_sound(sound, 1.0, rand::random::<f32>() * 0.2 + 0.9);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;

    use glam::DVec3;
    use steel_registry::{init_vanilla_registry, sound_events, vanilla_entities, vanilla_items};

    use super::*;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::WanderingTraderEntity;
    use crate::entity::{LivingEntity, Mob};

    #[test]
    fn start_holds_and_uses_the_item_and_stop_clears_the_hand() {
        init_vanilla_registry();
        init_behaviors();
        let trader = WanderingTraderEntity::new(
            &vanilla_entities::WANDERING_TRADER,
            1,
            DVec3::ZERO,
            Weak::new(),
        );
        let mut goal = UseItemGoal::new(
            ItemStack::new(&vanilla_items::MILK_BUCKET),
            Some(&sound_events::ENTITY_WANDERING_TRADER_REAPPEARED),
            |_| true,
        );
        assert!(goal.can_use(&trader));
        goal.start(&trader);
        assert!(
            trader
                .equipment_in_slot(EquipmentSlot::MainHand)
                .is(&vanilla_items::MILK_BUCKET)
        );
        assert!(trader.is_using_item());
        assert!(goal.can_continue_to_use(&trader));

        goal.stop(&trader);
        assert!(trader.equipment_in_slot(EquipmentSlot::MainHand).is_empty());
        trader.stop_using_held_item();
        assert!(!goal.can_continue_to_use(&trader));
    }
}
