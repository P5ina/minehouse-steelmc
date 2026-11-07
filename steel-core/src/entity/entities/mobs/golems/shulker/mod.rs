//! Vanilla `Shulker`: the End city box mob that clings to a block face,
//! peeks open and fires homing bullets.

use std::sync::{Arc, Weak};

use std::f32::consts::PI;

use glam::{DVec3, Vec3};
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_math::trig;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::dye_color::DyeColor;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::ShulkerEntityData;
use steel_registry::{
    level_events, sound_events, vanilla_attributes, vanilla_blocks, vanilla_entities,
    vanilla_game_events,
};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, Direction, Downcast as _, DowncastType, DowncastTypeKey, WorldAabb};

use crate::entity::ai::control::rotate_towards;
use crate::entity::ai::goal::{HurtByTargetGoal, LookAtPlayerGoal, RandomLookAroundGoal};
use crate::entity::attribute::{AttributeModifier, AttributeModifierOperation};
use crate::entity::damage::DamageSource;
use crate::entity::spawn::create_entity_instance;
use crate::entity::{
    Enemy, Entity, EntityBase, EntityBaseLoad, EntityEventSource as _, EntityMoveError,
    EntityMovementEmission, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule,
    start_riding_entities, sync_dirty_mob_effects,
};
use crate::physics::{MoveResult, MoverType, WorldCollisionProvider};
use crate::world::game_event::GameEventContext;
use crate::world::{LevelReader as _, World};

mod goals;
#[cfg(test)]
mod tests;

use goals::{ShulkerAttackGoal, ShulkerNearestAttackGoal, ShulkerPeekGoal};

/// Vanilla `NO_COLOR` / `DEFAULT_COLOR`.
const NO_COLOR: i8 = 16;
/// Vanilla `PEEK_PER_TICK`.
const PEEK_PER_TICK: f32 = 0.05;
/// Vanilla `MAX_SCALE`.
const MAX_SCALE: f32 = 3.0;
/// Vanilla `MAX_TELEPORT_DISTANCE`.
const MAX_TELEPORT_DISTANCE: i32 = 8;
/// Vanilla `OTHER_SHULKER_SCAN_RADIUS`.
const OTHER_SHULKER_SCAN_RADIUS: f64 = 8.0;
/// Vanilla `OTHER_SHULKER_LIMIT`.
const OTHER_SHULKER_LIMIT: f32 = 5.0;
/// Vanilla `COVERED_ARMOR_MODIFIER`: +20 armor while closed.
const COVERED_ARMOR: f64 = 20.0;
/// Vanilla `AbstractGolem.getAmbientSoundInterval`.
const GOLEM_AMBIENT_SOUND_INTERVAL: i32 = 120;
/// Vanilla `Direction.values()` order, which `findAttachableSurface` scans.
const DIRECTIONS: [Direction; 6] = [
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
    Direction::West,
    Direction::East,
];

const fn covered_armor_modifier_id() -> steel_utils::Identifier {
    steel_utils::Identifier::vanilla_static("covered")
}

/// The server-side lid animation state.
#[derive(Debug, Clone, Copy, Default)]
struct PeekState {
    current_peek_amount_old: f32,
    current_peek_amount: f32,
}

/// A shulker.
#[entity_behavior(class = "Shulker", spawn_rule)]
pub struct ShulkerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<ShulkerEntityData>,
    peek: SyncMutex<PeekState>,
}

// SAFETY: The owner-scoped type key uniquely identifies ShulkerEntity.
unsafe impl DowncastType for ShulkerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/shulker");
}

impl ShulkerEntity {
    /// Vanilla `SpawnPlacements` predicate: `Mob.checkMobSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Creates a new shulker, snapped to its block like vanilla `setPos`.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, snap_to_block(position), entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved shulker.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        let shulker = Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        );
        // Vanilla `Entity.load` re-applies the position through `setPos`.
        shulker
            .base
            .set_position_local(snap_to_block(shulker.position()));
        shulker.refresh_bounding_box();
        shulker
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        mob_base.set_xp_reward(5);
        let mut entity_data = ShulkerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(
                1,
                LookAtPlayerGoal::new_with_probability_and_horizontal(8.0, 0.02, true),
            );
            goal_selector.add_goal(4, ShulkerAttackGoal::new());
            goal_selector.add_goal(7, ShulkerPeekGoal::new());
            goal_selector.add_goal(8, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                HurtByTargetGoal::new()
                    .with_ignored_damage_types([Self::TYPE_KEY])
                    .set_alert_others([]),
            );
            target_selector.add_goal(2, ShulkerNearestAttackGoal::new());
            // Vanilla `ShulkerDefenseAttackGoal` (priority 3) only runs for a
            // shulker on a scoreboard team; Steel has no teams yet.
        }

        let shulker = Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            peek: SyncMutex::new(PeekState::default()),
        };
        shulker.refresh_bounding_box();
        shulker
    }

    /// The block face the shulker is attached to.
    #[must_use]
    pub fn attach_face(&self) -> Direction {
        *self.entity_data.lock().attach_face.get()
    }

    /// Vanilla `setAttachFace`, plus the client-side box update from
    /// `onSyncedDataUpdated`.
    pub(crate) fn set_attach_face(&self, face: Direction) {
        self.entity_data.lock().attach_face.set(face);
        self.refresh_bounding_box();
    }

    /// Vanilla `getRawPeekAmount`: 0 (closed) to 100 (fully open).
    #[must_use]
    pub fn raw_peek_amount(&self) -> i32 {
        i32::from(*self.entity_data.lock().peek.get())
    }

    /// Vanilla `isClosed`.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.raw_peek_amount() == 0
    }

    /// Vanilla `setRawPeekAmount`: closing adds 20 armor, both play a sound.
    pub(crate) fn set_raw_peek_amount(&self, amount: i32) {
        {
            let mut attributes = self.attributes().lock();
            attributes.remove_modifier(vanilla_attributes::ARMOR, &covered_armor_modifier_id());
            if amount == 0 {
                attributes.set_modifier(
                    vanilla_attributes::ARMOR,
                    AttributeModifier {
                        id: covered_armor_modifier_id(),
                        amount: COVERED_ARMOR,
                        operation: AttributeModifierOperation::AddValue,
                    },
                    true,
                );
            }
        }
        if amount == 0 {
            self.play_sound(&sound_events::ENTITY_SHULKER_CLOSE, 1.0, 1.0);
            self.game_event_with_source_entity(&vanilla_game_events::CONTAINER_CLOSE, None);
        } else {
            self.play_sound(&sound_events::ENTITY_SHULKER_OPEN, 1.0, 1.0);
            self.game_event_with_source_entity(&vanilla_game_events::CONTAINER_OPEN, None);
        }
        self.set_peek_data(amount);
    }

    fn set_peek_data(&self, amount: i32) {
        self.entity_data.lock().peek.set(amount as i8);
    }

    /// The lid opening the server simulates, from 0.0 to 1.0.
    #[must_use]
    pub fn current_peek_amount(&self) -> f32 {
        self.peek.lock().current_peek_amount
    }

    /// The dye color, or `None` for the default purple.
    #[must_use]
    pub fn color(&self) -> Option<DyeColor> {
        let color = *self.entity_data.lock().color.get();
        (color != NO_COLOR && (0..=15).contains(&color)).then(|| DyeColor::by_id(i32::from(color)))
    }

    /// Vanilla `setVariant`.
    pub fn set_color(&self, color: Option<DyeColor>) {
        let id = color.map_or(NO_COLOR, |color| color.id() as i8);
        self.entity_data.lock().color.set(id);
    }

    fn refresh_bounding_box(&self) {
        self.base
            .set_bounding_box(self.make_bounding_box_at(self.position()));
    }

    /// Vanilla `updatePeekAmount`: ease the lid toward the synced amount.
    fn update_peek_amount(&self) -> bool {
        let target_peek_amount = self.raw_peek_amount() as f32 * 0.01;
        let mut peek = self.peek.lock();
        peek.current_peek_amount_old = peek.current_peek_amount;
        #[expect(clippy::float_cmp, reason = "vanilla compares the eased value exactly")]
        if peek.current_peek_amount == target_peek_amount {
            return false;
        }
        peek.current_peek_amount = if peek.current_peek_amount > target_peek_amount {
            (peek.current_peek_amount - PEEK_PER_TICK).clamp(target_peek_amount, 1.0)
        } else {
            (peek.current_peek_amount + PEEK_PER_TICK).clamp(0.0, target_peek_amount)
        };
        true
    }

    /// Vanilla `onPeekAmountChange`: grow the box and shove whatever the lid
    /// opens into.
    fn on_peek_amount_change(&self) {
        self.refresh_dimensions();
        self.refresh_bounding_box();
        let (old_peek, peek) = {
            let state = self.peek.lock();
            (state.current_peek_amount_old, state.current_peek_amount)
        };
        let peek_now = physical_peek(peek);
        let peek_old = physical_peek(old_peek);
        let direction = self.attach_face().opposite();
        let scale = LivingEntity::get_scale(self);
        let push = (peek_now - peek_old) * scale;
        if push <= 0.0 {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let search = progress_delta_aabb(scale, direction, peek_old, peek_now, self.position());
        let (step_x, step_y, step_z) = direction.offset();
        let delta = DVec3::new(
            f64::from(push * step_x as f32),
            f64::from(push * step_y as f32),
            f64::from(push * step_z as f32),
        );
        for entity in world.get_entities_in_aabb_matching(&search, |entity| {
            entity.id() != self.id()
                && !entity.is_spectator()
                && !entity.is_passenger_of_same_vehicle(self)
        }) {
            if entity.downcast_ref::<Self>().is_none() && !entity.no_physics() {
                let _ = Arc::clone(&entity).move_entity(MoverType::Shulker, delta);
            }
        }
    }

    /// Vanilla `canStayAt`.
    fn can_stay_at(&self, target: BlockPos, face: Direction) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        if self.is_position_blocked(&world, target) {
            return false;
        }
        let opposite_face = face.opposite();
        let collision = WorldCollisionProvider::for_entity(&world, self);
        if !collision.loaded_and_entity_can_stand_on_face(face.relative(target), opposite_face) {
            return false;
        }
        let fully_opened = progress_aabb(
            LivingEntity::get_scale(self),
            opposite_face,
            1.0,
            block_bottom_center(target),
        )
        .deflate(1.0e-6);
        !collision.has_entity_context_collision(
            fully_opened,
            self.position().y,
            self.is_descending(),
        )
    }

    /// Vanilla `isPositionBlocked`.
    fn is_position_blocked(&self, world: &World, target: BlockPos) -> bool {
        let state = world.get_block_state(target);
        if state.is_air() {
            return false;
        }
        let moving_piston_here =
            state.get_block() == &vanilla_blocks::MOVING_PISTON && target == self.block_position();
        !moving_piston_here
    }

    /// Vanilla `findAttachableSurface`.
    fn find_attachable_surface(&self, target: BlockPos) -> Option<Direction> {
        DIRECTIONS
            .into_iter()
            .find(|&direction| self.can_stay_at(target, direction))
    }

    /// Vanilla `findNewAttachment`.
    fn find_new_attachment(&self) {
        if let Some(direction) = self.find_attachable_surface(self.block_position()) {
            self.set_attach_face(direction);
        } else {
            self.teleport_somewhere();
        }
    }

    /// Vanilla `teleportSomewhere`: up to 5 tries within 8 blocks.
    pub(crate) fn teleport_somewhere(&self) -> bool {
        if self.is_no_ai() || !LivingEntity::is_alive(self) {
            return false;
        }
        let Some(world) = self.level() else {
            return false;
        };
        let old_pos = self.block_position();
        let range = -MAX_TELEPORT_DISTANCE..=MAX_TELEPORT_DISTANCE;
        for _ in 0..5 {
            let target = old_pos.offset(
                rand::random_range(range.clone()),
                rand::random_range(range.clone()),
                rand::random_range(range.clone()),
            );
            let collision = WorldCollisionProvider::for_entity(&world, self);
            if target.y() <= world.min_y()
                || !world.get_block_state(target).is_air()
                || !world.world_border_snapshot().is_block_within_bounds(target)
                || collision.has_entity_context_collision(
                    unit_block_box(target).deflate(1.0e-6),
                    self.position().y,
                    self.is_descending(),
                )
            {
                continue;
            }
            let Some(direction) = self.find_attachable_surface(target) else {
                continue;
            };
            if world
                .get_block_state(direction.relative(target))
                .get_block()
                .has_tag(&BlockTag::SHULKER_DOES_NOT_TELEPORT_TO)
            {
                continue;
            }
            self.unride();
            self.set_attach_face(direction);
            self.play_sound(&sound_events::ENTITY_SHULKER_TELEPORT, 1.0, 1.0);
            if let Err(error) = self.try_set_position(block_bottom_center(target)) {
                log::debug!("shulker {} could not teleport: {error}", self.id());
                return false;
            }
            world.game_event(
                &vanilla_game_events::TELEPORT,
                old_pos,
                &GameEventContext::new(Some(self), None),
            );
            world.level_event(
                level_events::PARTICLES_SHULKER_TELEPORT,
                old_pos,
                pack_difference_in_position(old_pos, self.block_position(), 8, 8, 8),
                None,
            );
            self.set_peek_data(0);
            self.set_target(None);
            return true;
        }
        false
    }

    /// Vanilla `unRide`: leave the vehicle and drop the passengers.
    fn unride(&self) {
        self.stop_riding();
        for passenger in self.passengers().into_iter().rev() {
            passenger.stop_riding();
        }
    }

    /// Vanilla `hitByShulkerBullet`: teleport away and maybe duplicate.
    fn hit_by_shulker_bullet(&self) {
        let old_position = self.position();
        let old_aabb = self.bounding_box();
        if self.is_closed() || !self.teleport_somewhere() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let shulker_count = world
            .get_entities_in_aabb_matching(&old_aabb.inflate(OTHER_SHULKER_SCAN_RADIUS), |entity| {
                entity.entity_type() == &vanilla_entities::SHULKER && entity.is_alive()
            })
            .len();
        let failure_chance = (shulker_count as f32 - 1.0) / OTHER_SHULKER_LIMIT;
        if rand::random::<f32>() < failure_chance {
            return;
        }
        let Ok(baby) = create_entity_instance(&world, &vanilla_entities::SHULKER, old_position)
        else {
            return;
        };
        if let Some(shulker) = baby.downcast_ref::<Self>() {
            shulker.set_color(self.color());
        }
        let _ = world.try_add_entity(baby);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        if let Some(display) = sync_dirty_mob_effects(&self.living_base, &self.entity_data) {
            self.entity_data
                .set_base_glowing_flag(self.has_glowing_tag() || display.glowing);
        }
    }

    /// Vanilla `ShulkerLookControl.getYRotD`: the yaw on the attach plane
    /// toward the wanted position.
    fn look_y_rot_d(&self, wanted: DVec3) -> Option<f32> {
        let attach_face = self.attach_face().opposite();
        let forward = rotated_forward(attach_face);
        let (up_x, up_y, up_z) = attach_face.offset();
        let up = Vec3::new(up_x as f32, up_y as f32, up_z as f32);
        let right = up.cross(forward);
        let position = self.position();
        let out = Vec3::new(
            (wanted.x - position.x) as f32,
            (wanted.y - self.get_eye_y()) as f32,
            (wanted.z - position.z) as f32,
        );
        let delta_right = right.dot(out);
        let delta_forward = forward.dot(out);
        (delta_right.abs() > 1.0e-5 || delta_forward.abs() > 1.0e-5).then(|| {
            (f64::from(-delta_right).atan2(f64::from(delta_forward)) * 180.0 / f64::from(PI)) as f32
        })
    }
}

/// Vanilla `Shulker.setPos`: a shulker not riding sits at its block's
/// bottom center.
fn snap_to_block(position: DVec3) -> DVec3 {
    DVec3::new(
        position.x.floor() + 0.5,
        (position.y + 0.5).floor(),
        position.z.floor() + 0.5,
    )
}

fn block_bottom_center(pos: BlockPos) -> DVec3 {
    DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()),
        f64::from(pos.z()) + 0.5,
    )
}

fn unit_block_box(pos: BlockPos) -> WorldAabb {
    let min = DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()));
    WorldAabb::new(min.x, min.y, min.z, min.x + 1.0, min.y + 1.0, min.z + 1.0)
}

/// Vanilla `getPhysicalPeek`.
fn physical_peek(amount: f32) -> f32 {
    0.5 - trig::sin(f64::from((0.5 + amount) * PI)) * 0.5
}

/// Vanilla `getProgressAabb`.
pub(crate) fn progress_aabb(
    size: f32,
    direction: Direction,
    progress_to: f32,
    position: DVec3,
) -> WorldAabb {
    progress_delta_aabb(size, direction, -1.0, progress_to, position)
}

/// Vanilla `getProgressDeltaAabb`.
pub(crate) fn progress_delta_aabb(
    size: f32,
    direction: Direction,
    progress_from: f32,
    progress_to: f32,
    position: DVec3,
) -> WorldAabb {
    let half = f64::from(-size * 0.5);
    let size_d = f64::from(size);
    let bounds = WorldAabb::new(half, 0.0, half, -half, size_d, -half);
    let max_movement = f64::from(progress_from.max(progress_to));
    let min_movement = f64::from(progress_from.min(progress_to));
    let (step_x, step_y, step_z) = direction.offset();
    let step = DVec3::new(f64::from(step_x), f64::from(step_y), f64::from(step_z));
    bounds
        .expand_towards(step * max_movement * size_d)
        .contract(-step * (1.0 + min_movement) * size_d)
        .translate(position)
}

/// Vanilla `BlockUtil.packDifferenceInPosition` for in-range offsets.
const fn pack_difference_in_position(
    pos: BlockPos,
    test_pos: BlockPos,
    x_radius: i32,
    y_radius: i32,
    z_radius: i32,
) -> i32 {
    let x_diff = test_pos.x() - pos.x();
    let y_diff = test_pos.y() - pos.y();
    let z_diff = test_pos.z() - pos.z();
    ((x_diff + x_radius) & 0xFF) << 16
        | ((y_diff + y_radius) & 0xFF) << 8
        | (z_diff + z_radius) & 0xFF
}

/// `Direction.SOUTH.getUnitVec3f()` turned by vanilla `Direction.getRotation()`.
const fn rotated_forward(direction: Direction) -> Vec3 {
    match direction {
        Direction::Down => Vec3::new(0.0, 0.0, -1.0),
        Direction::Up => Vec3::new(0.0, 0.0, 1.0),
        Direction::North | Direction::South | Direction::West | Direction::East => {
            Vec3::new(0.0, -1.0, 0.0)
        }
    }
}

impl Enemy for ShulkerEntity {}

impl Entity for ShulkerEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn as_enemy(&self) -> Option<&dyn Enemy> {
        Some(self)
    }

    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        if !self.is_passenger() && !self.can_stay_at(self.block_position(), self.attach_face()) {
            self.find_new_attachment();
        }
        if self.update_peek_amount() {
            self.on_peek_amount_change();
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::None
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    fn make_bounding_box_at(&self, position: DVec3) -> WorldAabb {
        let physical_peek = physical_peek(self.peek.lock().current_peek_amount);
        let direction = self.attach_face().opposite();
        progress_aabb(
            LivingEntity::get_scale(self),
            direction,
            physical_peek,
            position,
        )
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        // Vanilla `getDefaultDimensions`: the open lid of a floor shulker
        // makes it taller.
        let peek = self.peek.lock().current_peek_amount;
        let mut dimensions = self.entity_type.dimensions;
        if self.attach_face() == Direction::Down && peek > 0.0 {
            let height_factor = 1.0 + peek;
            dimensions = EntityDimensions {
                height: dimensions.height * height_factor,
                eye_height: dimensions.eye_height * height_factor,
                attachments: dimensions.attachments.scale(1.0, height_factor),
                ..dimensions
            };
        }
        dimensions.scale(self.get_age_scale() * LivingEntity::get_scale(self))
    }

    fn try_set_position(&self, pos: DVec3) -> Result<(), EntityMoveError> {
        let old_pos = self.block_position();
        let pos = if self.is_passenger() {
            pos
        } else {
            snap_to_block(pos)
        };
        self.try_set_position_base(pos)?;
        self.refresh_bounding_box();
        if self.tick_count() != 0 && self.block_position() != old_pos {
            self.set_peek_data(0);
        }
        Ok(())
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        if mover_type == MoverType::ShulkerBox {
            self.teleport_somewhere();
            return None;
        }
        self.default_move_entity(mover_type, delta)
    }

    fn velocity(&self) -> DVec3 {
        DVec3::ZERO
    }

    fn set_velocity(&self, _velocity: DVec3) {}

    /// Vanilla `push(Entity)`: nothing shoves a shulker off its block.
    fn push_entity(&self, _entity: &dyn Entity) {}

    fn can_be_collided_with(&self, _other: Option<&dyn Entity>) -> bool {
        LivingEntity::is_alive(self)
    }

    fn start_riding(&self, entity_to_ride: &SharedEntity) -> bool {
        self.set_attach_face(Direction::Down);
        let Some(world) = self.level() else {
            return false;
        };
        let Some(passenger) = world.get_entity_by_id(self.id()) else {
            return false;
        };
        start_riding_entities(&passenger, entity_to_ride)
    }

    fn stop_riding(&self) {
        self.base().stop_riding();
        self.set_y_body_rot(0.0);
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("AttachFace", self.attach_face().get_3d_data_value() as i8);
        nbt.insert("Peek", *self.entity_data.lock().peek.get());
        nbt.insert("Color", *self.entity_data.lock().color.get());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        let face = nbt
            .byte("AttachFace")
            .map(i32::from)
            .filter(|id| (0..6).contains(id))
            .map_or(Direction::Down, Direction::from_3d_data_value);
        self.set_attach_face(face);
        self.set_peek_data(nbt.byte("Peek").map_or(0, i32::from));
        self.entity_data
            .lock()
            .color
            .set(nbt.byte("Color").unwrap_or(NO_COLOR));
    }
}

impl LivingEntity for ShulkerEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn get_scale(&self) -> f32 {
        // Vanilla `sanitizeScale`: capped at 3.
        let scale = self
            .attributes()
            .lock()
            .get_value(vanilla_attributes::SCALE)
            .unwrap_or(1.0) as f32;
        scale.min(MAX_SCALE)
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_closed() {
            &sound_events::ENTITY_SHULKER_HURT_CLOSED
        } else {
            &sound_events::ENTITY_SHULKER_HURT
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SHULKER_DEATH)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_closed()
            && source
                .direct_entity()
                .is_some_and(|entity| entity.is_abstract_arrow())
        {
            return false;
        }
        if !self.default_hurt_server(world, source, amount) {
            return false;
        }
        if self.get_health() < self.get_max_health() * 0.5 && rand::random_range(0..4) == 0 {
            self.teleport_somewhere();
        } else if source.is(&DamageTypeTag::IS_PROJECTILE)
            && source
                .direct_entity()
                .is_some_and(|entity| entity.entity_type() == &vanilla_entities::SHULKER_BULLET)
        {
            self.hit_by_shulker_bullet();
        }
        true
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Mob::mob_ai_step(self, entity)
    }
}

impl Mob for ShulkerEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    /// Vanilla `ShulkerLookControl`: yaw on the attach plane, no pitch, and no
    /// clamp to the body.
    fn tick_look_control(&self) {
        let look_control = {
            let mut controls = self.mob_base().controls().lock();
            let look_control = controls.look_control;
            controls.look_control.tick_cooldown();
            look_control
        };
        let mut rotation = self.rotation();
        rotation.1 = 0.0;
        if look_control.is_looking_at_target() {
            if let Some(y_rot_d) = self.look_y_rot_d(look_control.wanted_position()) {
                self.set_y_head_rot(rotate_towards(
                    self.y_head_rot(),
                    y_rot_d,
                    look_control.y_max_rot_speed(),
                ));
            }
        } else {
            self.set_y_head_rot(rotate_towards(self.y_head_rot(), self.y_body_rot(), 10.0));
        }
        self.set_rotation(rotation);
    }

    /// Vanilla `ShulkerBodyRotationControl`: the body never turns.
    fn tick_body_rotation_control(&self) {}

    fn max_head_x_rot(&self) -> f32 {
        180.0
    }

    fn max_head_y_rot(&self) -> f32 {
        180.0
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SHULKER_AMBIENT)
    }

    fn play_ambient_sound(&self) {
        if !self.is_closed() {
            self.make_sound(self.ambient_sound());
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        GOLEM_AMBIENT_SOUND_INTERVAL
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let pitch = self.rotation().1;
        self.set_rotation((0.0, pitch));
        self.set_y_head_rot(0.0);
        self.set_old_position_to_current();
        self.base.set_old_rotation_to_current();
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for ShulkerEntity {}
