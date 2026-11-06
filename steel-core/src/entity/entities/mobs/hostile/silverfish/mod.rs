//! Vanilla `Silverfish`: a small pest that hides in stone and wakes the
//! silverfish in nearby infested blocks when hurt.

use std::iter;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::SilverfishEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{sound_events, vanilla_attributes};
use steel_utils::direction::Direction;
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::behavior::blocks::{
    host_state_by_infested, infested_state_by_host, is_compatible_host_block,
};
use crate::entity::ai::goal::{
    ClimbOnTopOfPowderSnowGoal, FloatGoal, Goal, GoalControls, HurtByTargetGoal, MeleeAttackGoal,
    NearestAttackableTargetGoal, RandomStrollGoal, reduced_tick_delay,
};
use crate::entity::damage::DamageSource;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityEventSource as _,
    EntityMovementEmission, EntityPose, EntitySpawnReason, EntitySyncedData, LivingEntity,
    LivingEntityBase, Mob, MobBase, Monster, PathfinderMob, RemovalReason, SharedEntity, SpawnRule,
    sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::{LevelReader, ServerLevelAccessor, World};

const XP_REWARD: i32 = 5;
const DEFAULT_STEP_HEIGHT: f32 = 0.6;
/// Vanilla `SilverfishWakeUpFriendsGoal` search radius, horizontal and vertical.
const WAKE_RADIUS_XZ: i32 = 10;
const WAKE_RADIUS_Y: i32 = 5;
/// Ticks between being hurt and waking friends (`adjustedTickDelay(20)`).
const WAKE_DELAY_TICKS: i32 = 20;
/// Players closer than this stop natural silverfish spawns.
const SPAWN_PLAYER_DISTANCE: f64 = 5.0;
/// Walk-target score for blocks above a stone-like host block.
const HOST_BLOCK_WALK_VALUE: f32 = 10.0;

/// A silverfish.
#[entity_behavior(class = "Silverfish", spawn_rule)]
pub struct SilverfishEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<SilverfishEntityData>,
    /// `SilverfishWakeUpFriendsGoal.lookForFriends`, shared with the goal.
    look_for_friends: Arc<AtomicI32>,
}

// SAFETY: The owner-scoped type key uniquely identifies SilverfishEntity.
unsafe impl DowncastType for SilverfishEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/silverfish");
}

impl SilverfishEntity {
    /// Vanilla `SpawnPlacements` predicate: `Silverfish.checkSilverfishSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = Self::check_silverfish_spawn_rules;

    /// Vanilla `Silverfish.checkSilverfishSpawnRules`: any light, and no
    /// player within five blocks unless from a spawner.
    pub(crate) fn check_silverfish_spawn_rules(
        entity_type: EntityTypeRef,
        level: &dyn ServerLevelAccessor,
        spawn_reason: EntitySpawnReason,
        pos: BlockPos,
        _random: &mut dyn Rng,
    ) -> bool {
        if !Self::check_any_light_monster_spawn_rules(entity_type, level, spawn_reason, pos) {
            return false;
        }
        if spawn_reason.is_spawner() {
            return true;
        }
        let center = DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.5,
            f64::from(pos.z()) + 0.5,
        );
        !level.has_nearby_player(center, SPAWN_PLAYER_DISTANCE, true)
    }

    /// Creates a new silverfish.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved silverfish.
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
        let mut entity_data = SilverfishEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        let look_for_friends = Arc::new(AtomicI32::new(0));

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(1, FloatGoal::new(&mob_base));
            goal_selector.add_goal(1, ClimbOnTopOfPowderSnowGoal::new());
            goal_selector.add_goal(
                3,
                WakeUpFriendsGoal {
                    look_for_friends: Arc::clone(&look_for_friends),
                },
            );
            goal_selector.add_goal(4, MeleeAttackGoal::new(1.0, false));
            goal_selector.add_goal(5, MergeWithStoneGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, HurtByTargetGoal::new().set_alert_others([]));
            target_selector.add_goal(
                2,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            look_for_friends,
        }
    }

    /// Vanilla `SilverfishWakeUpFriendsGoal.notifyHurt`.
    fn notify_hurt(&self) {
        let _ = self.look_for_friends.compare_exchange(
            0,
            reduced_tick_delay(WAKE_DELAY_TICKS),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Ticks left before this silverfish wakes its friends; 0 when idle.
    #[must_use]
    pub fn look_for_friends(&self) -> i32 {
        self.look_for_friends.load(Ordering::Relaxed)
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

/// Vanilla `Silverfish.SilverfishWakeUpFriendsGoal`.
struct WakeUpFriendsGoal {
    look_for_friends: Arc<AtomicI32>,
}

impl Goal for WakeUpFriendsGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        self.look_for_friends.load(Ordering::Relaxed) > 0
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let remaining = self.look_for_friends.fetch_sub(1, Ordering::Relaxed) - 1;
        if remaining > 0 {
            return;
        }
        let Some(world) = mob.level() else {
            return;
        };
        let griefing = world.get_game_rule(&MOB_GRIEFING);
        let base_pos = mob.block_position();
        // Vanilla walks each axis outward from 0: 0, 1, -1, 2, -2, ...
        for y_off in outward_offsets(WAKE_RADIUS_Y) {
            for x_off in outward_offsets(WAKE_RADIUS_XZ) {
                for z_off in outward_offsets(WAKE_RADIUS_XZ) {
                    let test_pos = base_pos.offset(x_off, y_off, z_off);
                    let state = world.get_block_state(test_pos);
                    let Some(host_state) = host_state_by_infested(state) else {
                        continue;
                    };
                    if griefing {
                        let _ = world.destroy_block_by_entity(test_pos, true, entity.as_ref());
                    } else {
                        world.set_block(test_pos, host_state, UpdateFlags::UPDATE_ALL);
                    }
                    if rand::random::<bool>() {
                        return;
                    }
                }
            }
        }
    }
}

/// Vanilla's `for (off = 0; off <= r && off >= -r; off = (off <= 0 ? 1 : 0) - off)`.
fn outward_offsets(radius: i32) -> impl Iterator<Item = i32> {
    iter::successors(Some(0), |&off| Some(i32::from(off <= 0) - off))
        .take_while(move |&off| off <= radius && off >= -radius)
}

/// Vanilla `Silverfish.SilverfishMergeWithStoneGoal`: a random stroll that
/// sometimes hides the silverfish inside a neighboring stone block.
struct MergeWithStoneGoal {
    stroll: RandomStrollGoal,
    selected_direction: Option<Direction>,
    do_merge: bool,
}

impl MergeWithStoneGoal {
    const fn new() -> Self {
        Self {
            stroll: RandomStrollGoal::with_interval(1.0, 10),
            selected_direction: None,
            do_merge: false,
        }
    }

    fn merge_pos(mob: &dyn PathfinderMob, direction: Direction) -> BlockPos {
        let position = mob.position();
        BlockPos::from(DVec3::new(position.x, position.y + 0.5, position.z)).relative(direction)
    }
}

impl Goal for MergeWithStoneGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if mob.target().is_some() {
            return false;
        }
        if !mob.mob_base().navigation().lock().is_done() {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        if world.get_game_rule(&MOB_GRIEFING) && rand::random_range(0..reduced_tick_delay(10)) == 0
        {
            let direction = Direction::random();
            self.selected_direction = Some(direction);
            let state = world.get_block_state(Self::merge_pos(mob, direction));
            if is_compatible_host_block(state) {
                self.do_merge = true;
                return true;
            }
        }
        self.do_merge = false;
        self.stroll.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !self.do_merge && self.stroll.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if !self.do_merge {
            self.stroll.start(mob);
            return;
        }
        let (Some(world), Some(direction)) = (mob.level(), self.selected_direction) else {
            return;
        };
        let pos = Self::merge_pos(mob, direction);
        let state = world.get_block_state(pos);
        if let Some(infested) = infested_state_by_host(state) {
            world.set_block(pos, infested, UpdateFlags::UPDATE_ALL);
            mob.broadcast_entity_event(EntityStatus::SilverfishMergeAnim);
            mob.set_removed(RemovalReason::Discarded);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if !self.do_merge {
            self.stroll.stop(mob);
        }
    }
}

impl DefaultEntityPush for SilverfishEntity {}

impl Entity for SilverfishEntity {
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
        EntityMovementEmission::Events
    }

    fn tick(self: Arc<Self>) {
        // Vanilla `Silverfish.tick`: the body always faces the head yaw.
        self.living_base.set_y_body_rot(self.rotation().0);
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
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
        self.update_dirty_mob_effect_entity_data();
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

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_SILVERFISH_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
    }
}

impl LivingEntity for SilverfishEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    /// Vanilla `Silverfish.setYBodyRot`: the head follows the body.
    fn set_y_body_rot(&self, y_body_rot: f32) {
        let (_, pitch) = self.rotation();
        self.set_rotation((y_body_rot, pitch));
        self.living_base().set_y_body_rot(y_body_rot);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SILVERFISH_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SILVERFISH_DEATH)
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_invulnerable_to(world, source) {
            return false;
        }
        if source.causing_entity().is_some()
            || source.is(&DamageTypeTag::ALWAYS_TRIGGERS_SILVERFISH)
        {
            self.notify_hurt();
        }
        self.default_hurt_server(world, source, amount)
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

impl Mob for SilverfishEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_SILVERFISH_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl Monster for SilverfishEntity {}

impl PathfinderMob for SilverfishEntity {
    /// Vanilla `Silverfish.getWalkTargetValue`: silverfish love stone.
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        if is_compatible_host_block(level.get_block_state(pos.below())) {
            HOST_BLOCK_WALK_VALUE
        } else {
            self.get_walk_target_value_monster(pos, level)
        }
    }
}

#[cfg(test)]
mod tests;
