//! Vanilla `SmallFireball`: shot by blazes and dispensers, sets targets and
//! blocks on fire.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_data::SmallFireballEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_utils::locks::SyncMutex;
use steel_utils::types::UpdateFlags;
use steel_utils::{DowncastType, DowncastTypeKey};

use super::{Fireball, fireball_damage_source, fireball_post_attack_effects};
use crate::behavior::blocks::FireBlock;
use crate::entity::damage::DamageSource;
use crate::entity::{
    AbstractHurtingProjectile, Entity, EntityBase, EntityBaseLoad, EntitySyncedData,
    HurtingProjectileBase, Projectile, ProjectileBase, ProjectileHit, RemovalReason, SharedEntity,
};
use crate::world::{ClipHitResult, World};

/// Vanilla `SmallFireball` impact damage.
const IMPACT_DAMAGE: f32 = 5.0;
/// Vanilla `igniteForSeconds(5.0F)`.
const IGNITE_TICKS: i32 = 100;

/// A small fireball.
#[entity_behavior(class = "SmallFireball")]
pub struct SmallFireballEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<SmallFireballEntityData>,
    projectile_base: ProjectileBase,
    hurting_base: HurtingProjectileBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SmallFireballEntity`.
unsafe impl DowncastType for SmallFireballEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/small_fireball");
}

impl SmallFireballEntity {
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
            entity_data: SyncMutex::new(SmallFireballEntityData::new()),
            projectile_base: ProjectileBase::new(),
            hurting_base: HurtingProjectileBase::new(),
        }
    }
}

impl Entity for SmallFireballEntity {
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
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_fireball(nbt);
    }
}

impl Projectile for SmallFireballEntity {
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
        let remaining_fire_ticks = entity.remaining_fire_ticks();
        entity.ignite_for_ticks(IGNITE_TICKS);
        let damage_source =
            fireball_damage_source(Arc::clone(&self) as SharedEntity, self.get_owner());
        if entity.hurt(&world, &damage_source, IMPACT_DAMAGE) {
            fireball_post_attack_effects(&world, entity, &damage_source);
        } else {
            entity.set_remaining_fire_ticks(remaining_fire_ticks);
        }
    }

    fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        self.projectile_on_hit_block(hit);
        let Some(world) = self.level() else {
            return;
        };
        let owner_is_mob = self
            .get_owner()
            .is_some_and(|owner| owner.as_mob().is_some());
        if owner_is_mob && !world.get_game_rule(&MOB_GRIEFING) {
            return;
        }
        let pos = hit.block_pos.relative(hit.direction);
        if world.get_block_state(pos).is_air() {
            world.set_block(
                pos,
                FireBlock::get_state(world.as_ref(), pos),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        Arc::clone(&self).projectile_on_hit(hit);
        self.set_removed(RemovalReason::Discarded);
    }
}

impl AbstractHurtingProjectile for SmallFireballEntity {
    fn hurting_projectile_base(&self) -> &HurtingProjectileBase {
        &self.hurting_base
    }
}

impl Fireball for SmallFireballEntity {
    fn get_item(&self) -> ItemStack {
        self.entity_data.lock().fireball.item_stack.get().clone()
    }

    fn set_synced_item(&self, item: ItemStack) {
        self.entity_data.lock().fireball.item_stack.set(item);
    }
}
