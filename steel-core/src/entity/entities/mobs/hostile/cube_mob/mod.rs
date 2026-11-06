//! Vanilla `AbstractCubeMob`: the bouncing cubes (slime and magma cube).
//!
//! The client-side squish animation (`squish`, `targetSquish`) and the landing
//! particles only feed rendering, so the server keeps neither.

mod goals;
/// Magma cube.
pub mod magma_cube;
/// Slime.
pub mod slime;

use std::f32::consts::PI;
use std::mem;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_registry::entity_type::EntityDimensions;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{sound_events, vanilla_attributes, vanilla_damage_types, vanilla_entities};
use steel_utils::locks::SyncMutex;

pub(crate) use goals::{
    CubeMobAttackGoal, CubeMobFloatGoal, CubeMobKeepOnJumpingGoal, CubeMobRandomDirectionGoal,
};
pub use magma_cube::MagmaCubeEntity;
pub use slime::SlimeEntity;

use crate::enchantment_helper::{self, EnchantmentPostAttackContext};
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::damage::DamageSource;
use crate::entity::mob::rotlerp;
use crate::entity::{
    AgeableMob, AgeableMobGroupData, ConversionParams, ConversionType, Entity, EntitySpawnReason,
    LivingEntity, LivingTravelInput, Mob, SharedEntity, SpawnGroupData,
};
use crate::inventory::equipment::EquipmentSlot;
use crate::world::World;

/// Smallest cube size.
pub const MIN_SIZE: i32 = 1;
/// Largest cube size `/summon` or NBT can set.
pub const MAX_SIZE: i32 = 127;

const SIZE_TAG: &str = "Size";
const WAS_ON_GROUND_TAG: &str = "wasOnGround";
const SPLIT_SPAWN_Y_OFFSET: f64 = 0.5;
const MOVE_CONTROL_MAX_TURN: f32 = 90.0;

/// Vanilla `CubeMobMoveControl` fields beyond the shared move control.
#[derive(Debug, Clone, Copy)]
struct CubeMoveState {
    y_rot: f32,
    jump_delay: i32,
    is_aggressive: bool,
}

/// Per-entity `AbstractCubeMob` state.
#[derive(Debug)]
pub struct CubeMobBase {
    was_on_ground: SyncMutex<bool>,
    move_state: SyncMutex<CubeMoveState>,
}

impl CubeMobBase {
    /// Creates the state for a cube whose body faces `y_rot`.
    #[must_use]
    pub fn new(y_rot: f32) -> Self {
        Self {
            was_on_ground: SyncMutex::new(false),
            // Vanilla seeds the move control with this degree/radian mix-up.
            move_state: SyncMutex::new(CubeMoveState {
                y_rot: 180.0 * y_rot / PI,
                jump_delay: 0,
                is_aggressive: false,
            }),
        }
    }
}

/// Behavior shared by slimes and magma cubes.
pub trait AbstractCubeMob: AgeableMob {
    /// Returns the shared cube state.
    fn cube_mob_base(&self) -> &CubeMobBase;

    /// Returns the synced `ID_SIZE`.
    fn get_size(&self) -> i32;

    /// Writes the synced `ID_SIZE`.
    fn set_synced_size(&self, size: i32);

    /// Sound played when the cube jumps.
    fn jump_sound(&self) -> SoundEventRef;

    /// Sound played when the cube lands.
    fn squish_sound(&self) -> SoundEventRef;

    /// Applies the subclass part of vanilla `setSize`, after the shared part.
    /// `requested_size` is the unclamped argument.
    fn apply_size_attributes(&self, actual_size: i32, requested_size: i32);

    /// Vanilla `setSize`: clamps the size and rescales hitbox, health and speed.
    fn set_size(&self, size: i32, update_health: bool) {
        let actual_size = size.clamp(MIN_SIZE, MAX_SIZE);
        if self.get_size() != actual_size {
            self.set_synced_size(actual_size);
            self.on_size_synced();
        }
        self.refresh_dimensions();
        {
            let mut attributes = self.attributes().lock();
            attributes.set_base_value(
                vanilla_attributes::MAX_HEALTH,
                f64::from(actual_size * actual_size),
            );
            attributes.set_base_value(
                vanilla_attributes::MOVEMENT_SPEED,
                f64::from(0.2_f32 + 0.1_f32 * actual_size as f32),
            );
        }
        if update_health {
            self.set_health(self.get_max_health());
        }
        self.apply_size_attributes(actual_size, size);
    }

    /// Vanilla `onSyncedDataUpdated(ID_SIZE)` on the server.
    fn on_size_synced(&self) {
        self.refresh_dimensions();
        let y_head_rot = self.y_head_rot();
        self.set_rotation((y_head_rot, self.rotation().1));
        self.set_y_body_rot(y_head_rot);
        if self.is_in_water() && rand::random_range(0..20) == 0 {
            self.do_water_splash_effect();
        }
    }

    /// Returns whether this is the smallest size, which splits no further.
    fn is_tiny(&self) -> bool {
        self.get_size() <= 1
    }

    /// Vanilla `getDefaultDimensions`: the type's dimensions times the size.
    fn cube_dimensions(&self) -> EntityDimensions {
        self.entity_type()
            .dimensions
            .scale(self.get_size() as f32)
            .scale(LivingEntity::get_scale(self))
    }

    /// Vanilla `refreshDimensions`: resizing never moves the cube.
    fn refresh_cube_dimensions(&self) {
        let position = self.position();
        self.default_refresh_dimensions();
        if self.position() != position
            && let Err(error) = self.try_set_position(position)
        {
            log::debug!("cube mob could not keep its position after resizing: {error}");
        }
    }

    /// Ticks between jumps.
    fn get_jump_delay(&self) -> i32 {
        rand::random_range(0..20) + 10
    }

    /// Vanilla `tick` after `super.tick()`: the landing squish sound.
    fn tick_landing(&self) {
        let on_ground = self.on_ground();
        let was_on_ground =
            mem::replace(&mut *self.cube_mob_base().was_on_ground.lock(), on_ground);
        if on_ground && !was_on_ground {
            let pitch = ((rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0) / 0.8;
            self.play_sound(self.squish_sound(), self.sound_volume(), pitch);
        }
    }

    /// Volume of every cube sound.
    fn cube_sound_volume(&self) -> f32 {
        0.4 * self.get_size() as f32
    }

    /// Vanilla `getSoundPitch`, used for the jump sound.
    fn cube_sound_pitch(&self) -> f32 {
        let pitch_adjuster = if self.is_tiny() { 1.4 } else { 0.8 };
        ((rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0) * pitch_adjuster
    }

    /// Whether landing a jump plays the jump sound.
    fn do_play_jump_sound(&self) -> bool {
        self.get_size() > 0
    }

    /// Whether touching the cube hurts.
    fn can_deal_damage(&self) -> bool {
        !self.is_tiny() && self.is_effective_ai()
    }

    /// Damage the cube deals on contact.
    fn cube_attack_damage(&self) -> f32 {
        self.attributes()
            .lock()
            .required_value(vanilla_attributes::ATTACK_DAMAGE) as f32
    }

    /// How many smaller cubes a dead cube splits into.
    fn get_split_count(&self) -> i32 {
        2 + rand::random_range(0..3)
    }

    /// Vanilla `jumpFromGround`: the jump ignores sprinting and jump boost bonuses
    /// beyond the jump power.
    fn cube_jump_from_ground(&self, extra_power: f32) {
        let movement = self.velocity();
        self.set_velocity(DVec3::new(
            movement.x,
            f64::from(self.get_jump_power() + extra_power),
            movement.z,
        ));
        self.mark_velocity_sync();
    }

    /// Vanilla `dealDamage`: contact damage against a player or iron golem.
    fn deal_damage(&self, world: &World, attacker: &SharedEntity, target: &dyn LivingEntity) {
        // TODO: vanilla also checks `doTeamsAllowDamage` once teams carry friendly fire.
        if !LivingEntity::is_alive(self)
            || !self.is_within_melee_attack_range(target)
            || !self.has_line_of_sight(target.as_entity_event_source())
        {
            return;
        }
        let damage_source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
            .with_causing_entity(Arc::clone(attacker))
            .with_direct_entity(Arc::clone(attacker));
        if !target.hurt(world, &damage_source, self.cube_attack_damage()) {
            return;
        }
        let pitch = (rand::random::<f32>() - rand::random::<f32>()) * 0.2 + 1.0;
        self.play_sound(&sound_events::ENTITY_SLIME_ATTACK, 1.0, pitch);
        let Some(target_entity) = world.get_entity_by_id(target.id()) else {
            return;
        };
        let weapon = self.equipment_in_slot(EquipmentSlot::MainHand);
        let context = EnchantmentPostAttackContext::new(&target_entity, &damage_source);
        enchantment_helper::do_post_attack_effects_with_item_source(
            world,
            target_entity.as_ref(),
            &weapon,
            &context,
        );
    }

    /// Vanilla `push(Entity)`: pushing an iron golem hurts it.
    fn cube_push(&self, entity: &dyn Entity) {
        self.default_push_entity(entity);
        if entity.entity_type() != &vanilla_entities::IRON_GOLEM || !self.can_deal_damage() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let (Some(attacker), Some(target)) =
            (world.get_entity_by_id(self.id()), entity.as_living_entity())
        else {
            return;
        };
        self.deal_damage(&world, &attacker, target);
    }

    /// Vanilla `playerTouch`.
    fn cube_player_touch(&self, attacker: &SharedEntity, player: &dyn LivingEntity) {
        if !self.can_deal_damage() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        self.deal_damage(&world, attacker, player);
    }

    /// Vanilla `remove`: a dead cube bigger than tiny splits into halves.
    fn split_on_death(&self)
    where
        Self: Sized,
    {
        let size = self.get_size();
        if size <= 1 || !self.is_dead_or_dying() {
            return;
        }
        let x_z_offset = self.dimensions_for_pose(self.pose()).width / 2.0;
        let half_size = size / 2;
        let position = self.position();
        for i in 0..self.get_split_count() {
            let xd = (f64::from(i % 2) - 0.5) * f64::from(x_z_offset);
            let zd = (f64::from(i / 2) - 0.5) * f64::from(x_z_offset);
            let params = ConversionParams {
                conversion_type: ConversionType::SplitOnDeath,
                keep_equipment: false,
                preserve_can_pick_up_loot: false,
            };
            self.convert_to(
                self.entity_type(),
                params,
                Some(&|converted: &dyn Mob| {
                    let Some(cube) = converted.as_cube_mob() else {
                        return;
                    };
                    cube.set_size(half_size, true);
                    cube.snap_to(
                        DVec3::new(
                            position.x + xd,
                            position.y + SPLIT_SPAWN_Y_OFFSET,
                            position.z + zd,
                        ),
                        rand::random::<f32>() * 360.0,
                        0.0,
                    );
                }),
            );
        }
    }

    /// Vanilla `setSpawnSize`: 1, 2 or 4, biased up by regional difficulty.
    fn set_spawn_size(&self, world: &World) {
        let difficulty = world.current_difficulty_at(self.block_position());
        let mut size_scale = rand::random_range(0..3);
        if size_scale < 2 && rand::random::<f32>() < 0.5 * difficulty.special_multiplier() {
            size_scale += 1;
        }
        self.set_size(1 << size_scale, true);
    }

    /// Vanilla `finalizeSpawn` for slimes and magma cubes: never a baby, then a
    /// random size.
    fn finalize_spawn_cube_mob(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let group_data = group_data.or(Some(SpawnGroupData::AgeableMob(
            AgeableMobGroupData::with_should_spawn_baby(false),
        )));
        let data = self.finalize_spawn_ageable_mob(world, spawn_reason, group_data);
        self.set_spawn_size(world);
        data
    }

    /// Vanilla `CubeMobMoveControl.setDirection`.
    fn set_move_direction(&self, y_rot: f32, is_aggressive: bool) {
        let mut state = self.cube_mob_base().move_state.lock();
        state.y_rot = y_rot;
        state.is_aggressive = is_aggressive;
    }

    /// Vanilla `CubeMobMoveControl.setWantedMovement`.
    fn set_wanted_movement(&self, speed_modifier: f64) {
        self.mob_base()
            .controls()
            .lock()
            .move_control
            .set_wanted_movement(speed_modifier);
    }

    /// Vanilla `CubeMobMoveControl.tick`: hops toward the chosen direction.
    fn tick_cube_move_control(&self) {
        let wanted_y_rot = self.cube_mob_base().move_state.lock().y_rot;
        let y_rot = rotlerp(self.rotation().0, wanted_y_rot, MOVE_CONTROL_MAX_TURN);
        self.set_rotation((y_rot, self.rotation().1));
        self.set_y_head_rot(y_rot);
        self.set_y_body_rot(y_rot);

        let (operation, speed_modifier) = {
            let mut controls = self.mob_base().controls().lock();
            let move_control = controls.move_control;
            if move_control.operation() == MoveControlOperation::MoveTo {
                controls.move_control.set_wait();
            }
            (move_control.operation(), move_control.speed_modifier())
        };
        if operation != MoveControlOperation::MoveTo {
            let input = self.travel_input();
            self.set_travel_input(LivingTravelInput::new(
                input.sideways(),
                input.vertical(),
                0.0,
            ));
            return;
        }

        let speed = (speed_modifier
            * self
                .attributes()
                .lock()
                .required_value(vanilla_attributes::MOVEMENT_SPEED)) as f32;
        if !self.on_ground() {
            self.set_mob_speed(speed);
            return;
        }
        self.set_mob_speed(speed);
        let jump = {
            let mut state = self.cube_mob_base().move_state.lock();
            let jump = state.jump_delay <= 0;
            state.jump_delay -= 1;
            jump
        };
        if !jump {
            let input = self.travel_input();
            self.set_travel_input(LivingTravelInput::new(0.0, input.vertical(), 0.0));
            self.set_mob_speed(0.0);
            return;
        }

        let mut jump_delay = self.get_jump_delay();
        {
            let mut state = self.cube_mob_base().move_state.lock();
            if state.is_aggressive {
                jump_delay /= 3;
            }
            state.jump_delay = jump_delay;
        }
        self.jump_control_jump();
        if self.do_play_jump_sound() {
            self.play_sound(
                self.jump_sound(),
                self.sound_volume(),
                self.cube_sound_pitch(),
            );
        }
    }

    /// Saves `Size` and `wasOnGround`.
    fn save_cube_mob(&self, nbt: &mut NbtCompound) {
        nbt.insert(SIZE_TAG, self.get_size() - 1);
        nbt.insert(
            WAS_ON_GROUND_TAG,
            i8::from(*self.cube_mob_base().was_on_ground.lock()),
        );
    }

    /// Loads the size before the rest of the mob, as vanilla does.
    fn load_cube_mob_size(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.set_size(nbt.int(SIZE_TAG).unwrap_or(0) + 1, false);
    }

    /// Loads `wasOnGround` after the rest of the mob.
    fn load_cube_mob_was_on_ground(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        *self.cube_mob_base().was_on_ground.lock() =
            nbt.byte(WAS_ON_GROUND_TAG).is_some_and(|value| value != 0);
    }
}

#[cfg(test)]
mod tests;
