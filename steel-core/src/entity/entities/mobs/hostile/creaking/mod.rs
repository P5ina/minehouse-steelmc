//! Vanilla `Creaking`: the pale garden mob that freezes while watched and is
//! bound to a creaking heart.

mod ai;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, CreakingHeartState};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::particle_type::{BlockParticleOption, ParticleData};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::CreakingEntityData;
use steel_registry::{sound_events, vanilla_blocks, vanilla_game_events, vanilla_particle_types};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};

use crate::block_entity::entities::CreakingHeartBlockEntity;
use crate::entity::ai::brain::activity::Activity;
use crate::entity::ai::brain::memory_types::{ATTACK_TARGET, NEAREST_PLAYERS};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityStatus, EntitySyncedData,
    LivingEntity, LivingEntityBase, Mob, MobBase, Monster, PathfinderMob, RemovalReason,
    SharedEntity, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, World};

/// Vanilla `Creaking.ATTACK_ANIMATION_DURATION`.
const ATTACK_ANIMATION_DURATION: i32 = 15;
/// Vanilla `Creaking.INVULNERABILITY_ANIMATION_DURATION`.
const INVULNERABILITY_ANIMATION_DURATION: i32 = 8;
/// Vanilla `Creaking.TWITCH_DEATH_DURATION`.
const TWITCH_DEATH_DURATION: i32 = 45;
/// Vanilla `Creaking.ACTIVATION_RANGE_SQ`.
const ACTIVATION_RANGE_SQR: f64 = 144.0;
/// Vanilla `Creaking.MAX_PLAYER_STUCK_COUNTER`.
const MAX_PLAYER_STUCK_COUNTER: i32 = 4;
/// Vanilla `Creaking.isLookingAtMe` cone size.
const LOOK_CONE: f64 = 0.5;

/// Per-tick counters the creaking keeps outside its synced data.
#[derive(Default)]
struct CreakingCounters {
    attack_animation_remaining_ticks: i32,
    invulnerability_animation_remaining_ticks: i32,
    player_stuck_counter: i32,
}

/// A creaking.
#[entity_behavior(class = "Creaking", spawn_rule)]
pub struct CreakingEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<CreakingEntityData>,
    brain: Brain<Self>,
    counters: SyncMutex<CreakingCounters>,
}

// SAFETY: The owner-scoped type key uniquely identifies CreakingEntity.
unsafe impl DowncastType for CreakingEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/creaking");
}

impl CreakingEntity {
    /// Vanilla `SpawnPlacements`: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new creaking.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved creaking.
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
        mob_base.set_xp_reward(0);
        mob_base.navigation().lock().set_can_float(true);
        let mut entity_data = CreakingEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        let brain = Brain::new(&[], ai::sensors(), ai::activities(), None);
        brain.set_default_activity(Activity::IDLE);
        brain.use_default_activity();
        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            brain,
            counters: SyncMutex::new(CreakingCounters::default()),
        }
    }

    /// The creaking's brain memories.
    #[must_use]
    pub const fn memories(&self) -> &SyncMutex<Memories> {
        self.brain.memories()
    }

    /// Vanilla `setTransient`: binds the creaking to the heart at `pos`.
    pub fn set_transient(&self, pos: BlockPos) {
        self.set_home_pos(pos);
        self.set_pathfinding_malus(PathType::Damaging, 8.0);
        self.set_pathfinding_malus(PathType::PowderSnow, 8.0);
        self.set_pathfinding_malus(PathType::Lava, 8.0);
        self.set_pathfinding_malus(PathType::Fire, 0.0);
        self.set_pathfinding_malus(PathType::FireInNeighbor, 0.0);
    }

    /// Vanilla `isHeartBound`.
    #[must_use]
    pub fn is_heart_bound(&self) -> bool {
        self.home_pos().is_some()
    }

    /// Vanilla `getHomePos`.
    #[must_use]
    pub fn home_pos(&self) -> Option<BlockPos> {
        *self.entity_data.lock().home_pos.get()
    }

    fn set_home_pos(&self, pos: BlockPos) {
        self.entity_data.lock().home_pos.set(Some(pos));
    }

    /// Vanilla `canMove`: false while a player watches it.
    #[must_use]
    pub fn can_move(&self) -> bool {
        *self.entity_data.lock().can_move.get()
    }

    /// Vanilla `isActive`: it has noticed a player.
    #[must_use]
    pub fn is_active(&self) -> bool {
        *self.entity_data.lock().is_active.get()
    }

    fn set_is_active(&self, active: bool) {
        self.entity_data.lock().is_active.set(active);
    }

    /// Vanilla `isTearingDown`.
    #[must_use]
    pub fn is_tearing_down(&self) -> bool {
        *self.entity_data.lock().is_tearing_down.get()
    }

    /// Vanilla `setTearingDown`.
    pub fn set_tearing_down(&self) {
        self.entity_data.lock().is_tearing_down.set(true);
    }

    /// The heart at the home position, when it still protects this creaking.
    fn protecting_heart(&self, world: &World, home: BlockPos) -> Option<bool> {
        let block_entity = world.get_block_entity(home)?;
        let heart = block_entity.downcast_ref::<CreakingHeartBlockEntity>()?;
        Some(heart.is_protector(world, self.id()))
    }

    /// Vanilla `blameSourceForDamage`: records who hurt it and returns the
    /// responsible player.
    pub fn blame_source_for_damage(&self, source: &DamageSource) -> Option<SharedEntity> {
        self.resolve_mob_responsible_for_damage(source);
        self.resolve_player_responsible_for_damage(source);
        let world = self.level()?;
        self.living_base.last_hurt_by_player(&world)
    }

    /// Vanilla `tearDown`: crumbles into pale oak and heart particles and is
    /// discarded.
    pub fn tear_down(&self) {
        if let Some(world) = self.level() {
            let bounding_box = self.bounding_box();
            let center = bounding_box.center();
            let spread = DVec3::new(
                bounding_box.width() * 0.3,
                bounding_box.height() * 0.3,
                bounding_box.depth() * 0.3,
            );
            world.send_particles(
                block_crumble(vanilla_blocks::PALE_OAK_WOOD.default_state()),
                center,
                100,
                spread,
                0.0,
            );
            world.send_particles(
                block_crumble(vanilla_blocks::CREAKING_HEART.default_state().set_value(
                    &BlockStateProperties::CREAKING_HEART_STATE,
                    CreakingHeartState::Awake,
                )),
                center,
                10,
                spread,
                0.0,
            );
        }
        self.make_sound(self.death_sound());
        self.set_removed(RemovalReason::Discarded);
    }

    /// Vanilla `creakingDeathEffects`: dies to `source` and twitches.
    pub fn creaking_death_effects(&self, source: &DamageSource) {
        self.blame_source_for_damage(source);
        self.die(source);
        self.make_sound(Some(&sound_events::ENTITY_CREAKING_TWITCH));
    }

    /// Vanilla `playerIsStuckInYou`: a player's eyes stayed inside it for
    /// more than four checks.
    pub fn player_is_stuck_in_you(&self) -> bool {
        let players = self
            .memories()
            .lock()
            .get(&NEAREST_PLAYERS)
            .unwrap_or_default();
        let mut counters = self.counters.lock();
        if players.is_empty() {
            counters.player_stuck_counter = 0;
            return false;
        }
        let own_box = self.bounding_box();
        if players.iter().any(|player| {
            let position = player.position();
            own_box.contains(DVec3::new(position.x, player.get_eye_y(), position.z))
        }) {
            counters.player_stuck_counter += 1;
            return counters.player_stuck_counter > MAX_PLAYER_STUCK_COUNTER;
        }
        counters.player_stuck_counter = 0;
        false
    }

    /// Vanilla `checkCanMove`: frozen while an attackable player looks at it,
    /// activating on the first look within 12 blocks.
    pub fn check_can_move(&self) -> bool {
        let players = self
            .memories()
            .lock()
            .get(&NEAREST_PLAYERS)
            .unwrap_or_default();
        let active = self.is_active();
        if players.is_empty() {
            if active {
                self.deactivate();
            }
            return true;
        }

        let mut has_potential_target = false;
        for player_entity in &players {
            let Some(player) = player_entity.as_player() else {
                continue;
            };
            if !Mob::can_attack(self, player) || self.is_allied_to(player) {
                continue;
            }
            has_potential_target = true;
            let position = self.position();
            let gaze_heights = [
                self.get_eye_y(),
                position.y + 0.5 * f64::from(self.get_scale()),
                f64::midpoint(self.get_eye_y(), position.y),
            ];
            if (!active || player_not_wearing_disguise_item(player))
                && self.is_looking_at_me(player, LOOK_CONE, false, true, &gaze_heights)
            {
                if active {
                    return false;
                }
                if player.position().distance_squared(position) < ACTIVATION_RANGE_SQR {
                    self.activate(player_entity);
                    return false;
                }
            }
        }

        if !has_potential_target && active {
            self.deactivate();
        }
        true
    }

    /// Vanilla `activate`.
    pub fn activate(&self, player: &SharedEntity) {
        self.memories()
            .lock()
            .set(&ATTACK_TARGET, Some(Arc::clone(player)));
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.make_sound(Some(&sound_events::ENTITY_CREAKING_ACTIVATE));
        self.set_is_active(true);
    }

    /// Vanilla `deactivate`.
    pub fn deactivate(&self) {
        self.memories().lock().erase(&ATTACK_TARGET);
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.make_sound(Some(&sound_events::ENTITY_CREAKING_DEACTIVATE));
        self.set_is_active(false);
    }

    fn update_dirty_mob_effect_entity_data(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }
}

/// Vanilla `BlockParticleOption(ParticleTypes.BLOCK_CRUMBLE, state)`.
fn block_crumble(state: BlockStateId) -> ParticleData {
    ParticleData::new(
        &vanilla_particle_types::BLOCK_CRUMBLE,
        BlockParticleOption::new(state),
    )
}

/// Vanilla `LivingEntity.PLAYER_NOT_WEARING_DISGUISE_ITEM`.
fn player_not_wearing_disguise_item(player: &Player) -> bool {
    use steel_registry::equipment::EquipmentSlot;
    use steel_registry::vanilla_item_tags::ItemTag;

    let mut disguised = false;
    player.with_equipment_slot(EquipmentSlot::Head, &mut |helmet| {
        disguised = helmet.item().has_tag(&ItemTag::GAZE_DISGUISE_EQUIPMENT);
    });
    !disguised
}

/// Vanilla `BlockPos.CODEC`: an `[x, y, z]` int array.
fn read_block_pos(nbt: BorrowedNbtCompoundView<'_, '_>, key: &str) -> Option<BlockPos> {
    match nbt.int_array(key)?.as_slice() {
        &[x, y, z] => Some(BlockPos::new(x, y, z)),
        _ => None,
    }
}

impl DefaultEntityPush for CreakingEntity {}

impl Entity for CreakingEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `Creaking.tick`: a heart-bound creaking dies once its heart no
    /// longer protects it.
    fn tick(self: Arc<Self>) {
        if let Some(home) = self.home_pos()
            && let Some(world) = self.level()
            && self.protecting_heart(&world, home) != Some(true)
        {
            self.set_health(0.0);
        }
        let entity = Arc::clone(&self).into_shared_entity();
        if let Some(living) = entity.as_living_entity() {
            living.tick_living_entity(&entity);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        self.update_dirty_mob_effect_entity_data();
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

    /// Vanilla `Creaking.isPushable`: not while frozen.
    fn is_pushable(&self) -> bool {
        LivingEntity::is_alive(self)
            && !self.is_spectator()
            && !self.on_climbable()
            && self.can_move()
    }

    /// Vanilla `Creaking.push(double, double, double)`: not while frozen.
    fn push_impulse(&self, impulse: DVec3) {
        if self.can_move() {
            self.base().push_impulse(impulse);
        }
    }

    /// Vanilla `Creaking.fireImmune`.
    fn fire_immune(&self) -> bool {
        self.is_heart_bound() || self.entity_type.fire_immune
    }

    /// Vanilla `Creaking.canUsePortal`: heart-bound creakings stay home.
    fn can_use_portal(&self, ignore_passenger: bool) -> bool {
        !self.is_heart_bound()
            && (ignore_passenger || !self.is_passenger())
            && LivingEntity::is_alive(self)
            && !self.is_sleeping()
    }

    /// Vanilla `Creaking.playStepSound`.
    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_CREAKING_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("Brain", self.brain.save());
        if let Some(home) = self.home_pos() {
            nbt.insert(
                "home_pos",
                NbtTag::IntArray(vec![home.x(), home.y(), home.z()]),
            );
        }
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
        if let Some(home) = read_block_pos(nbt, "home_pos") {
            self.set_transient(home);
        }
    }
}

impl LivingEntity for CreakingEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
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

    /// Vanilla `Creaking.getHurtSound`.
    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_heart_bound() {
            &sound_events::ENTITY_CREAKING_SWAY
        } else {
            &sound_events::ENTITY_GENERIC_HURT
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_CREAKING_DEATH)
    }

    /// Vanilla `Creaking.hurtServer`: a heart-bound creaking only flinches,
    /// and tells its heart when a player hit it.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let Some(home) = self.home_pos() else {
            return self.default_hurt_server(world, source, amount);
        };
        if source.is(&DamageTypeTag::BYPASSES_INVULNERABILITY) {
            return self.default_hurt_server(world, source, amount);
        }
        if self.is_invulnerable_to(world, source)
            || self
                .counters
                .lock()
                .invulnerability_animation_remaining_ticks
                > 0
            || self.is_dead_or_dying()
        {
            return false;
        }

        let responsible_player = self.blame_source_for_damage(source);
        let direct_is_attacker = source
            .direct_entity()
            .is_some_and(|direct| direct.is_living_entity() || direct.as_projectile().is_some());
        if !direct_is_attacker && responsible_player.is_none() {
            return false;
        }

        self.counters
            .lock()
            .invulnerability_animation_remaining_ticks = INVULNERABILITY_ANIMATION_DURATION;
        self.broadcast_entity_event(EntityStatus::Shake);
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        if let Some(block_entity) = world.get_block_entity(home)
            && let Some(heart) = block_entity.downcast_ref::<CreakingHeartBlockEntity>()
            && heart.is_protector(world, self.id())
        {
            if responsible_player.is_some()
                && let Some(level) = self.level()
            {
                heart.creaking_hurt(&level);
            }
            self.play_hurt_sound(source);
        }
        true
    }

    /// Vanilla `Creaking.knockback`: not while frozen.
    fn knockback(&self, power: f64, xd: f64, zd: f64) {
        if self.can_move() {
            self.default_knockback(power, xd, zd);
        }
    }

    /// Vanilla `Creaking.tickDeath`: a torn-down creaking twitches for 45
    /// ticks before crumbling.
    fn tick_death(&self) {
        if self.is_heart_bound() && self.is_tearing_down() {
            let death_time = self.living_base.increment_death_time();
            if death_time > TWITCH_DEATH_DURATION && !self.is_removed() {
                self.tear_down();
            }
        } else {
            self.default_tick_death();
        }
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

    /// Vanilla `Creaking.aiStep`: counts down the animations and freezes or
    /// thaws as players look at it.
    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        {
            let mut counters = self.counters.lock();
            if counters.invulnerability_animation_remaining_ticks > 0 {
                counters.invulnerability_animation_remaining_ticks -= 1;
            }
            if counters.attack_animation_remaining_ticks > 0 {
                counters.attack_animation_remaining_ticks -= 1;
            }
        }

        let can_move = self.can_move();
        let now_can_move = self.check_can_move();
        if now_can_move != can_move {
            self.game_event(&vanilla_game_events::ENTITY_ACTION);
            if now_can_move {
                self.make_sound(Some(&sound_events::ENTITY_CREAKING_UNFREEZE));
            } else {
                self.stop_in_place();
                self.make_sound(Some(&sound_events::ENTITY_CREAKING_FREEZE));
            }
        }
        self.entity_data.lock().can_move.set(now_can_move);

        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for CreakingEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    /// Vanilla `CreakingPathNavigation.tick`: paused while frozen.
    fn tick_path_navigation(&self) {
        if self.can_move() {
            PathfinderMob::tick_pathfinder_path_navigation(self);
        }
    }

    /// Vanilla `CreakingMoveControl.tick`.
    fn tick_move_control(&self) {
        if self.can_move() {
            self.default_tick_move_control();
        }
    }

    /// Vanilla `CreakingLookControl.tick`.
    fn tick_look_control(&self) {
        if self.can_move() {
            self.default_tick_look_control();
        }
    }

    /// Vanilla `CreakingJumpControl.tick`.
    fn tick_jump_control(&self) {
        if self.can_move() {
            let jumping = self.mob_base().controls().lock().jump_control.tick();
            self.set_jumping(jumping);
        } else {
            self.set_jumping(false);
        }
    }

    /// Vanilla `CreakingBodyRotationControl.clientTick`.
    fn tick_body_rotation_control(&self) {
        if self.can_move() {
            self.default_tick_body_rotation_control();
        }
    }

    fn path_home_bound(&self) -> Option<BlockPos> {
        self.home_pos()
    }

    /// Vanilla `Creaking.getTarget`: the brain's attack target.
    fn target(&self) -> Option<SharedEntity> {
        self.brain.memories().lock().get(&ATTACK_TARGET)
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
        }
        ai::update_activity(self);
    }

    /// Vanilla `Creaking.doHurtTarget`: only living targets, with the attack
    /// animation.
    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        if !target.is_living_entity() {
            return false;
        }
        self.counters.lock().attack_animation_remaining_ticks = ATTACK_ANIMATION_DURATION;
        self.broadcast_entity_event(EntityStatus::StartAttacking);
        self.default_do_hurt_target(entity, world, target)
    }

    fn play_attack_sound(&self) {
        self.make_sound(Some(&sound_events::ENTITY_CREAKING_ATTACK));
    }

    /// Vanilla `Creaking.getAmbientSound`: silent once it has noticed a player.
    fn ambient_sound(&self) -> Option<SoundEventRef> {
        (!self.is_active()).then_some(&sound_events::ENTITY_CREAKING_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl PathfinderMob for CreakingEntity {
    /// Vanilla `Creaking.getWalkTargetValue`: every block is as good.
    fn get_walk_target_value_in(&self, _pos: BlockPos, _level: &dyn LevelReader) -> f32 {
        0.0
    }
}

impl Monster for CreakingEntity {}
