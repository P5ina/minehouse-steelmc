//! Vanilla `Ghast`: a huge floating Nether mob that shoots explosive
//! fireballs, and dies to one sent back by a player.

pub(crate) mod flight;

#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::GhastEntityData;
use steel_registry::{level_events, sound_events, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, Downcast as _, DowncastType, DowncastTypeKey};

use self::flight::{GhastLookGoal, GhastMoveState, RandomFloatAroundGoal, tick_ghast_move_control};
use crate::enchantment_helper;
use crate::entity::ai::goal::{Goal, GoalControls, NearestAttackableTargetGoal};
use crate::entity::damage::DamageSource;
use crate::entity::entities::LargeFireballEntity;
use crate::entity::projectile::normalize_or_zero;
use crate::entity::{
    AbstractHurtingProjectile, DefaultEntityPush, Enemy, Entity, EntityBase, EntityBaseLoad,
    EntityMovementEmission, EntityPose, EntitySyncedData, LivingEntity, LivingEntityBase, Mob,
    MobBase, PathfinderMob, SharedEntity, SpawnRule, next_entity_id, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;
use rand::RngExt as _;
use steel_utils::types::Difficulty;

const XP_REWARD: i32 = 5;
const GHAST_SPAWN_CHANCE: i32 = 20;
const DEFAULT_EXPLOSION_POWER: i32 = 1;
const EXPLOSION_POWER_TAG: &str = "ExplosionPower";
const REFLECTED_FIREBALL_DAMAGE: f32 = 1000.0;
const SOUND_VOLUME: f32 = 5.0;
const FLYING_AIR_SPEED: f32 = 0.02;
const PLAYER_TARGET_MAX_Y_DIFFERENCE: f64 = 4.0;
const SHOOT_RANGE_SQR: f64 = 4096.0;
const WARNING_CHARGE_TIME: i32 = 10;
const SHOOT_CHARGE_TIME: i32 = 20;
const AFTER_SHOT_CHARGE_TIME: i32 = -40;
const FIREBALL_FORWARD_OFFSET: f64 = 4.0;

/// A ghast.
#[entity_behavior(class = "Ghast", spawn_rule)]
pub struct GhastEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<GhastEntityData>,
    move_state: SyncMutex<GhastMoveState>,
    explosion_power: SyncMutex<i32>,
}

// SAFETY: The owner-scoped type key uniquely identifies GhastEntity.
unsafe impl DowncastType for GhastEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/ghast");
}

impl GhastEntity {
    /// Vanilla `Ghast.checkGhastSpawnRules`: not on Peaceful, one roll in 20.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        level.difficulty() != Difficulty::Peaceful
            && random.random_range(0..GHAST_SPAWN_CHANCE) == 0
            && <Self as Mob>::check_mob_spawn_rules(entity_type, level, spawn_reason, pos)
    };

    /// Creates a new ghast.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved ghast.
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
        let mut entity_data = GhastEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(5, RandomFloatAroundGoal::new());
            goal_selector.add_goal(7, GhastLookGoal);
            goal_selector.add_goal(7, GhastShootFireballGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                1,
                NearestAttackableTargetGoal::new_for_players_from_mob(
                    10,
                    true,
                    false,
                    |target, mob, _| {
                        (target.position().y - mob.position().y).abs()
                            <= PLAYER_TARGET_MAX_Y_DIFFERENCE
                    },
                ),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            move_state: SyncMutex::new(GhastMoveState::default()),
            explosion_power: SyncMutex::new(DEFAULT_EXPLOSION_POWER),
        }
    }

    /// Whether the ghast is about to shoot (its mouth is open).
    #[must_use]
    pub fn is_charging(&self) -> bool {
        *self.entity_data.lock().is_charging.get()
    }

    /// Sets the charging flag shown to clients.
    pub fn set_charging(&self, charging: bool) {
        self.entity_data.lock().is_charging.set(charging);
    }

    /// Radius of the fireballs' explosions.
    #[must_use]
    pub fn explosion_power(&self) -> i32 {
        *self.explosion_power.lock()
    }

    /// Sets the radius of the fireballs' explosions.
    pub fn set_explosion_power(&self, explosion_power: i32) {
        *self.explosion_power.lock() = explosion_power;
    }
}

/// Vanilla `Ghast.isReflectedFireball`: a large fireball a player sent back.
fn is_reflected_fireball(source: &DamageSource) -> bool {
    source
        .direct_entity()
        .is_some_and(|entity| entity.downcast_ref::<LargeFireballEntity>().is_some())
        && source
            .causing_entity()
            .is_some_and(|entity| entity.entity_type() == &vanilla_entities::PLAYER)
}

impl DefaultEntityPush for GhastEntity {}

impl Enemy for GhastEntity {}

impl Entity for GhastEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn as_enemy(&self) -> Option<&dyn Enemy> {
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

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(LivingEntity::get_scale(self))
    }

    fn check_fall_damage(
        self: Arc<Self>,
        _vertical_movement: f64,
        _on_ground: bool,
        _on_state: BlockStateId,
        _pos: BlockPos,
        _world: &Arc<World>,
    ) {
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn update_data_before_sync(&self) {
        sync_dirty_mob_effects(&self.living_base, &self.entity_data);
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Hostile
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        // Vanilla stores the power as a byte.
        nbt.insert(EXPLOSION_POWER_TAG, self.explosion_power() as i8);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.set_explosion_power(
            nbt.byte(EXPLOSION_POWER_TAG)
                .map_or(DEFAULT_EXPLOSION_POWER, i32::from),
        );
    }
}

impl LivingEntity for GhastEntity {
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

    fn sound_volume(&self) -> f32 {
        SOUND_VOLUME
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GHAST_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GHAST_DEATH)
    }

    fn is_invulnerable_to(&self, _world: &World, source: &DamageSource) -> bool {
        self.is_invulnerable() && !source.is(&DamageTypeTag::BYPASSES_INVULNERABILITY)
            || !is_reflected_fireball(source)
                && (self.default_is_invulnerable_to(source)
                    || enchantment_helper::is_immune_to_damage(self, source))
    }

    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if is_reflected_fireball(source) {
            let _ = self.default_hurt_server(world, source, REFLECTED_FIREBALL_DAMAGE);
            return true;
        }
        !self.is_invulnerable_to(world, source) && self.default_hurt_server(world, source, amount)
    }

    fn travel(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        self.travel_flying(
            entity,
            input,
            FLYING_AIR_SPEED,
            FLYING_AIR_SPEED,
            FLYING_AIR_SPEED,
        )
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        Mob::mob_ai_step(self, entity)
    }
}

impl Mob for GhastEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_move_control(&self) {
        tick_ghast_move_control(self, &mut self.move_state.lock());
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_GHAST_AMBIENT)
    }

    fn max_spawn_cluster_size(&self) -> i32 {
        1
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

/// Steel runs goals on pathfinder mobs only; the ghast never walks a path.
impl PathfinderMob for GhastEntity {}

/// Vanilla `Ghast.GhastShootFireballGoal`: warn at half a second of sight,
/// shoot at one second, then cool down for two.
struct GhastShootFireballGoal {
    charge_time: i32,
}

impl GhastShootFireballGoal {
    const fn new() -> Self {
        Self { charge_time: 0 }
    }

    fn shoot(
        ghast: &GhastEntity,
        entity: &SharedEntity,
        world: &Arc<World>,
        target: &SharedEntity,
    ) {
        let view = ghast.calculate_view_vector(ghast.rotation().1, ghast.y_head_rot());
        let position = ghast.position();
        let target_position = target.position();
        let direction = DVec3::new(
            target_position.x - (position.x + view.x * FIREBALL_FORWARD_OFFSET),
            y_at(target.as_ref(), 0.5) - (0.5 + y_at(ghast, 0.5)),
            target_position.z - (position.z + view.z * FIREBALL_FORWARD_OFFSET),
        );
        if !ghast.is_silent() {
            world.level_event(
                level_events::SOUND_GHAST_FIREBALL,
                ghast.block_position(),
                0,
                None,
            );
        }
        let fireball = Arc::new(LargeFireballEntity::new(
            &vanilla_entities::FIREBALL,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        ));
        fireball.set_explosion_power(ghast.explosion_power());
        fireball.shoot_from_mob(entity, normalize_or_zero(direction));
        let spawn = DVec3::new(
            position.x + view.x * FIREBALL_FORWARD_OFFSET,
            y_at(ghast, 0.5) + 0.5,
            fireball.position().z + view.z * FIREBALL_FORWARD_OFFSET,
        );
        if let Err(error) = fireball.try_set_position(spawn) {
            log::debug!("ghast fireball could not move to its spawn point: {error}");
        }
        if let Err(error) = world.try_add_entity(fireball) {
            log::debug!("ghast fireball was not added to the world: {error:?}");
        }
    }
}

/// Vanilla `Entity.getY(scale)`.
fn y_at(entity: &dyn Entity, scale: f64) -> f64 {
    let bounding_box = entity.bounding_box();
    entity.position().y + (bounding_box.max_y() - bounding_box.min_y()) * scale
}

impl Goal for GhastShootFireballGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_some() && mob.downcast_ref::<GhastEntity>().is_some()
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.charge_time = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(ghast) = mob.downcast_ref::<GhastEntity>() {
            ghast.set_charging(false);
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(ghast) = mob.downcast_ref::<GhastEntity>() else {
            return;
        };
        let Some(target) = mob.target() else {
            return;
        };
        let Some(world) = ghast.level() else {
            return;
        };
        if target.distance_to_sqr(ghast.position()) < SHOOT_RANGE_SQR
            && ghast.has_line_of_sight(target.as_ref())
        {
            self.charge_time += 1;
            if self.charge_time == WARNING_CHARGE_TIME && !ghast.is_silent() {
                world.level_event(
                    level_events::SOUND_GHAST_WARNING,
                    ghast.block_position(),
                    0,
                    None,
                );
            }
            if self.charge_time == SHOOT_CHARGE_TIME {
                GhastShootFireballGoal::shoot(ghast, entity, &world, &target);
                self.charge_time = AFTER_SHOT_CHARGE_TIME;
            }
        } else if self.charge_time > 0 {
            self.charge_time -= 1;
        }
        ghast.set_charging(self.charge_time > WARNING_CHARGE_TIME);
    }
}
