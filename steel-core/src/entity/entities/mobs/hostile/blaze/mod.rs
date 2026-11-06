//! Vanilla `Blaze`: a hovering Nether monster that shoots volleys of small
//! fireballs.

use std::iter::empty;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::BlazeEntityData;
use steel_registry::{level_events, sound_events, vanilla_attributes, vanilla_entities};
use steel_utils::locks::SyncMutex;
use steel_utils::{Downcast as _, DowncastType, DowncastTypeKey};

use crate::entity::ai::goal::{
    Goal, GoalControls, HurtByTargetGoal, LookAtPlayerGoal, MoveTowardsRestrictionGoal,
    NearestAttackableTargetGoal, RandomLookAroundGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::entities::SmallFireballEntity;
use crate::entity::projectile::{normalize_or_zero, triangle_random};
use crate::entity::{
    AbstractHurtingProjectile, DefaultEntityPush, Entity, EntityBase, EntityBaseLoad,
    EntityMovementEmission, EntityPose, EntitySyncedData, LivingEntity, LivingEntityBase, Mob,
    MobBase, Monster, PathfinderMob, SharedEntity, SpawnRule, next_entity_id,
    sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::World;

const XP_REWARD: i32 = 10;
const CHARGED_FLAG: i8 = 1;
const HEIGHT_OFFSET_CHANGE_TICKS: i32 = 100;
const FALL_SLOWDOWN: f64 = 0.6;
const RISE_TARGET_SPEED: f32 = 0.3;
const MELEE_DISTANCE_SQR: f64 = 4.0;
const MELEE_COOLDOWN: i32 = 20;
const CHARGE_TICKS: i32 = 60;
const SHOT_INTERVAL: i32 = 6;
const VOLLEY_COOLDOWN: i32 = 100;
const LAST_VOLLEY_STEP: i32 = 4;
const LOST_SIGHT_CHASE_TICKS: i32 = 5;
const FIREBALL_SPREAD: f64 = 2.297;

/// A blaze.
#[entity_behavior(class = "Blaze", spawn_rule)]
pub struct BlazeEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<BlazeEntityData>,
    hover: SyncMutex<BlazeHover>,
}

/// Vanilla `allowedHeightOffset` / `nextHeightOffsetChangeTick`.
struct BlazeHover {
    allowed_height_offset: f32,
    next_height_offset_change_tick: i32,
}

// SAFETY: The owner-scoped type key uniquely identifies BlazeEntity.
unsafe impl DowncastType for BlazeEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/blaze");
}

impl BlazeEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkAnyLightMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, _random| {
        <Self as Monster>::check_any_light_monster_spawn_rules(
            entity_type,
            level,
            spawn_reason,
            pos,
        )
    };

    /// Creates a new blaze.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved blaze.
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
        {
            let mut malus = mob_base.pathfinding_malus().lock();
            malus.set(PathType::Water, -1.0);
            malus.set(PathType::Lava, 8.0);
            malus.set(PathType::FireInNeighbor, 0.0);
            malus.set(PathType::Fire, 0.0);
        }
        let mut entity_data = BlazeEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            goal_selector.add_goal(4, BlazeAttackGoal::new());
            goal_selector.add_goal(5, MoveTowardsRestrictionGoal::new(1.0));
            goal_selector.add_goal(7, WaterAvoidingRandomStrollGoal::with_probability(1.0, 0.0));
            goal_selector.add_goal(8, LookAtPlayerGoal::new(8.0));
            goal_selector.add_goal(8, RandomLookAroundGoal::new());

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(1, HurtByTargetGoal::new().set_alert_others(empty()));
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
            hover: SyncMutex::new(BlazeHover {
                allowed_height_offset: 0.5,
                next_height_offset_change_tick: 0,
            }),
        }
    }

    /// Whether the blaze is charging or firing a volley (drawn on fire).
    #[must_use]
    pub fn is_charged(&self) -> bool {
        *self.entity_data.lock().flags.get() & CHARGED_FLAG != 0
    }

    fn set_charged(&self, charged: bool) {
        let mut data = self.entity_data.lock();
        let flags = *data.flags.get();
        data.flags.set(if charged {
            flags | CHARGED_FLAG
        } else {
            flags & !CHARGED_FLAG
        });
    }

    /// Vanilla `customServerAiStep`: rise toward a target that is above the
    /// randomly drifting hover height.
    fn hover_towards_target(&self) {
        let allowed_height_offset = {
            let mut hover = self.hover.lock();
            hover.next_height_offset_change_tick -= 1;
            if hover.next_height_offset_change_tick <= 0 {
                hover.next_height_offset_change_tick = HEIGHT_OFFSET_CHANGE_TICKS;
                hover.allowed_height_offset = triangle_random(0.5, 6.891) as f32;
            }
            hover.allowed_height_offset
        };
        let Some(target) = self.target() else {
            return;
        };
        let Some(living_target) = target.as_living_entity() else {
            return;
        };
        if target.get_eye_y() > self.get_eye_y() + f64::from(allowed_height_offset)
            && Mob::can_attack(self, living_target)
        {
            let movement = self.velocity();
            // Vanilla `(0.3F - movement.y) * 0.3F`, widened to double.
            let rise = f64::from(RISE_TARGET_SPEED);
            self.set_velocity(movement + DVec3::new(0.0, (rise - movement.y) * rise, 0.0));
            self.mark_velocity_sync();
        }
    }
}

impl DefaultEntityPush for BlazeEntity {}

impl Entity for BlazeEntity {
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

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.entity_type
            .dimensions
            .scale(LivingEntity::get_scale(self))
    }

    fn is_on_fire(&self) -> bool {
        self.is_charged()
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

impl LivingEntity for BlazeEntity {
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
        Some(&sound_events::ENTITY_BLAZE_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_BLAZE_DEATH)
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
        let movement = self.velocity();
        if !self.on_ground() && movement.y < 0.0 {
            self.set_velocity(DVec3::new(
                movement.x,
                movement.y * FALL_SLOWDOWN,
                movement.z,
            ));
        }
        // VANILLA CLIENT-LOCAL: the burn sound and smoke particles play on clients.
        Monster::monster_ai_step(self, entity)
    }
}

impl Mob for BlazeEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn custom_server_ai_step(&self) {
        self.hover_towards_target();
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_BLAZE_AMBIENT)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl Monster for BlazeEntity {
    fn light_level_dependent_magic_value(&self) -> f32 {
        1.0
    }
}

impl PathfinderMob for BlazeEntity {}

/// Vanilla `Blaze.BlazeAttackGoal`: melee up close, otherwise charge for 3 s
/// and fire three small fireballs, then rest for 5 s.
struct BlazeAttackGoal {
    attack_step: i32,
    attack_time: i32,
    last_seen: i32,
}

impl BlazeAttackGoal {
    const fn new() -> Self {
        Self {
            attack_step: 0,
            attack_time: 0,
            last_seen: 0,
        }
    }

    fn shoot_fireball(blaze: &BlazeEntity, entity: &SharedEntity, target: &SharedEntity) {
        let Some(world) = blaze.level() else {
            return;
        };
        let distance = blaze.distance_to_sqr(target.position());
        let target_position = target.position();
        let blaze_position = blaze.position();
        let xd = target_position.x - blaze_position.x;
        let yd = y_at(target.as_ref(), 0.5) - y_at(blaze, 0.5);
        let zd = target_position.z - blaze_position.z;
        let spread = distance.sqrt().sqrt() * 0.5;
        if !blaze.is_silent() {
            world.level_event(
                level_events::SOUND_BLAZE_FIREBALL,
                blaze.block_position(),
                0,
                None,
            );
        }
        let direction = DVec3::new(
            triangle_random(xd, FIREBALL_SPREAD * spread),
            yd,
            triangle_random(zd, FIREBALL_SPREAD * spread),
        );
        let fireball = Arc::new(SmallFireballEntity::new(
            &vanilla_entities::SMALL_FIREBALL,
            next_entity_id(),
            blaze_position,
            Arc::downgrade(&world),
        ));
        fireball.shoot_from_mob(entity, normalize_or_zero(direction));
        let spawn = DVec3::new(blaze_position.x, y_at(blaze, 0.5) + 0.5, blaze_position.z);
        if let Err(error) = fireball.try_set_position(spawn) {
            log::debug!("blaze fireball could not move to its spawn point: {error}");
        }
        if let Err(error) = world.try_add_entity(fireball) {
            log::debug!("blaze fireball was not added to the world: {error:?}");
        }
    }
}

/// Vanilla `Entity.getY(scale)`.
fn y_at(entity: &dyn Entity, scale: f64) -> f64 {
    let bounding_box = entity.bounding_box();
    entity.position().y + (bounding_box.max_y() - bounding_box.min_y()) * scale
}

fn blaze(mob: &dyn PathfinderMob) -> Option<&BlazeEntity> {
    mob.downcast_ref::<BlazeEntity>()
}

impl Goal for BlazeAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(target) = mob.target() else {
            return false;
        };
        let Some(living) = target.as_living_entity() else {
            return false;
        };
        LivingEntity::is_alive(living) && Mob::can_attack(mob, living) && blaze(mob).is_some()
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.attack_step = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(blaze) = blaze(mob) {
            blaze.set_charged(false);
        }
        self.last_seen = 0;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.attack_time -= 1;
        let Some(blaze) = blaze(mob) else {
            return;
        };
        let Some(target) = mob.target() else {
            return;
        };
        let has_line_of_sight = mob.has_line_of_sight_cached(target.as_ref());
        if has_line_of_sight {
            self.last_seen = 0;
        } else {
            self.last_seen += 1;
        }

        let distance = mob.distance_to_sqr(target.position());
        let follow_distance = mob
            .attributes()
            .lock()
            .required_value(vanilla_attributes::FOLLOW_RANGE);
        if distance < MELEE_DISTANCE_SQR {
            if !has_line_of_sight {
                return;
            }
            if self.attack_time <= 0 {
                self.attack_time = MELEE_COOLDOWN;
                if let Some(world) = mob.level() {
                    let _ = mob.do_hurt_target(entity, &world, &target);
                }
            }
            mob.set_wanted_position(target.position(), 1.0);
        } else if distance < follow_distance * follow_distance && has_line_of_sight {
            if self.attack_time <= 0 {
                self.attack_step += 1;
                if self.attack_step == 1 {
                    self.attack_time = CHARGE_TICKS;
                    blaze.set_charged(true);
                } else if self.attack_step <= LAST_VOLLEY_STEP {
                    self.attack_time = SHOT_INTERVAL;
                } else {
                    self.attack_time = VOLLEY_COOLDOWN;
                    self.attack_step = 0;
                    blaze.set_charged(false);
                }
                if self.attack_step > 1 {
                    BlazeAttackGoal::shoot_fireball(blaze, entity, &target);
                }
            }
            let target_position = target.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
                10.0,
                10.0,
            );
        } else if self.last_seen < LOST_SIGHT_CHASE_TICKS {
            mob.set_wanted_position(target.position(), 1.0);
        }
    }
}

#[cfg(test)]
mod tests;
