//! Vanilla `ElderGuardian`: the persistent monument boss guardian.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::{CGameEvent, GameEventType, SoundSource};
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::ElderGuardianEntityData;
use steel_registry::{sound_events, vanilla_mob_effects};
use steel_utils::locks::SyncMutex;
use steel_utils::types::GameType;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use super::{
    GUARDIAN_AMBIENT_SOUND_INTERVAL, GUARDIAN_MAX_HEAD_X_ROT, Guardian, check_guardian_spawn_rules,
    init_guardian,
};
use crate::entity::damage::DamageSource;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, MobEffectInstance, Monster,
    PathfinderMob, SharedEntity, SpawnRule, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{LevelReader, ServerLevelAccessor, World};

/// Vanilla `ElderGuardian.getAttackDuration`.
const ELDER_GUARDIAN_ATTACK_DURATION: i32 = 60;
/// Vanilla `ElderGuardian` stroll interval set after `registerGoals`.
const ELDER_GUARDIAN_STROLL_INTERVAL: i32 = 400;
const EFFECT_INTERVAL: i32 = 1200;
const EFFECT_RADIUS: f64 = 50.0;
const EFFECT_DURATION: i32 = 6000;
const EFFECT_AMPLIFIER: i32 = 2;
const EFFECT_DISPLAY_LIMIT: i32 = 1200;
const HOME_RADIUS: i32 = 16;

/// Vanilla elder guardian.
#[entity_behavior(class = "ElderGuardian", spawn_rule)]
pub struct ElderGuardianEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<ElderGuardianEntityData>,
    stroll_trigger: Arc<AtomicBool>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ElderGuardianEntity`.
unsafe impl DowncastType for ElderGuardianEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/elder_guardian");
}

impl ElderGuardianEntity {
    /// Vanilla `SpawnPlacements` predicate: `Guardian.checkGuardianSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_guardian_spawn_rules;

    /// Creates a new elder guardian.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved elder guardian.
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
        let stroll_trigger = init_guardian(&mob_base, true, ELDER_GUARDIAN_STROLL_INTERVAL);
        *mob_base.persistence_required().lock() = true;

        let mut entity_data = ElderGuardianEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            stroll_trigger,
        }
    }

    /// Vanilla `MobEffectUtil.addEffectToPlayersAround` with the elder's
    /// mining fatigue curse, plus the curse screen effect for each player hit.
    fn curse_players_around(&self, world: &World) {
        let position = self.position();
        let mut players: Vec<Arc<Player>> = Vec::new();
        world.players.iter_players(|_, player| {
            let refresh = player
                .mob_effect(vanilla_mob_effects::MINING_FATIGUE)
                .is_none_or(|effect| {
                    effect.amplifier() < EFFECT_AMPLIFIER
                        || effect.ends_within(EFFECT_DISPLAY_LIMIT - 1)
                });
            if matches!(player.game_mode(), GameType::Survival | GameType::Adventure)
                && !self.is_allied_to(player.as_ref())
                && position.distance_squared(player.position()) < EFFECT_RADIUS * EFFECT_RADIUS
                && refresh
            {
                players.push(Arc::clone(player));
            }
            true
        });

        let data = if self.is_silent() { 0.0 } else { 1.0 };
        for player in players {
            player.add_mob_effect_with_source(
                MobEffectInstance::with_duration(
                    vanilla_mob_effects::MINING_FATIGUE,
                    EFFECT_DURATION,
                    EFFECT_AMPLIFIER,
                ),
                Some(self),
            );
            player.send_packet(CGameEvent {
                event: GameEventType::GuardianElderEffect,
                data,
            });
        }
    }
}

impl Guardian for ElderGuardianEntity {
    fn is_guardian_moving(&self) -> bool {
        *self.entity_data.lock().guardian().id_moving.get()
    }

    fn set_guardian_moving(&self, moving: bool) {
        self.entity_data.lock().guardian_mut().id_moving.set(moving);
    }

    fn active_attack_target_id(&self) -> i32 {
        *self.entity_data.lock().guardian().id_attack_target.get()
    }

    fn set_active_attack_target(&self, entity_id: i32) {
        self.entity_data
            .lock()
            .guardian_mut()
            .id_attack_target
            .set(entity_id);
    }

    fn random_stroll_trigger(&self) -> &AtomicBool {
        &self.stroll_trigger
    }

    fn attack_duration(&self) -> i32 {
        ELDER_GUARDIAN_ATTACK_DURATION
    }
}

impl DefaultEntityPush for ElderGuardianEntity {}

impl Entity for ElderGuardianEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        self.guardian_hurt(world, source, amount)
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(LivingEntity::get_scale(self))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
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
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
    }
}

impl LivingEntity for ElderGuardianEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let clamped = health.clamp(0.0, self.get_max_health());
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_ELDER_GUARDIAN_HURT
        } else {
            &sound_events::ENTITY_ELDER_GUARDIAN_HURT_LAND
        })
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_ELDER_GUARDIAN_DEATH
        } else {
            &sound_events::ENTITY_ELDER_GUARDIAN_DEATH_LAND
        })
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn travel_in_water(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        _base_gravity: f64,
        _is_falling: bool,
        _old_y: f64,
    ) -> Option<MoveResult> {
        self.guardian_travel_in_water(entity, input)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        self.guardian_ai_step(entity)
    }
}

impl Mob for ElderGuardianEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn tick_move_control(&self) {
        self.tick_guardian_move_control();
    }

    /// Vanilla `ElderGuardian.customServerAiStep`: curses nearby players
    /// every minute and keeps a home around where it first ticked.
    fn custom_server_ai_step(&self) {
        if self.tick_count().wrapping_add(self.id()) % EFFECT_INTERVAL == 0
            && let Some(world) = self.level()
        {
            self.curse_players_around(&world);
        }

        if !self.has_home() {
            self.set_home_to(self.block_position(), HOME_RADIUS);
        }
    }

    fn max_head_x_rot(&self) -> f32 {
        GUARDIAN_MAX_HEAD_X_ROT
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.is_in_water() {
            &sound_events::ENTITY_ELDER_GUARDIAN_AMBIENT
        } else {
            &sound_events::ENTITY_ELDER_GUARDIAN_AMBIENT_LAND
        })
    }

    fn ambient_sound_interval(&self) -> i32 {
        GUARDIAN_AMBIENT_SOUND_INTERVAL
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        level.is_unobstructed_by_entities(self)
    }
}

impl Monster for ElderGuardianEntity {}

impl PathfinderMob for ElderGuardianEntity {
    fn get_walk_target_value_in(&self, pos: BlockPos, level: &dyn LevelReader) -> f32 {
        self.guardian_walk_target_value(pos, level)
    }
}
