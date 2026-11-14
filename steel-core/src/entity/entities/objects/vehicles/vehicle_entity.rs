//! Damage and breaking shared by minecarts and boats (vanilla `VehicleEntity`).

use steel_registry::data_components::vanilla_components::CUSTOM_NAME;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::VehicleEntityData;
use steel_registry::vanilla_game_events;
use steel_registry::vanilla_game_rules::{ENTITY_DROPS, MOB_GRIEFING};

use crate::entity::damage::DamageSource;
use crate::entity::{Entity, RemovalReason};
use crate::world::{Explosion, World};

/// Vanilla `VehicleEntity` hurt-wobble duration in ticks.
pub(super) const HURT_TIME: i32 = 10;
/// Accumulated damage above which a vehicle breaks.
const BREAK_DAMAGE: f32 = 40.0;

/// Behavior shared by every vehicle that players break by hitting it.
pub trait VehicleEntity: Entity {
    /// Runs `f` on the vehicle's synchronized `VehicleEntity` data layer.
    fn with_vehicle_data(&self, f: &mut dyn FnMut(&mut VehicleEntityData));

    /// The item dropped when this vehicle breaks (vanilla `getDropItem`).
    fn drop_item(&self) -> ItemRef;

    /// Ticks left on the hurt wobble.
    fn hurt_time(&self) -> i32 {
        let mut value = 0;
        self.with_vehicle_data(&mut |data| value = *data.id_hurt.get());
        value
    }

    /// Sets the hurt wobble.
    fn set_hurt_time(&self, hurt_time: i32) {
        self.with_vehicle_data(&mut |data| data.id_hurt.set(hurt_time));
    }

    /// Direction of the hurt wobble, `1` or `-1`.
    fn hurt_dir(&self) -> i32 {
        let mut value = 0;
        self.with_vehicle_data(&mut |data| value = *data.id_hurtdir.get());
        value
    }

    /// Sets the direction of the hurt wobble.
    fn set_hurt_dir(&self, hurt_dir: i32) {
        self.with_vehicle_data(&mut |data| data.id_hurtdir.set(hurt_dir));
    }

    /// Accumulated damage; it decays by one each tick.
    fn vehicle_damage(&self) -> f32 {
        let mut value = 0.0;
        self.with_vehicle_data(&mut |data| value = *data.id_damage.get());
        value
    }

    /// Sets the accumulated damage.
    fn set_vehicle_damage(&self, damage: f32) {
        self.with_vehicle_data(&mut |data| data.id_damage.set(damage));
    }

    /// Vanilla `VehicleEntity.hurtServer`.
    fn hurt_vehicle(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_removed() {
            return true;
        }
        if self.is_invulnerable_to_base(source) {
            return false;
        }

        self.set_hurt_dir(-self.hurt_dir());
        self.set_hurt_time(HURT_TIME);
        self.mark_hurt();
        self.set_vehicle_damage(self.vehicle_damage() + amount * 10.0);
        self.game_event_with_source_entity(
            &vanilla_game_events::ENTITY_DAMAGE,
            source.causing_entity().map(AsRef::as_ref),
        );

        let creative_player = source.is_creative_player();
        if (creative_player || self.vehicle_damage() <= BREAK_DAMAGE)
            && !self.should_source_destroy(source)
        {
            if creative_player {
                self.set_removed(RemovalReason::Discarded);
            }
        } else {
            self.destroy(world, source);
        }
        true
    }

    /// Vanilla `VehicleEntity.shouldSourceDestroy`: whether this damage breaks
    /// the vehicle at once, whatever its damage.
    fn should_source_destroy(&self, _source: &DamageSource) -> bool {
        false
    }

    /// Vanilla `VehicleEntity.destroy`: kills the vehicle and drops its item.
    fn destroy(&self, world: &World, _source: &DamageSource) {
        self.default_destroy(world);
    }

    /// The base `destroy` body, for overrides that call `super.destroy`.
    fn default_destroy(&self, world: &World) {
        self.kill(world);
        if !world.get_game_rule(&ENTITY_DROPS) {
            return;
        }
        let mut item = ItemStack::new(self.drop_item());
        if let Some(name) = self.custom_name() {
            item.set(CUSTOM_NAME, name);
        }
        let _ = self.spawn_at_location(item, 0.0);
    }

    /// Vanilla `VehicleEntity.ignoreExplosion`.
    fn vehicle_ignores_explosion(&self, explosion: &dyn Explosion) -> bool {
        !explosion.world().get_game_rule(&MOB_GRIEFING)
            && explosion
                .indirect_source_entity()
                .is_some_and(|entity| entity.as_mob().is_some())
    }
}
