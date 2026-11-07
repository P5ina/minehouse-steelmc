//! Vanilla `ShulkerBullet`: the homing projectile shulkers fire, which
//! levitates whatever it hits.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::particle_type::ParticleData;
use steel_registry::vanilla_entity_data::ShulkerBulletEntityData;
use steel_registry::vanilla_particle_types::{CRIT, EXPLOSION};
use steel_registry::{
    sound_events, vanilla_damage_types, vanilla_entities, vanilla_game_events, vanilla_mob_effects,
};
use steel_utils::axis::Axis;
use steel_utils::locks::SyncMutex;
use steel_utils::types::Difficulty;
use steel_utils::{BlockPos, Direction, DowncastType, DowncastTypeKey};

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::living_base::MobEffectInstance;
use crate::entity::projectile::rotate_towards_movement;
use crate::entity::reference::EntityReference;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySyncedData, Projectile, ProjectileBase,
    ProjectileDeflection, ProjectileHit, RemovalReason, SharedEntity, next_entity_id,
};
use crate::physics::WorldCollisionProvider;
use crate::portal::PortalProcessor;
use crate::world::game_event::GameEventContext;
use crate::world::{ClipHitResult, World};

/// Vanilla `SPEED`.
const SPEED: f64 = 0.15;
/// Vanilla `getDefaultGravity`.
const GRAVITY: f64 = 0.04;
/// Damage dealt on an entity hit.
const DAMAGE: f32 = 4.0;
/// Levitation applied on an entity hit, in ticks.
const LEVITATION_TICKS: i32 = 200;

/// The homing state of a shulker bullet.
#[derive(Clone, Default)]
struct BulletState {
    final_target: Option<EntityReference>,
    current_move_direction: Option<Direction>,
    flight_steps: i32,
    target_delta: DVec3,
}

/// A shulker bullet.
#[entity_behavior(class = "ShulkerBullet")]
pub struct ShulkerBulletEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ShulkerBulletEntityData>,
    projectile_base: ProjectileBase,
    state: SyncMutex<BulletState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ShulkerBulletEntity`.
unsafe impl DowncastType for ShulkerBulletEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/shulker_bullet");
}

impl ShulkerBulletEntity {
    /// Creates a shulker bullet at rest.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved shulker bullet.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let bullet = Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(ShulkerBulletEntityData::new()),
            projectile_base: ProjectileBase::new(),
            state: SyncMutex::new(BulletState::default()),
        };
        bullet.set_no_physics(true);
        bullet
    }

    /// Vanilla `ShulkerBullet(level, owner, target, invalidStartAxis)`: a
    /// bullet at the owner's box center, heading for `target` off
    /// `invalid_start_axis`.
    #[must_use]
    pub(crate) fn fired_by(
        world: &Arc<World>,
        owner: &SharedEntity,
        target: &SharedEntity,
        invalid_start_axis: Axis,
    ) -> Self {
        let bullet = Self::new(
            &vanilla_entities::SHULKER_BULLET,
            next_entity_id(),
            owner.bounding_box().center(),
            Arc::downgrade(world),
        );
        bullet.set_owner_entity(Some(owner));
        {
            let mut state = bullet.state.lock();
            state.final_target = Some(EntityReference::from_entity(target));
            state.current_move_direction = Some(Direction::Up);
        }
        bullet.select_next_move_direction(Some(invalid_start_axis), Some(target));
        bullet
    }

    /// The direction the bullet is currently stepping along.
    #[must_use]
    pub fn move_direction(&self) -> Option<Direction> {
        self.state.lock().current_move_direction
    }

    /// The velocity the bullet is steering toward.
    #[must_use]
    pub fn target_delta(&self) -> DVec3 {
        self.state.lock().target_delta
    }

    /// Vanilla `selectNextMoveDirection`: pick an open axis-aligned step
    /// toward the target (or straight down without one).
    fn select_next_move_direction(&self, avoid_axis: Option<Axis>, target: Option<&SharedEntity>) {
        let Some(world) = self.level() else {
            return;
        };
        let mut y_offset = 0.5;
        let target_pos = match target {
            None => self.block_position().below(),
            Some(target) => {
                y_offset = f64::from(target.base().dimensions().height) * 0.5;
                let position = target.position();
                BlockPos::containing(position.x, position.y + y_offset, position.z)
            }
        };
        let mut target_x = f64::from(target_pos.x()) + 0.5;
        let mut target_y = f64::from(target_pos.y()) + y_offset;
        let mut target_z = f64::from(target_pos.z()) + 0.5;
        let mut selection = None;
        if !target_pos.closer_to_center_than(self.position(), 2.0) {
            let current = self.block_position();
            let is_empty = |pos: BlockPos| world.get_block_state(pos).is_air();
            let mut options = Vec::new();
            if avoid_axis != Some(Axis::X) {
                if current.x() < target_pos.x() && is_empty(current.east()) {
                    options.push(Direction::East);
                } else if current.x() > target_pos.x() && is_empty(current.west()) {
                    options.push(Direction::West);
                }
            }
            if avoid_axis != Some(Axis::Y) {
                if current.y() < target_pos.y() && is_empty(current.above()) {
                    options.push(Direction::Up);
                } else if current.y() > target_pos.y() && is_empty(current.below()) {
                    options.push(Direction::Down);
                }
            }
            if avoid_axis != Some(Axis::Z) {
                if current.z() < target_pos.z() && is_empty(current.south()) {
                    options.push(Direction::South);
                } else if current.z() > target_pos.z() && is_empty(current.north()) {
                    options.push(Direction::North);
                }
            }

            let mut direction = Direction::random();
            if options.is_empty() {
                let mut attempts = 5;
                while !is_empty(direction.relative(current)) && attempts > 0 {
                    direction = Direction::random();
                    attempts -= 1;
                }
            } else {
                direction = options[rand::random_range(0..options.len())];
            }

            let (step_x, step_y, step_z) = direction.offset();
            let position = self.position();
            target_x = position.x + f64::from(step_x);
            target_y = position.y + f64::from(step_y);
            target_z = position.z + f64::from(step_z);
            selection = Some(direction);
        }

        let position = self.position();
        let delta = DVec3::new(target_x, target_y, target_z) - position;
        let distance = delta.length();
        let target_delta = if distance == 0.0 {
            DVec3::ZERO
        } else {
            delta / distance * SPEED
        };
        {
            let mut state = self.state.lock();
            state.current_move_direction = selection;
            state.target_delta = target_delta;
            state.flight_steps = 10 + rand::random_range(0..5) * 10;
        }
        self.mark_velocity_sync();
    }

    /// Vanilla `destroy`.
    fn destroy(&self) {
        self.set_removed(RemovalReason::Discarded);
        if let Some(world) = self.level() {
            world.game_event_at(
                &vanilla_game_events::ENTITY_DAMAGE,
                self.position(),
                &GameEventContext::new(Some(self), None),
            );
        }
    }

    fn final_target(&self, world: &World) -> Option<SharedEntity> {
        let reference = self.state.lock().final_target.clone()?;
        reference.get_entity(world)
    }
}

impl Entity for ShulkerBulletEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn tick(self: Arc<Self>) {
        self.projectile_base_tick();
        let Some(world) = self.level() else {
            return;
        };
        let final_target = self.final_target(&world);
        if final_target.is_none() {
            self.state.lock().final_target = None;
        }
        match &final_target {
            Some(target)
                if target.is_alive() && !target.as_player().is_some_and(Entity::is_spectator) =>
            {
                let movement = self.velocity();
                let target_delta = {
                    let mut state = self.state.lock();
                    state.target_delta =
                        (state.target_delta * 1.025).clamp(DVec3::splat(-1.0), DVec3::splat(1.0));
                    state.target_delta
                };
                self.set_velocity(movement + (target_delta - movement) * 0.2);
            }
            _ => self.apply_gravity(),
        }
        let hit = self.get_hit_result_on_move_vector();

        let movement = self.velocity();
        if let Err(error) = self.try_set_position(self.position() + movement) {
            log::debug!("failed to advance shulker bullet {}: {error}", self.id());
            self.set_removed(RemovalReason::Discarded);
            return;
        }
        self.apply_effects_from_blocks();
        if self
            .base()
            .portal_process()
            .is_some_and(PortalProcessor::is_inside_portal_this_tick)
        {
            self.handle_portal();
        }
        if let Some(hit) = hit
            && self.is_alive()
            && !self.is_world_change_pending()
        {
            Arc::clone(&self).hit_target_or_deflect_self(&hit);
        }
        rotate_towards_movement(self.as_ref(), 0.5);

        let Some(final_target) = final_target else {
            return;
        };
        let reselect_axis = {
            let mut state = self.state.lock();
            if state.flight_steps > 0 {
                state.flight_steps -= 1;
                (state.flight_steps == 0).then(|| state.current_move_direction.map(Direction::axis))
            } else {
                None
            }
        };
        if let Some(axis) = reselect_axis {
            self.select_next_move_direction(axis, Some(&final_target));
        }
        let current_move_direction = self.move_direction();
        if let Some(direction) = current_move_direction {
            let current = self.block_position();
            let axis = direction.axis();
            let collision = WorldCollisionProvider::for_entity(&world, self.as_ref());
            if collision
                .loaded_and_entity_can_stand_on_face(direction.relative(current), Direction::Up)
            {
                self.select_next_move_direction(Some(axis), Some(&final_target));
            } else {
                let target_pos = final_target.block_position();
                if axis == Axis::X && current.x() == target_pos.x()
                    || axis == Axis::Z && current.z() == target_pos.z()
                    || axis == Axis::Y && current.y() == target_pos.y()
                {
                    self.select_next_move_direction(Some(axis), Some(&final_target));
                }
            }
        }
    }

    fn check_despawn(&self) {
        if self
            .level()
            .is_some_and(|world| world.difficulty() == Difficulty::Peaceful)
        {
            self.set_removed(RemovalReason::Discarded);
        }
    }

    fn get_default_gravity(&self) -> f64 {
        GRAVITY
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    fn is_on_fire(&self) -> bool {
        false
    }

    fn is_pickable(&self) -> bool {
        true
    }

    fn is_affected_by_blocks(&self) -> bool {
        !self.is_removed()
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
        let state = self.state.lock();
        if let Some(target) = &state.final_target {
            target.store(nbt, "Target");
        }
        if let Some(direction) = state.current_move_direction {
            nbt.insert("Dir", direction.get_3d_data_value());
        }
        nbt.insert("Steps", state.flight_steps);
        nbt.insert("TXD", state.target_delta.x);
        nbt.insert("TYD", state.target_delta.y);
        nbt.insert("TZD", state.target_delta.z);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        let mut state = self.state.lock();
        state.flight_steps = nbt.int("Steps").unwrap_or(0);
        state.target_delta = DVec3::new(
            nbt.double("TXD").unwrap_or(0.0),
            nbt.double("TYD").unwrap_or(0.0),
            nbt.double("TZD").unwrap_or(0.0),
        );
        state.current_move_direction = nbt
            .int("Dir")
            .filter(|id| (0..6).contains(id))
            .map(Direction::from_3d_data_value);
        state.final_target = EntityReference::read(&nbt, "Target");
    }
}

impl Projectile for ShulkerBulletEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    fn can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.base_can_hit_entity(entity) && !entity.no_physics()
    }

    fn on_redirect_projectile(&self, hit_projectile: &dyn Projectile) {
        hit_projectile.deflect(
            ProjectileDeflection::MomentumDeflect,
            Some(self),
            self.owner_reference(),
            true,
        );
    }

    fn hurt(&self, world: &World, _source: &DamageSource, _amount: f32) -> bool {
        self.play_sound(&sound_events::ENTITY_SHULKER_BULLET_HURT, 1.0, 1.0);
        world.send_particles(
            ParticleData::simple(&CRIT),
            self.position(),
            15,
            DVec3::splat(0.2),
            0.0,
        );
        self.destroy();
        true
    }

    fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
        let Some(world) = entity.level() else {
            return;
        };
        let owner = self.get_owner();
        let living_owner = owner
            .as_ref()
            .filter(|owner| owner.as_living_entity().is_some())
            .map(Arc::clone);
        let mut damage_source = DamageSource::environment(&vanilla_damage_types::MOB_PROJECTILE)
            .with_direct_entity(Arc::clone(&self) as SharedEntity);
        if let Some(living_owner) = living_owner {
            damage_source = damage_source.with_causing_entity(living_owner);
        }
        if !entity.hurt(&world, &damage_source, DAMAGE) {
            return;
        }
        let context = EnchantmentPostAttackContext::new(entity, &damage_source);
        enchantment_helper::do_post_attack_effects_with_item_source(
            &world,
            entity.as_ref(),
            &ItemStack::empty(),
            &context,
        );
        if let Some(living) = entity.as_living_entity() {
            let effect_source: &dyn Entity = match &owner {
                Some(owner) => owner.as_ref(),
                None => self.as_ref(),
            };
            living.add_mob_effect_with_source(
                MobEffectInstance::with_duration(
                    vanilla_mob_effects::LEVITATION,
                    LEVITATION_TICKS,
                    0,
                ),
                Some(effect_source),
            );
        }
    }

    fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        self.projectile_on_hit_block(hit);
        if let Some(world) = self.level() {
            world.send_particles(
                ParticleData::simple(&EXPLOSION),
                self.position(),
                2,
                DVec3::splat(0.2),
                0.0,
            );
        }
        self.play_sound(&sound_events::ENTITY_SHULKER_BULLET_HIT, 1.0, 1.0);
    }

    fn on_hit(self: Arc<Self>, hit: &ProjectileHit) {
        Arc::clone(&self).projectile_on_hit(hit);
        self.destroy();
    }
}
