//! Vanilla `LargeFireball`: the ghast's fireball, exploding on impact.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_data::FireballEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_utils::locks::SyncMutex;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{Fireball, fireball_damage_source, fireball_post_attack_effects};
use crate::entity::damage::DamageSource;
use crate::entity::{
    AbstractHurtingProjectile, Entity, EntityBase, EntityBaseLoad, EntitySyncedData,
    HurtingProjectileBase, Projectile, ProjectileBase, ProjectileHit, RemovalReason, SharedEntity,
};
use crate::world::World;
use crate::world::explosion::{ExplosionInteraction, ExplosionOptions};

/// Vanilla `LargeFireball` impact damage.
const IMPACT_DAMAGE: f32 = 6.0;
/// Vanilla `DEFAULT_EXPLOSION_POWER`.
pub const DEFAULT_EXPLOSION_POWER: i32 = 1;
const EXPLOSION_POWER_TAG: &str = "ExplosionPower";

/// A large fireball.
#[entity_behavior(class = "LargeFireball")]
pub struct LargeFireballEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<FireballEntityData>,
    projectile_base: ProjectileBase,
    hurting_base: HurtingProjectileBase,
    explosion_power: SyncMutex<i32>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `LargeFireballEntity`.
unsafe impl DowncastType for LargeFireballEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/large_fireball");
}

impl LargeFireballEntity {
    /// Creates a small fireball at rest.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved small fireball.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(FireballEntityData::new()),
            projectile_base: ProjectileBase::new(),
            hurting_base: HurtingProjectileBase::new(),
            explosion_power: SyncMutex::new(DEFAULT_EXPLOSION_POWER),
        }
    }

    /// Radius of the explosion on impact.
    #[must_use]
    pub fn explosion_power(&self) -> i32 {
        *self.explosion_power.lock()
    }

    /// Sets the radius of the explosion on impact (vanilla ghasts use 1).
    pub fn set_explosion_power(&self, explosion_power: i32) {
        *self.explosion_power.lock() = explosion_power;
    }
}

impl Entity for LargeFireballEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.hurting_projectile_tick();
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    fn is_pickable(&self) -> bool {
        self.projectile_is_pickable()
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
        self.save_fireball(nbt);
        // Vanilla stores the power as a byte.
        nbt.insert(EXPLOSION_POWER_TAG, self.explosion_power() as i8);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fireball(nbt);
        self.set_explosion_power(
            nbt.byte(EXPLOSION_POWER_TAG)
                .map_or(DEFAULT_EXPLOSION_POWER, i32::from),
        );
    }
}

impl Projectile for LargeFireballEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    fn can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.hurting_can_hit_entity(entity)
    }

    fn on_deflection(&self, by_attack: bool) {
        self.on_hurting_deflection(by_attack);
    }

    fn hurt(&self, _world: &World, _source: &DamageSource, _amount: f32) -> bool {
        false
    }

    fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
        let Some(world) = entity.level() else {
            return;
        };
        let damage_source =
            fireball_damage_source(Arc::clone(&self) as SharedEntity, self.get_owner());
        // Vanilla runs the post-attack effects whether or not the hit landed.
        entity.hurt(&world, &damage_source, IMPACT_DAMAGE);
        fireball_post_attack_effects(&world, entity, &damage_source);
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        Arc::clone(&self).projectile_on_hit(hit);
        let Some(world) = self.level() else {
            return;
        };
        let grief = world.get_game_rule(&MOB_GRIEFING);
        let mut options = ExplosionOptions::new(
            self.position(),
            self.explosion_power() as f32,
            ExplosionInteraction::Mob,
        );
        options.source = Some(Arc::clone(&self) as SharedEntity);
        options.fire = grief;
        world.explode(options);
        self.set_removed(RemovalReason::Discarded);
    }
}

impl AbstractHurtingProjectile for LargeFireballEntity {
    fn hurting_projectile_base(&self) -> &HurtingProjectileBase {
        &self.hurting_base
    }
}

impl Fireball for LargeFireballEntity {
    fn get_item(&self) -> ItemStack {
        self.entity_data.lock().item_stack.get().clone()
    }

    fn set_synced_item(&self, item: ItemStack) {
        self.entity_data.lock().item_stack.set(item);
    }
}
