//! Vanilla `LlamaSpit`: the spit a llama shoots at its target.

use std::f32::consts::PI;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_math::trig;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_damage_types;
use steel_registry::vanilla_entity_data::LlamaSpitEntityData;
use steel_utils::axis::Axis;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySyncedData, Projectile, ProjectileBase,
    RemovalReason, SharedEntity,
};
use crate::world::{ClipHitResult, World};

/// Vanilla `LlamaSpit.getDefaultGravity`.
const GRAVITY: f64 = 0.06;
/// Vanilla `LlamaSpit.getAirDrag`.
const AIR_DRAG: f64 = 0.99;
/// Damage a spit deals on hit.
const SPIT_DAMAGE: f32 = 1.0;
/// Vanilla `owner.getEyeY() - 0.1F`.
const EYE_OFFSET: f32 = 0.1;

/// Spit shot by a llama.
#[entity_behavior(class = "LlamaSpit")]
pub struct LlamaSpitEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<LlamaSpitEntityData>,
    projectile_base: ProjectileBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `LlamaSpitEntity`.
unsafe impl DowncastType for LlamaSpitEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/llama_spit");
}

impl LlamaSpitEntity {
    /// Creates spit with no owner.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(LlamaSpitEntityData::new()),
            projectile_base: ProjectileBase::new(),
        }
    }

    /// Creates spit from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(LlamaSpitEntityData::new()),
            projectile_base: ProjectileBase::new(),
        }
    }

    /// Vanilla `LlamaSpit(level, owner)`: the spawn point in front of the
    /// llama's mouth.
    #[must_use]
    pub fn spawn_position(
        owner_position: DVec3,
        owner_width: f32,
        eye_y: f64,
        y_body_rot: f32,
    ) -> DVec3 {
        let reach = f64::from(owner_width + 1.0) * 0.5;
        let angle = y_body_rot * (PI / 180.0);
        DVec3::new(
            owner_position.x - reach * f64::from(trig::sin(angle)),
            eye_y - f64::from(EYE_OFFSET),
            owner_position.z + reach * f64::from(trig::cos(angle)),
        )
    }

    /// Vanilla `level().findBlocksIn(getBoundingBox()).filterState(isAir).noneMatched()`.
    fn touches_no_air(&self, world: &World) -> bool {
        let bounds = self.bounding_box();
        let min = BlockPos::containing(
            bounds.min(Axis::X),
            bounds.min(Axis::Y),
            bounds.min(Axis::Z),
        );
        let max = BlockPos::containing(
            bounds.max(Axis::X),
            bounds.max(Axis::Y),
            bounds.max(Axis::Z),
        );
        BlockPos::between_closed(min, max)
            .into_iter()
            .all(|pos| !world.get_block_state(pos).is_air())
    }
}

impl Entity for LlamaSpitEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `LlamaSpit.tick`.
    fn tick(self: Arc<Self>) {
        self.set_old_position_to_current();
        self.base().set_old_rotation_to_current();
        self.projectile_base_tick();

        let movement = self.velocity();
        if let Some(hit) = self.get_hit_result_on_move_vector() {
            Arc::clone(&self).hit_target_or_deflect_self(&hit);
        }
        if self.is_removed() {
            return;
        }
        let next = self.position() + movement;
        self.update_rotation();
        let Some(world) = self.level() else {
            return;
        };
        if self.touches_no_air(&world) || self.is_in_water() {
            self.set_removed(RemovalReason::Discarded);
            return;
        }
        self.set_velocity(movement * AIR_DRAG);
        self.apply_gravity();
        if let Err(error) = self.try_set_position(next) {
            log::debug!("failed to advance llama spit {}: {error}", self.id());
            self.set_removed(RemovalReason::Discarded);
        }
    }

    fn get_default_gravity(&self) -> f64 {
        GRAVITY
    }

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
        self.save_projectile(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
    }
}

impl Projectile for LlamaSpitEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    /// Vanilla `LlamaSpit.onHitEntity`: one point of spit damage, credited
    /// to the living owner. Spit without a living owner does nothing.
    fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
        let Some(owner) = self
            .get_owner()
            .filter(|owner| owner.as_living_entity().is_some())
        else {
            return;
        };
        let Some(world) = entity.level() else {
            return;
        };
        let spit: SharedEntity = Arc::<Self>::clone(&self);
        let source =
            DamageSource::direct(&vanilla_damage_types::SPIT, spit).with_causing_entity(owner);
        if entity.hurt(&world, &source, SPIT_DAMAGE) {
            let context = EnchantmentPostAttackContext::new(entity, &source);
            enchantment_helper::do_post_attack_effects_with_item_source(
                &world,
                entity.as_ref(),
                &ItemStack::empty(),
                &context,
            );
        }
    }

    /// Vanilla `LlamaSpit.onHitBlock`.
    fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        self.projectile_on_hit_block(hit);
        self.set_removed(RemovalReason::Discarded);
    }
}
