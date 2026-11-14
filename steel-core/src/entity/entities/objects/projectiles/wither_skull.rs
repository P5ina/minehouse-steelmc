//! Vanilla `WitherSkull`: the wither's explosive projectile.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::fluid::FluidState;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::WitherSkullEntityData;
use steel_registry::{vanilla_damage_types, vanilla_mob_effects};
use steel_utils::locks::SyncMutex;
use steel_utils::nbt::NbtNumeric as _;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::living_base::MobEffectInstance;
use crate::entity::{
    AbstractHurtingProjectile, Entity, EntityBase, EntityBaseLoad, EntitySyncedData,
    HurtingProjectileBase, Projectile, ProjectileBase, ProjectileHit, RemovalReason, SharedEntity,
};
use crate::world::World;
use crate::world::explosion::{Explosion, ExplosionInteraction, ExplosionOptions};

const TAG_DANGEROUS: &str = "dangerous";
/// Damage from a skull with a living owner.
const OWNED_DAMAGE: f32 = 8.0;
/// Damage from an ownerless skull.
const UNOWNED_DAMAGE: f32 = 5.0;
/// Health the owner regains when its skull kills.
const KILL_HEAL: f32 = 5.0;
/// Explosion radius on impact.
const EXPLOSION_RADIUS: f32 = 1.0;
/// Inertia of a blue (dangerous) skull.
const DANGEROUS_INERTIA: f32 = 0.73;

/// Vanilla `WitherBoss.canDestroy`: what the wither and its blue skulls break.
#[must_use]
pub fn wither_can_destroy(state: BlockStateId) -> bool {
    !state.is_air() && !state.get_block().has_tag(&BlockTag::WITHER_IMMUNE)
}

/// Vanilla `WitherSkull`.
#[entity_behavior(class = "WitherSkull")]
pub struct WitherSkullEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<WitherSkullEntityData>,
    projectile_base: ProjectileBase,
    hurting_base: HurtingProjectileBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `WitherSkullEntity`.
unsafe impl DowncastType for WitherSkullEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/wither_skull");
}

impl WitherSkullEntity {
    /// Creates a wither skull at rest.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved wither skull.
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
            entity_data: SyncMutex::new(WitherSkullEntityData::new()),
            projectile_base: ProjectileBase::new(),
            hurting_base: HurtingProjectileBase::new(),
        }
    }

    /// Vanilla `isDangerous`: a blue skull, which flies slower and breaks
    /// tougher blocks.
    #[must_use]
    pub fn is_dangerous(&self) -> bool {
        *self.entity_data.lock().dangerous.get()
    }

    /// Vanilla `setDangerous`.
    pub fn set_dangerous(&self, dangerous: bool) {
        self.entity_data.lock().dangerous.set(dangerous);
    }

    /// Seconds of Wither II a hit inflicts on `difficulty`.
    const fn wither_seconds(difficulty: Difficulty) -> i32 {
        match difficulty {
            Difficulty::Normal => 10,
            Difficulty::Hard => 40,
            Difficulty::Peaceful | Difficulty::Easy => 0,
        }
    }
}

impl Entity for WitherSkullEntity {
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

    fn is_on_fire(&self) -> bool {
        false
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

    fn block_explosion_resistance(
        &self,
        _explosion: &dyn Explosion,
        _world: &World,
        _pos: BlockPos,
        state: BlockStateId,
        _fluid: FluidState,
        resistance: f32,
    ) -> f32 {
        if self.is_dangerous() && wither_can_destroy(state) {
            resistance.min(0.8)
        } else {
            resistance
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_projectile(nbt);
        self.save_hurting_projectile(nbt);
        nbt.insert(TAG_DANGEROUS, i8::from(self.is_dangerous()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        self.load_hurting_projectile(nbt);
        self.set_dangerous(
            nbt.get(TAG_DANGEROUS)
                .and_then(|tag| tag.byte_value())
                .is_some_and(|value| value != 0),
        );
    }
}

impl Projectile for WitherSkullEntity {
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
        let Some(world) = self.level() else {
            return;
        };
        let owner = self.get_owner();
        let living_owner = owner
            .as_ref()
            .filter(|owner| owner.as_living_entity().is_some());
        let was_hurt = if let Some(living_owner) = living_owner {
            let damage_source = DamageSource::environment(&vanilla_damage_types::WITHER_SKULL)
                .with_direct_entity(Arc::clone(&self) as SharedEntity)
                .with_causing_entity(Arc::clone(living_owner));
            let was_hurt = entity.hurt(&world, &damage_source, OWNED_DAMAGE);
            if was_hurt {
                if entity.is_alive() {
                    let context = EnchantmentPostAttackContext::new(entity, &damage_source);
                    enchantment_helper::do_post_attack_effects_with_item_source(
                        &world,
                        entity.as_ref(),
                        &ItemStack::empty(),
                        &context,
                    );
                } else if let Some(owner) = living_owner.as_living_entity() {
                    owner.heal(KILL_HEAL);
                }
            }
            was_hurt
        } else {
            entity.hurt(
                &world,
                &DamageSource::environment(&vanilla_damage_types::MAGIC),
                UNOWNED_DAMAGE,
            )
        };

        if was_hurt && let Some(living) = entity.as_living_entity() {
            let seconds = Self::wither_seconds(world.difficulty());
            if seconds > 0 {
                // Vanilla `Projectile.getEffectSource`: the owner, else the skull.
                let effect_source: &dyn Entity = match &owner {
                    Some(owner) => owner.as_ref(),
                    None => self.as_ref(),
                };
                living.add_mob_effect_with_source(
                    MobEffectInstance::with_duration(vanilla_mob_effects::WITHER, 20 * seconds, 1),
                    Some(effect_source),
                );
            }
        }
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        Arc::clone(&self).projectile_on_hit(hit);
        let Some(world) = self.level() else {
            return;
        };
        let mut options =
            ExplosionOptions::new(self.position(), EXPLOSION_RADIUS, ExplosionInteraction::Mob);
        options.source = Some(Arc::clone(&self) as SharedEntity);
        world.explode(options);
        self.set_removed(RemovalReason::Discarded);
    }
}

impl AbstractHurtingProjectile for WitherSkullEntity {
    fn hurting_projectile_base(&self) -> &HurtingProjectileBase {
        &self.hurting_base
    }

    fn get_inertia(&self) -> f32 {
        if self.is_dangerous() {
            DANGEROUS_INERTIA
        } else {
            0.95
        }
    }

    fn should_burn(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};

    use super::*;

    fn new_skull() -> WitherSkullEntity {
        WitherSkullEntity::new(&vanilla_entities::WITHER_SKULL, 1, DVec3::ZERO, Weak::new())
    }

    #[test]
    fn blue_skulls_fly_slower_and_never_burn() {
        init_vanilla_registry();
        let skull = new_skull();
        assert_eq!(skull.get_inertia(), 0.95);
        skull.set_dangerous(true);
        assert_eq!(skull.get_inertia(), DANGEROUS_INERTIA);
        assert!(!skull.should_burn());
        assert!(!skull.is_on_fire());
    }

    #[test]
    fn dangerous_flag_round_trips() {
        init_vanilla_registry();
        let skull = new_skull();
        skull.set_dangerous(true);
        let mut nbt = NbtCompound::new();
        skull.save_additional(&mut nbt);
        assert_eq!(nbt.byte(TAG_DANGEROUS), Some(1));

        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed =
            read_compound(&mut Cursor::new(bytes.as_slice())).expect("test NBT should reborrow");
        let loaded = new_skull();
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_dangerous());
    }

    #[test]
    fn the_wither_spares_air_and_wither_immune_blocks() {
        init_vanilla_registry();
        assert!(wither_can_destroy(vanilla_blocks::OBSIDIAN.default_state()));
        assert!(!wither_can_destroy(vanilla_blocks::BEDROCK.default_state()));
        assert!(!wither_can_destroy(vanilla_blocks::AIR.default_state()));
    }

    #[test]
    fn wither_lasts_by_difficulty() {
        assert_eq!(WitherSkullEntity::wither_seconds(Difficulty::Easy), 0);
        assert_eq!(WitherSkullEntity::wither_seconds(Difficulty::Normal), 10);
        assert_eq!(WitherSkullEntity::wither_seconds(Difficulty::Hard), 40);
    }
}
