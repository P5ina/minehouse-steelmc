//! Vex (Vanilla `Vex`): the evoker's flying summon that phases through walls.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::VexEntityData;
use steel_registry::{sound_events, vanilla_attributes, vanilla_damage_types, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey};

use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{
    FloatGoal, HurtByTargetGoal, LookAtPlayerGoal, NearestAttackableTargetGoal,
    VexChargeAttackGoal, VexCopyOwnerTargetGoal, VexRandomMoveGoal,
};
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::mob::spawn_equipment::populate_default_equipment_enchantments;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntityReference, EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob,
    MobBase, Monster, PathfinderMob, SharedEntity, sync_dirty_mob_effects,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 3;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `Vex.TICKS_PER_FLAP`: `ceil(5π/4)`.
const TICKS_PER_FLAP: i32 = 4;
const FLAG_IS_CHARGING: i8 = 1;
const TAG_BOUND_POS: &str = "bound_pos";
const TAG_LIFE_TICKS: &str = "life_ticks";
const TAG_OWNER: &str = "owner";
/// Vanilla `Vex.tick`: once its life runs out a vex starves every second.
const STARVE_INTERVAL: i32 = 20;
const STARVE_DAMAGE: f32 = 1.0;
/// Vanilla `VexMoveControl`: velocity gain per tick toward the wanted position.
const MOVE_ACCELERATION: f64 = 0.05;

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

/// Server-only vex state.
struct VexState {
    owner: Option<EntityReference>,
    bound_origin: Option<BlockPos>,
    has_limited_life: bool,
    limited_life_ticks: i32,
}

/// A vex: a small flying spirit that charges at its owner's target.
#[entity_behavior(class = "Vex")]
pub struct VexEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<VexEntityData>,
    state: SyncMutex<VexState>,
}

// SAFETY: The owner-scoped type key uniquely identifies VexEntity.
unsafe impl DowncastType for VexEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/vex");
}

impl VexEntity {
    /// Creates a new vex.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved vex.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        mob_base.set_xp_reward(XP_REWARD);
        let mut entity_data = VexEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(4, VexChargeAttackGoal::new());
            goal_selector.add_goal(8, VexRandomMoveGoal::new());
            goal_selector.add_goal(9, LookAtPlayerGoal::new_with_probability(3.0, 1.0));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(8.0, 0.02, |target, _| {
                    target.as_mob().is_some()
                }),
            );

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                HurtByTargetGoal::new()
                    .with_ignored_damage_selector(is_raider)
                    .set_alert_others([]),
            );
            target_selector.add_goal(2, VexCopyOwnerTargetGoal::new());
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            state: SyncMutex::new(VexState {
                owner: None,
                bound_origin: None,
                has_limited_life: false,
                limited_life_ticks: 0,
            }),
        }
    }

    /// Vanilla `getOwner`: the live mob that summoned this vex.
    #[must_use]
    pub fn owner(&self) -> Option<SharedEntity> {
        let owner = self.state.lock().owner.clone()?;
        let world = self.level()?;
        owner
            .get_entity(&world)
            .filter(|entity| entity.as_living_entity().is_some())
    }

    /// Vanilla `getRootOwner`: follows owners of owners up to the first one
    /// that has no owner itself.
    #[must_use]
    pub fn root_owner(&self) -> Option<SharedEntity> {
        let mut owner = self.owner()?;
        let mut seen = vec![self.uuid()];
        while let Some(vex) = owner.downcast_ref::<Self>() {
            if seen.contains(&owner.uuid()) {
                return None;
            }
            seen.push(owner.uuid());
            let Some(next) = vex.owner() else {
                break;
            };
            owner = next;
        }
        Some(owner)
    }

    /// Vanilla `setOwner`.
    pub fn set_owner(&self, owner: &SharedEntity) {
        self.state.lock().owner = Some(EntityReference::from_entity(owner));
    }

    /// Vanilla `getBoundOrigin`.
    #[must_use]
    pub fn bound_origin(&self) -> Option<BlockPos> {
        self.state.lock().bound_origin
    }

    /// Vanilla `setBoundOrigin`.
    pub fn set_bound_origin(&self, bound_origin: Option<BlockPos>) {
        self.state.lock().bound_origin = bound_origin;
    }

    /// Vanilla `setLimitedLife`.
    pub fn set_limited_life(&self, life_ticks: i32) {
        let mut state = self.state.lock();
        state.has_limited_life = true;
        state.limited_life_ticks = life_ticks;
    }

    /// Vanilla `isCharging`.
    #[must_use]
    pub fn is_charging(&self) -> bool {
        *self.entity_data.lock().flags.get() & FLAG_IS_CHARGING != 0
    }

    /// Vanilla `setIsCharging`.
    pub fn set_is_charging(&self, charging: bool) {
        let mut entity_data = self.entity_data.lock();
        let flags = *entity_data.flags.get();
        let flags = if charging {
            flags | FLAG_IS_CHARGING
        } else {
            flags & !FLAG_IS_CHARGING
        };
        entity_data.flags.set(flags);
    }

    /// Vanilla `getMoveControl().hasWanted()`.
    #[must_use]
    pub fn move_control_has_wanted(&self) -> bool {
        self.mob_base.controls().lock().move_control.operation() == MoveControlOperation::MoveTo
    }

    /// Vanilla `moveControl.setWantedPosition`.
    pub fn set_wanted_position(&self, position: DVec3, speed_modifier: f64) {
        self.mob_base
            .controls()
            .lock()
            .move_control
            .set_wanted_position(position, speed_modifier);
    }

    /// Vanilla `Vex.tick` after `super.tick()`: the limited life runs out.
    fn tick_limited_life(&self) {
        let starve = {
            let mut state = self.state.lock();
            if !state.has_limited_life {
                return;
            }
            state.limited_life_ticks -= 1;
            if state.limited_life_ticks <= 0 {
                state.limited_life_ticks = STARVE_INTERVAL;
                true
            } else {
                false
            }
        };
        if starve && let Some(world) = self.level() {
            let source = DamageSource::environment(&vanilla_damage_types::STARVE);
            let _ = self.hurt(&world, &source, STARVE_DAMAGE);
        }
    }

    /// Vanilla `VexMoveControl.tick`: flies straight at the wanted position.
    fn tick_vex_move_control(&self) {
        let (operation, wanted, speed_modifier) = {
            let controls = self.mob_base.controls().lock();
            let move_control = controls.move_control;
            (
                move_control.operation(),
                move_control.wanted_position(),
                move_control.speed_modifier(),
            )
        };
        if operation != MoveControlOperation::MoveTo {
            return;
        }

        let delta = wanted - self.position();
        let delta_length = delta.length();
        if delta_length < self.bounding_box().size() {
            self.mob_base.controls().lock().move_control.set_wait();
            self.set_velocity(self.velocity() * 0.5);
            return;
        }

        self.set_velocity(
            self.velocity() + delta * (speed_modifier * MOVE_ACCELERATION / delta_length),
        );
        let facing = match self.target() {
            None => {
                let movement = self.velocity();
                DVec3::new(movement.x, 0.0, movement.z)
            }
            Some(target) => target.position() - self.position(),
        };
        let y_rot = -(facing.x.atan2(facing.z) as f32).to_degrees();
        self.set_rotation((y_rot, self.rotation().1));
        self.set_y_body_rot(y_rot);
    }
}

impl DefaultEntityPush for VexEntity {}

impl Entity for VexEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::All
    }

    fn tick(self: Arc<Self>) {
        self.set_no_physics(true);
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
        self.set_no_physics(false);
        self.set_no_gravity(true);
        self.tick_limited_life();
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn is_flapping(&self) -> bool {
        self.tick_count() % TICKS_PER_FLAP == 0
    }

    fn is_affected_by_blocks(&self) -> bool {
        !self.is_removed()
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if self.entity_type.fixed {
            self.entity_type.dimensions
        } else {
            self.entity_type.dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn max_up_step(&self) -> f32 {
        self.attributes()
            .lock()
            .get_value(vanilla_attributes::STEP_HEIGHT)
            .unwrap_or(f64::from(DEFAULT_STEP_HEIGHT)) as f32
    }

    fn sound_source(&self) -> SoundSource {
        Monster::sound_source_monster(self)
    }

    fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        Monster::fall_sounds_monster(self)
    }

    fn swim_sound(&self) -> SoundEventRef {
        Monster::swim_sound_monster(self)
    }

    fn swim_splash_sound(&self) -> SoundEventRef {
        Monster::swim_splash_sound_monster(self)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        let state = self.state.lock();
        if let Some(pos) = state.bound_origin {
            nbt.insert(
                TAG_BOUND_POS,
                NbtTag::IntArray(vec![pos.x(), pos.y(), pos.z()]),
            );
        }
        if state.has_limited_life {
            nbt.insert(TAG_LIFE_TICKS, state.limited_life_ticks);
        }
        if let Some(owner) = state.owner.as_ref() {
            owner.store(nbt, TAG_OWNER);
        }
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        let mut state = self.state.lock();
        state.bound_origin =
            nbt.int_array(TAG_BOUND_POS)
                .and_then(|values| match values.as_slice() {
                    [x, y, z] => Some(BlockPos::new(*x, *y, *z)),
                    _ => None,
                });
        match nbt.int(TAG_LIFE_TICKS) {
            Some(life_ticks) => {
                state.has_limited_life = true;
                state.limited_life_ticks = life_ticks;
            }
            None => state.has_limited_life = false,
        }
        state.owner = EntityReference::read(&nbt, TAG_OWNER);
    }
}

impl LivingEntity for VexEntity {
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

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VEX_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VEX_DEATH)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for VexEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        self.tick_vex_move_control();
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_VEX_AMBIENT)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let difficulty = world.current_difficulty_at(self.block_position());
        let mut random = LegacyRandom::from_seed(rand::random());
        // Vanilla `Vex.populateDefaultEquipmentSlots`: a sword that never drops.
        let _ = self.living_base.equipment().lock().set(
            EquipmentSlot::MainHand,
            ItemStack::new(&vanilla_items::IRON_SWORD),
        );
        self.set_equipment_drop_chance(EquipmentSlot::MainHand, 0.0);
        populate_default_equipment_enchantments(self, &mut random, difficulty);
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for VexEntity {}

impl Monster for VexEntity {}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Weak;

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::{init_vanilla_registry, vanilla_entities};
    use steel_utils::BlockPos;

    use super::VexEntity;
    use crate::entity::Entity;

    #[test]
    fn bound_origin_and_life_round_trip() {
        init_vanilla_registry();
        let vex = VexEntity::new(&vanilla_entities::VEX, 1, DVec3::ZERO, Weak::new());
        vex.set_bound_origin(Some(BlockPos::new(3, 70, -9)));
        vex.set_limited_life(600);
        vex.set_is_charging(true);
        assert!(vex.is_charging());

        let mut nbt = NbtCompound::new();
        vex.save_additional(&mut nbt);
        assert_eq!(nbt.int("life_ticks"), Some(600));
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        let loaded = VexEntity::new(&vanilla_entities::VEX, 2, DVec3::ZERO, Weak::new());
        loaded.load_additional((&borrowed).into());
        assert_eq!(loaded.bound_origin(), Some(BlockPos::new(3, 70, -9)));
        assert_eq!(loaded.state.lock().limited_life_ticks, 600);
    }

    #[test]
    fn vex_flaps_every_fourth_tick() {
        init_vanilla_registry();
        let vex = VexEntity::new(&vanilla_entities::VEX, 1, DVec3::ZERO, Weak::new());
        assert!(vex.is_flapping());
    }
}
