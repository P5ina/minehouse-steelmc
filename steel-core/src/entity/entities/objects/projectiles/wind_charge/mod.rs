//! Vanilla `AbstractWindCharge` and its two kinds: the player's wind charge and
//! the breeze's.

mod breeze_wind_charge;
mod player_wind_charge;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_protocol::packets::game::ExplosionParticlePalette;
use steel_registry::particle_type::ParticleData;
use steel_registry::sound_event::{SoundEventHolder, SoundEventRef};
use steel_registry::{vanilla_damage_types, vanilla_entities, vanilla_particle_types};

pub use breeze_wind_charge::BreezeWindChargeEntity;
pub use player_wind_charge::WindChargeEntity;

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::{AbstractHurtingProjectile, Entity, RemovalReason, SharedEntity};
use crate::world::ClipHitResult;
use crate::world::World;
use crate::world::explosion::{ExplosionDamageCalculator, ExplosionInteraction, ExplosionOptions};
use steel_registry::item_stack::ItemStack;
use steel_utils::WorldAabb;

/// Vanilla `makeBoundingBox` drops the box this far below the position
/// (`0.15F` widened to double).
const BOUNDING_BOX_Y_OFFSET: f64 = 0.15_f32 as f64;
/// Vanilla impact damage.
const IMPACT_DAMAGE: f32 = 1.0;
/// Vanilla explodes a wind charge this many blocks above the build limit.
const MAX_HEIGHT_ABOVE_WORLD: i32 = 30;
/// Vanilla moves block-hit bursts this far out along the hit face.
const BLOCK_HIT_NORMAL_SCALE: f64 = 0.25;

/// Whether `entity` is either kind of wind charge.
fn is_wind_charge(entity: &dyn Entity) -> bool {
    let entity_type = entity.entity_type();
    entity_type == &vanilla_entities::WIND_CHARGE
        || entity_type == &vanilla_entities::BREEZE_WIND_CHARGE
}

/// Vanilla `level.explode(..., ExplosionInteraction.TRIGGER, GUST_EMITTER_SMALL,
/// GUST_EMITTER_LARGE, WeightedList.of(), sound)`: a wind burst that pushes
/// entities and triggers blocks without breaking anything.
pub(crate) fn explode_wind_burst(
    world: &Arc<World>,
    source: SharedEntity,
    calculator: &dyn ExplosionDamageCalculator,
    center: DVec3,
    radius: f32,
    sound: SoundEventRef,
) {
    let mut options = ExplosionOptions::new(center, radius, ExplosionInteraction::Trigger);
    options.source = Some(source);
    options.damage_calculator = Some(calculator);
    options.small_explosion_particle =
        ParticleData::simple(&vanilla_particle_types::GUST_EMITTER_SMALL);
    options.large_explosion_particle =
        ParticleData::simple(&vanilla_particle_types::GUST_EMITTER_LARGE);
    options.block_particles = ExplosionParticlePalette::empty();
    options.explosion_sound = SoundEventHolder::registry(sound);
    world.explode(options);
}

/// Behavior shared by vanilla `AbstractWindCharge` subclasses.
pub trait AbstractWindCharge: AbstractHurtingProjectile + Sized + 'static {
    /// Vanilla `explode`: the kind's own wind burst at `position`.
    fn explode(self: &Arc<Self>, position: DVec3);

    /// Vanilla constructors: no acceleration and the lowered box.
    fn init_wind_charge(&self) {
        self.set_acceleration_power(0.0);
        self.base().set_bounding_box_y_offset(BOUNDING_BOX_Y_OFFSET);
    }

    /// Vanilla `makeBoundingBox`.
    fn wind_charge_bounding_box_at(&self, position: DVec3) -> WorldAabb {
        let dimensions = self.base().dimensions();
        WorldAabb::entity_box(
            position.x,
            position.y - BOUNDING_BOX_Y_OFFSET,
            position.z,
            f64::from(dimensions.half_width()),
            f64::from(dimensions.height),
        )
    }

    /// Vanilla `AbstractWindCharge.tick`: bursts once far above the world.
    fn wind_charge_tick(self: Arc<Self>) {
        if let Some(world) = self.level()
            && self.block_position().y() > world.get_max_y() + MAX_HEIGHT_ABOVE_WORLD
        {
            self.explode(self.position());
            self.set_removed(RemovalReason::Discarded);
            return;
        }
        self.hurting_projectile_tick();
    }

    /// Vanilla `canCollideWith`: wind charges pass through each other.
    fn wind_charge_can_collide_with(&self, other: &dyn Entity) -> bool {
        !is_wind_charge(other)
            && other.can_be_collided_with(Some(self.as_entity_event_source()))
            && !self.is_passenger_of_same_vehicle(other)
    }

    /// Vanilla `canHitEntity`: never other wind charges or end crystals.
    fn wind_charge_can_hit_entity(&self, entity: &dyn Entity) -> bool {
        !is_wind_charge(entity)
            && entity.entity_type() != &vanilla_entities::END_CRYSTAL
            && self.hurting_can_hit_entity(entity)
    }

    /// Vanilla `onHitEntity`: one point of wind damage, then a burst.
    fn wind_charge_hit_entity(self: Arc<Self>, entity: &SharedEntity) {
        let Some(world) = self.level() else {
            return;
        };
        let owner = self
            .get_owner()
            .filter(|owner| owner.as_living_entity().is_some());
        if let Some(living) = owner.as_ref().and_then(|owner| owner.as_living_entity()) {
            living.set_last_hurt_mob(Some(entity));
        }
        let mut source = DamageSource::environment(&vanilla_damage_types::WIND_CHARGE)
            .with_direct_entity(Arc::clone(&self) as SharedEntity);
        if let Some(owner) = owner {
            source = source.with_causing_entity(owner);
        }
        if entity.hurt(&world, &source, IMPACT_DAMAGE) && entity.as_living_entity().is_some() {
            let context = EnchantmentPostAttackContext::new(entity, &source);
            enchantment_helper::do_post_attack_effects_with_item_source(
                &world,
                entity.as_ref(),
                &ItemStack::empty(),
                &context,
            );
        }
        self.explode(self.position());
    }

    /// Vanilla `onHitBlock`: bursts just off the hit face.
    fn wind_charge_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        self.projectile_on_hit_block(hit);
        let normal = hit.direction.offset();
        let offset = DVec3::new(
            f64::from(normal.0),
            f64::from(normal.1),
            f64::from(normal.2),
        ) * BLOCK_HIT_NORMAL_SCALE;
        self.explode(hit.location + offset);
        self.set_removed(RemovalReason::Discarded);
    }

    /// Saves the projectile and acceleration.
    fn save_wind_charge(&self, nbt: &mut NbtCompound) {
        self.save_projectile(nbt);
        self.save_hurting_projectile(nbt);
    }

    /// Loads the projectile and acceleration.
    fn load_wind_charge(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        self.load_hurting_projectile(nbt);
    }
}
