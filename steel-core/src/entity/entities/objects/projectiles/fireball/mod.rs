//! Vanilla `Fireball` projectiles: the blaze's small fireball and the ghast's
//! explosive large fireball.

mod large_fireball;
mod small_fireball;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::item_stack::ItemStack;
use steel_registry::{vanilla_damage_types, vanilla_items};

pub use large_fireball::LargeFireballEntity;
pub use small_fireball::SmallFireballEntity;

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::{AbstractHurtingProjectile, SharedEntity};
use crate::world::World;

/// Behavior of vanilla `Fireball`: a hurting projectile showing an item (a fire
/// charge by default).
pub trait Fireball: AbstractHurtingProjectile {
    /// Returns the synced item stack.
    fn get_item(&self) -> ItemStack;

    /// Writes the synced item stack.
    fn set_synced_item(&self, item: ItemStack);

    /// Vanilla `setItem`: an empty stack falls back to a fire charge.
    fn set_item(&self, item: &ItemStack) {
        if item.is_empty() {
            self.set_synced_item(default_fireball_item());
        } else {
            self.set_synced_item(item.copy_with_count(1));
        }
    }

    /// Saves `Item`.
    fn save_fireball(&self, nbt: &mut NbtCompound) {
        self.save_projectile(nbt);
        self.save_hurting_projectile(nbt);
        let item = self.get_item();
        if !item.is_empty() {
            nbt.insert("Item", item.to_nbt_tag());
        }
    }

    /// Loads `Item`.
    fn load_fireball(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        self.load_hurting_projectile(nbt);
        let item = nbt
            .compound("Item")
            .and_then(|tag| ItemStack::from_borrowed_compound(&tag))
            .unwrap_or_else(default_fireball_item);
        self.set_item(&item);
    }
}

fn default_fireball_item() -> ItemStack {
    ItemStack::new(&vanilla_items::FIRE_CHARGE)
}

/// Vanilla `DamageSources.fireball`: credited to the shooter, or to the
/// fireball itself when nobody shot it.
fn fireball_damage_source(fireball: SharedEntity, owner: Option<SharedEntity>) -> DamageSource {
    match owner {
        Some(owner) => DamageSource::environment(&vanilla_damage_types::FIREBALL)
            .with_direct_entity(fireball)
            .with_causing_entity(owner),
        None => DamageSource::environment(&vanilla_damage_types::UNATTRIBUTED_FIREBALL)
            .with_direct_entity(Arc::clone(&fireball))
            .with_causing_entity(fireball),
    }
}

/// Vanilla `EnchantmentHelper.doPostAttackEffects` for a projectile hit: the
/// victim's armor reacts; fireballs carry no weapon.
fn fireball_post_attack_effects(world: &World, victim: &SharedEntity, source: &DamageSource) {
    let context = EnchantmentPostAttackContext::new(victim, source);
    enchantment_helper::do_post_attack_effects_with_item_source(
        world,
        victim.as_ref(),
        &ItemStack::empty(),
        &context,
    );
}
