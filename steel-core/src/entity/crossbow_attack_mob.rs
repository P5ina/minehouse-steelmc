//! Mobs that fire crossbows (Vanilla `CrossbowAttackMob`).

use steel_registry::vanilla_items;
use steel_utils::types::InteractionHand;

use crate::behavior::items::crossbow::perform_shooting;
use crate::entity::{RangedAttackMob, SharedEntity};
use crate::inventory::equipment::EquipmentSlot;

/// Vanilla `CrossbowAttackMob`.
pub trait CrossbowAttackMob: RangedAttackMob {
    /// Vanilla `setChargingCrossbow` (synced for the charge arm pose).
    fn set_charging_crossbow(&self, is_charging: bool);

    /// Vanilla `onCrossbowAttackPerformed`.
    fn on_crossbow_attack_performed(&self);

    /// Vanilla `CrossbowAttackMob.performCrossbowAttack`: fires the loaded
    /// crossbow at this mob's target.
    fn perform_crossbow_attack(&self, entity: &SharedEntity, crossbow_power: f32) {
        let hand = weapon_holding_hand(self);
        let slot = match hand {
            InteractionHand::MainHand => EquipmentSlot::MainHand,
            InteractionHand::OffHand => EquipmentSlot::OffHand,
        };
        let mut used_item = self.living_base().equipment().lock().get_ref(slot).clone();
        if used_item.is(&vanilla_items::CROSSBOW)
            && let Some(world) = self.level()
        {
            let uncertainty = self.ranged_attack_uncertainty(&world);
            let target = self.target();
            perform_shooting(
                &world,
                entity,
                &mut used_item,
                crossbow_power,
                uncertainty,
                target.as_ref(),
                false,
                self.has_infinite_materials(),
            );
            self.living_base().equipment().lock().set(slot, used_item);
        }
        self.on_crossbow_attack_performed();
    }
}

/// Vanilla `ProjectileUtil.getWeaponHoldingHand(mob, Items.CROSSBOW)`.
#[must_use]
pub(crate) fn weapon_holding_hand<M: RangedAttackMob + ?Sized>(mob: &M) -> InteractionHand {
    let main_hand_is_crossbow = mob
        .living_base()
        .equipment()
        .lock()
        .get_ref(EquipmentSlot::MainHand)
        .is(&vanilla_items::CROSSBOW);
    if main_hand_is_crossbow {
        InteractionHand::MainHand
    } else {
        InteractionHand::OffHand
    }
}
