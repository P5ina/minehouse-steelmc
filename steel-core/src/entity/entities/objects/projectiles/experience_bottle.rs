//! Thrown bottle o' enchanting (`ThrownExperienceBottle`).
//!
//! Mirrors vanilla `ThrownExperienceBottle` on the Steel
//! `Projectile → ThrowableProjectile → ThrowableItemProjectile` trait stack.
//! On any impact it shows the splash, drops 3–11 experience thrown away from
//! the hit surface, and discards itself.

use std::sync::Arc;
use std::sync::Weak;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::vanilla_entity_data::ExperienceBottleEntityData;
use steel_registry::{level_events, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::entity::entities::ExperienceOrbEntity;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySyncedData, Projectile, ProjectileBase,
    ProjectileHit, RemovalReason, SharedEntity, ThrowableItemProjectile, ThrowableProjectile,
};
use crate::world::World;

/// Vanilla `ThrownExperienceBottle.getDefaultGravity`.
const GRAVITY: f64 = 0.07;
/// Potion color the splash particles use (`-13083194`, `0xFF385DC6`).
const SPLASH_COLOR: i32 = -13_083_194;
/// Fixed part of the dropped experience.
const BASE_EXPERIENCE: i32 = 3;
/// Each of the two random rolls adds `0..EXPERIENCE_ROLL`.
const EXPERIENCE_ROLL: i32 = 5;

/// A thrown bottle o' enchanting.
#[entity_behavior(class = "ThrownExperienceBottle")]
pub struct ExperienceBottleEntity {
    /// Common entity fields (id, uuid, position, etc.).
    base: EntityBase,
    /// Vanilla entity type registered for this implementation.
    entity_type: EntityTypeRef,
    /// Synced data carrying the rendered item stack.
    entity_data: SyncMutex<ExperienceBottleEntityData>,
    /// Shared `Projectile` state (owner / left-owner / has-been-shot).
    projectile_base: ProjectileBase,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ExperienceBottleEntity`.
unsafe impl DowncastType for ExperienceBottleEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/experience_bottle");
}

impl ExperienceBottleEntity {
    /// Creates a new thrown bottle with no owner and the default rendered item.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self {
            base: EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
            entity_data: SyncMutex::new(ExperienceBottleEntityData::new()),
            projectile_base: ProjectileBase::new(),
        }
    }

    /// Creates a thrown bottle from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(ExperienceBottleEntityData::new()),
            projectile_base: ProjectileBase::new(),
        }
    }

    /// Vanilla `3 + random.nextInt(5) + random.nextInt(5)`.
    fn roll_experience() -> i32 {
        BASE_EXPERIENCE
            + rand::random_range(0..EXPERIENCE_ROLL)
            + rand::random_range(0..EXPERIENCE_ROLL)
    }

    /// The direction the orbs are thrown: the hit face's normal for a block,
    /// otherwise back along the bottle's flight.
    fn orb_direction(&self, hit: &ProjectileHit) -> DVec3 {
        match hit {
            ProjectileHit::Block { hit, .. } => hit.direction.offset_vec().as_dvec3(),
            ProjectileHit::Entity(_) => -self.velocity(),
        }
    }
}

impl Entity for ExperienceBottleEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.throwable_projectile_tick();
    }

    fn get_default_gravity(&self) -> f64 {
        GRAVITY
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
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
        self.save_throwable_item(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        self.load_throwable_item(nbt);
    }
}

impl Projectile for ExperienceBottleEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        // Vanilla `ThrownExperienceBottle.onHit`: super.onHit(), then the
        // server-side splash and experience.
        Arc::clone(&self).projectile_on_hit(hit);

        if let Some(world) = self.level() {
            let block_pos = BlockPos::from(self.position());
            world.level_event(
                level_events::PARTICLES_SPELL_POTION_SPLASH,
                block_pos,
                SPLASH_COLOR,
                None,
            );
            if !self.is_silent() {
                world.level_event(level_events::SOUND_SPELL_POTION_SPLASH, block_pos, 0, None);
            }

            ExperienceOrbEntity::award_with_direction(
                &world,
                hit.location(),
                self.orb_direction(hit),
                Self::roll_experience(),
            );
        }
        self.set_removed(RemovalReason::Discarded);
    }
}

impl ThrowableProjectile for ExperienceBottleEntity {}

impl ThrowableItemProjectile for ExperienceBottleEntity {
    fn get_default_item(&self) -> ItemRef {
        &vanilla_items::EXPERIENCE_BOTTLE
    }

    fn set_item(&self, item: ItemStack) {
        self.entity_data
            .lock()
            .throwable_item_projectile
            .item_stack
            .set(item);
    }

    fn get_item(&self) -> ItemStack {
        self.entity_data
            .lock()
            .throwable_item_projectile
            .item_stack
            .get()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use steel_utils::Direction;

    use crate::entity::EntityHitResult;
    use crate::world::ClipHitResult;

    use super::*;

    fn new_bottle() -> Arc<ExperienceBottleEntity> {
        Arc::new(ExperienceBottleEntity::new(
            &vanilla_entities::EXPERIENCE_BOTTLE,
            1,
            DVec3::ZERO,
            Weak::<World>::new(),
        ))
    }

    #[test]
    fn experience_roll_stays_in_vanilla_range() {
        for _ in 0..1000 {
            let xp = ExperienceBottleEntity::roll_experience();
            assert!((3..=11).contains(&xp), "rolled {xp}");
        }
    }

    #[test]
    fn orbs_leave_a_block_along_its_face_and_an_entity_against_the_flight() {
        init_vanilla_registry();
        let bottle = new_bottle();
        bottle.set_velocity(DVec3::new(0.5, -1.0, 0.0));

        let block_hit = ProjectileHit::Block {
            location: DVec3::ZERO,
            hit: ClipHitResult {
                location: DVec3::ZERO,
                direction: Direction::East,
                block_pos: BlockPos::new(0, 0, 0),
                miss: false,
                inside: false,
                world_border_hit: false,
            },
        };
        assert_eq!(bottle.orb_direction(&block_hit), DVec3::X);

        let entity_hit = ProjectileHit::Entity(EntityHitResult {
            entity: Arc::clone(&bottle) as SharedEntity,
            location: DVec3::ZERO,
        });
        assert_eq!(
            bottle.orb_direction(&entity_hit),
            DVec3::new(-0.5, 1.0, 0.0)
        );
    }

    #[test]
    fn bottle_uses_vanilla_gravity_and_item() {
        init_vanilla_registry();
        let bottle = new_bottle();
        assert_eq!(bottle.get_default_gravity(), 0.07);
        assert_eq!(
            bottle.get_item().item,
            &vanilla_items::EXPERIENCE_BOTTLE as ItemRef
        );
    }

    #[test]
    fn on_hit_discards_the_bottle() {
        init_vanilla_registry();
        let bottle = new_bottle();
        let hit = ProjectileHit::Block {
            location: DVec3::ZERO,
            hit: ClipHitResult {
                location: DVec3::ZERO,
                direction: Direction::Up,
                block_pos: BlockPos::new(0, 0, 0),
                miss: false,
                inside: false,
                world_border_hit: false,
            },
        };
        Arc::clone(&bottle).on_hit(&hit);
        assert!(bottle.is_removed());
    }
}
