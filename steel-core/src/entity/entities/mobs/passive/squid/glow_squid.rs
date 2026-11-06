use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::{borrow, owned};
use steel_macros::entity_behavior;
use steel_registry::{
    blocks::block_state_ext::BlockStateExt as _,
    entity_data::{EntityPose, ParticleData},
    entity_type::{EntityDimensions, EntityTypeRef},
    sound_event::SoundEventRef,
    sound_events, vanilla_blocks,
    vanilla_entity_data::GlowSquidEntityData,
    vanilla_particle_types,
};
use steel_utils::{
    BlockPos, DowncastType, DowncastTypeKey, locks::SyncMutex, types::InteractionHand,
};

use super::{
    SQUID_AMBIENT_SOUND_INTERVAL, SQUID_BABY_DIMENSIONS, SQUID_GRAVITY, SQUID_SOUND_VOLUME, Squid,
    SquidState, init_squid,
};
use crate::{
    behavior::InteractionResult,
    entity::{
        AgeableMob, AgeableMobBase, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission,
        EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase,
        PathfinderMob, SharedEntity, SpawnGroupData, SpawnRule, damage::DamageSource,
    },
    physics::{MoveResult, MoverType},
    player::Player,
    world::{ServerLevelAccessor, World},
};

/// Vanilla `GlowSquid.checkGlowSquidSpawnRules`: deep (33 or more blocks
/// below sea level), in complete darkness and inside a water block.
fn check_glow_squid_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    pos.y() <= level.sea_level() - GLOW_SQUID_MIN_SPAWN_DEPTH
        && level.raw_brightness(pos, 0) == 0
        && level.get_block_state(pos).get_block() == &vanilla_blocks::WATER
}

/// How far below sea level glow squids spawn at the least.
const GLOW_SQUID_MIN_SPAWN_DEPTH: i32 = 33;

/// Vanilla `GlowSquid`: ticks dark for 100 ticks after a mob hurts it.
const DARK_TICKS_AFTER_HURT: i32 = 100;

/// Vanilla glow squid: a squid with glowing ink that dims when hurt.
#[entity_behavior(class = "GlowSquid", spawn_rule)]
pub struct GlowSquidEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    ageable_base: AgeableMobBase,
    entity_data: SyncMutex<GlowSquidEntityData>,
    state: SyncMutex<SquidState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `GlowSquidEntity`.
unsafe impl DowncastType for GlowSquidEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/glow_squid");
}

impl GlowSquidEntity {
    /// Vanilla `SpawnPlacements` predicate: `GlowSquid.checkGlowSquidSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = check_glow_squid_spawn_rules;

    /// Creates a new glow squid.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Recreates a glow squid from saved entity state.
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
        init_squid(&living_base, &mob_base);

        let mut entity_data = GlowSquidEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            ageable_base: AgeableMobBase::new(),
            entity_data: SyncMutex::new(entity_data),
            state: SyncMutex::new(SquidState::new()),
        }
    }

    /// Ticks left until the glow squid lights up again.
    #[must_use]
    pub fn dark_ticks_remaining(&self) -> i32 {
        *self.entity_data.lock().dark_ticks_remaining.get()
    }

    fn set_dark_ticks(&self, ticks: i32) {
        self.entity_data.lock().dark_ticks_remaining.set(ticks);
    }
}

impl Squid for GlowSquidEntity {
    fn squid_state(&self) -> &SyncMutex<SquidState> {
        &self.state
    }

    fn ink_particle(&self) -> ParticleData {
        ParticleData::simple(&vanilla_particle_types::GLOW_SQUID_INK)
    }

    fn squirt_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_GLOW_SQUID_SQUIRT
    }
}

impl Entity for GlowSquidEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        let hurt = self.squid_hurt(world, source, amount);
        if hurt {
            self.set_dark_ticks(DARK_TICKS_AFTER_HURT);
        }
        hurt
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn base_tick(&self) {
        let air_supply = self.air_supply();
        Mob::base_tick_mob(self);
        self.handle_squid_air_supply(air_supply);
    }

    fn get_default_gravity(&self) -> f64 {
        SQUID_GRAVITY
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        let scale = LivingEntity::get_scale(self);
        if AgeableMob::is_baby(self) {
            SQUID_BABY_DIMENSIONS.scale(scale)
        } else if self.entity_type.fixed {
            self.entity_type.dimensions
        } else {
            self.entity_type.dimensions.scale(scale)
        }
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn save_additional(&self, nbt: &mut owned::NbtCompound) {
        self.save_mob(nbt);
        self.save_ageable_mob(nbt);
        nbt.insert("DarkTicksRemaining", self.dark_ticks_remaining());
    }

    fn load_additional(&self, nbt: borrow::NbtCompound<'_, '_>) {
        self.load_mob(nbt);
        self.load_ageable_mob(nbt);
        self.set_dark_ticks(nbt.int("DarkTicksRemaining").unwrap_or(0));
    }
}

impl LivingEntity for GlowSquidEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn sound_volume(&self) -> f32 {
        SQUID_SOUND_VOLUME
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

    fn travel(&self, entity: &SharedEntity, _input: DVec3) -> Option<MoveResult> {
        Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity())
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = self.squid_ai_step(entity);
        let dark_ticks = self.dark_ticks_remaining();
        if dark_ticks > 0 {
            self.set_dark_ticks(dark_ticks - 1);
        }
        // Vanilla also calls `level.addParticle(GLOW, ..)`, which does nothing
        // on a server level.
        result
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GLOW_SQUID_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GLOW_SQUID_DEATH)
    }

    fn base_experience_reward(&self) -> i32 {
        rand::random_range(1..4)
    }
}

impl AgeableMob for GlowSquidEntity {
    fn ageable_base(&self) -> &AgeableMobBase {
        &self.ageable_base
    }

    fn is_age_locked(&self) -> bool {
        *self.entity_data.lock().ageable_mob().age_locked.get()
    }

    fn set_age_locked(&self, age_locked: bool) {
        self.entity_data
            .lock()
            .ageable_mob_mut()
            .age_locked
            .set(age_locked);
    }

    fn set_synced_baby(&self, baby: bool) {
        self.entity_data.lock().ageable_mob_mut().baby.set(baby);
    }
}

impl Mob for GlowSquidEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        AgeableMob::mob_interact_ageable(self, player, hand)
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GLOW_SQUID_AMBIENT)
    }

    fn ambient_sound_interval(&self) -> i32 {
        SQUID_AMBIENT_SOUND_INTERVAL
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_ageable_mob(world, spawn_reason, group_data)
    }
}

impl PathfinderMob for GlowSquidEntity {}
