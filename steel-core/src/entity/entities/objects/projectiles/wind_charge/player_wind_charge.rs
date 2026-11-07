//! Vanilla `WindCharge`: thrown by players and shot from dispensers;
//! knocks entities back without hurting them.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::sound_events;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::WindChargeEntityData;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey, WorldAabb};

use super::{AbstractWindCharge, explode_wind_burst};
use crate::entity::damage::DamageSource;
use crate::entity::{
    AbstractHurtingProjectile, Entity, EntityBase, EntityBaseLoad, EntitySyncedData,
    HurtingProjectileBase, Projectile, ProjectileBase, ProjectileHit, RemovalReason, SharedEntity,
};
use crate::world::explosion::SimpleExplosionDamageCalculator;
use crate::world::{ClipHitResult, World};

/// Vanilla `WindCharge.EXPLOSION_DAMAGE_CALCULATOR`: a stronger push.
static EXPLOSION_CALCULATOR: SimpleExplosionDamageCalculator = SimpleExplosionDamageCalculator::new(
    true,
    false,
    Some(1.22),
    Some(BlockTag::BLOCKS_WIND_CHARGE_EXPLOSIONS),
);
/// Vanilla `RADIUS`.
const RADIUS: f32 = 1.2;
/// Vanilla initial `noDeflectTicks`.
const NO_DEFLECT_TICKS: i32 = 5;

/// A player's wind charge.
#[entity_behavior(class = "WindCharge")]
pub struct WindChargeEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<WindChargeEntityData>,
    projectile_base: ProjectileBase,
    hurting_base: HurtingProjectileBase,
    no_deflect_ticks: AtomicI32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `WindChargeEntity`.
unsafe impl DowncastType for WindChargeEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/wind_charge");
}

impl WindChargeEntity {
    /// Creates a wind charge at rest.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved wind charge.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let charge = Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(WindChargeEntityData::new()),
            projectile_base: ProjectileBase::new(),
            hurting_base: HurtingProjectileBase::new(),
            no_deflect_ticks: AtomicI32::new(NO_DEFLECT_TICKS),
        };
        charge.init_wind_charge();
        charge
    }
}

impl Entity for WindChargeEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        Arc::clone(&self).wind_charge_tick();
        if self.no_deflect_ticks.load(Ordering::Relaxed) > 0 {
            self.no_deflect_ticks.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn is_pickable(&self) -> bool {
        self.projectile_is_pickable()
    }

    fn push_impulse(&self, _impulse: DVec3) {}

    fn can_collide_with(&self, other: &dyn Entity) -> bool {
        self.wind_charge_can_collide_with(other)
    }

    fn make_bounding_box_at(&self, position: DVec3) -> WorldAabb {
        self.wind_charge_bounding_box_at(position)
    }

    fn play_entity_on_fire_extinguished_sound(&self) {}

    fn spawn_data(&self) -> i32 {
        self.get_owner().map_or(0, |owner| owner.id())
    }

    fn restore_owner_reference(&self, owner: &SharedEntity) {
        self.cache_owner_entity(owner);
    }

    fn projectile_owner_uuid(&self) -> Option<uuid::Uuid> {
        self.owner_uuid()
    }

    fn projectile_owner(&self) -> Option<SharedEntity> {
        self.get_owner()
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_wind_charge(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_wind_charge(nbt);
    }
}

impl Projectile for WindChargeEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    fn can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.wind_charge_can_hit_entity(entity)
    }

    fn on_deflection(&self, by_attack: bool) {
        self.on_hurting_deflection(by_attack);
    }

    /// Vanilla `WindCharge.deflect`: a fresh charge cannot be knocked back
    /// for its first ticks.
    fn can_be_deflected(&self) -> bool {
        self.no_deflect_ticks.load(Ordering::Relaxed) <= 0
    }

    fn hurt(&self, _world: &World, _source: &DamageSource, _amount: f32) -> bool {
        false
    }

    fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
        self.wind_charge_hit_entity(entity);
    }

    fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        self.wind_charge_hit_block(hit);
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        Arc::clone(&self).projectile_on_hit(hit);
        self.set_removed(RemovalReason::Discarded);
    }
}

impl AbstractHurtingProjectile for WindChargeEntity {
    fn hurting_projectile_base(&self) -> &HurtingProjectileBase {
        &self.hurting_base
    }

    fn get_inertia(&self) -> f32 {
        1.0
    }

    fn get_liquid_inertia(&self) -> f32 {
        self.get_inertia()
    }

    fn should_burn(&self) -> bool {
        false
    }
}

impl AbstractWindCharge for WindChargeEntity {
    fn explode(self: &Arc<Self>, position: DVec3) {
        let Some(world) = self.level() else {
            return;
        };
        explode_wind_burst(
            &world,
            Arc::clone(self) as SharedEntity,
            &EXPLOSION_CALCULATOR,
            position,
            RADIUS,
            &sound_events::ENTITY_WIND_CHARGE_WIND_BURST,
        );
    }
}
