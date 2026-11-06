//! Ravager (Vanilla `Ravager`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_entity_data::RavagerEntityData;
use steel_registry::vanilla_entity_type_tags::EntityTypeTag;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_damage_types,
    vanilla_entities, vanilla_game_events,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, WorldAabb};

use crate::entity::SpawnRule;
use crate::entity::ai::goal::{
    FloatGoal, GoalControl, HurtByTargetGoal, LongDistancePatrolGoal, LookAtPlayerGoal,
    MeleeAttackGoal, NearestAttackableTargetGoal, WaterAvoidingRandomStrollGoal,
};
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::spawn::SpawnGroupData;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntityPose,
    EntitySpawnReason, EntitySyncedData, LivingEntity, LivingEntityBase, Mob, MobBase, Monster,
    PathfinderMob, PatrollingMonster, PatrollingMonsterState, Raider, RaiderState, SharedEntity,
    is_abstract_illager, is_abstract_villager, level_contains_any_liquid, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::world::{ServerLevelAccessor, World};

const XP_REWARD: i32 = 20;
const DEFAULT_STEP_HEIGHT: f32 = 1.0;
const BASE_MOVEMENT_SPEED: f64 = 0.3;
const ATTACK_MOVEMENT_SPEED: f64 = 0.35;
/// Vanilla `ATTACK_DURATION`.
const ATTACK_DURATION: i32 = 10;
/// Ticks of the roar wind-up started when a stun ends.
const ROAR_DURATION: i32 = 20;
/// The roar hits on this tick of the wind-up countdown.
const ROAR_HIT_TICK: i32 = 10;
const ROAR_RADIUS: f64 = 4.0;
const ROAR_DAMAGE: f32 = 6.0;
/// Vanilla `MeleeAttackGoal(this, 1.0, true)`.
const MELEE_SPEED: f64 = 1.0;
/// Vanilla `WaterAvoidingRandomStrollGoal(this, 0.4)`.
const STROLL_SPEED: f64 = 0.4;
const LOOK_AT_PLAYER_DISTANCE: f64 = 6.0;
const LOOK_AT_MOB_DISTANCE: f64 = 8.0;
/// Vanilla `LookAtPlayerGoal` default probability.
const LOOK_AT_MOB_PROBABILITY: f32 = 0.02;
/// Vanilla `getAttackBoundingBox(...).deflate(0.05, 0.0, 0.05)`.
const ATTACK_BOX_DEFLATE: f64 = 0.05;
const TAG_ATTACK_TICK: &str = "AttackTick";
const TAG_STUN_TICK: &str = "StunTick";
const TAG_ROAR_TICK: &str = "RoarTick";

fn is_raider(entity: &SharedEntity) -> bool {
    entity.as_raider().is_some()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct RavagerTicks {
    attack: i32,
    stunned: i32,
    roar: i32,
}

/// A ravager: the raid beast that tramples leaves and roars after a stun.
#[entity_behavior(class = "Ravager", spawn_rule)]
pub struct RavagerEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    patrolling: PatrollingMonsterState,
    raider: RaiderState,
    entity_data: SyncMutex<RavagerEntityData>,
    ticks: SyncMutex<RavagerTicks>,
}

// SAFETY: The owner-scoped type key uniquely identifies RavagerEntity.
unsafe impl DowncastType for RavagerEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/ravager");
}

impl RavagerEntity {
    /// Vanilla `SpawnPlacements` predicate: `Monster.checkMonsterSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |entity_type, level, spawn_reason, pos, random| {
        <Self as Monster>::check_monster_spawn_rules(entity_type, level, spawn_reason, pos, random)
    };

    /// Creates a new ravager.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved ravager.
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
        mob_base
            .pathfinding_malus()
            .lock()
            .set(PathType::Leaves, 0.0);
        let mut entity_data = RavagerEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);

        {
            let mut goal_selector = mob_base.goal_selector().lock();
            // TODO: Raider goals (`ObtainRaidLeaderBannerGoal` 1, `PathfindToRaidGoal` 3,
            // `RaiderMoveThroughVillageGoal` 4, `RaiderCelebration` 5) come with raids.
            // Vanilla `PatrollingMonster.registerGoals`.
            goal_selector.add_goal(4, LongDistancePatrolGoal::new(0.7, 0.595));
            goal_selector.add_goal(0, FloatGoal::new(&mob_base));
            goal_selector.add_goal(4, MeleeAttackGoal::new(MELEE_SPEED, true));
            goal_selector.add_goal(5, WaterAvoidingRandomStrollGoal::new(STROLL_SPEED));
            goal_selector.add_goal(6, LookAtPlayerGoal::new(LOOK_AT_PLAYER_DISTANCE));
            goal_selector.add_goal(
                10,
                LookAtPlayerGoal::new_for_living_entities(
                    LOOK_AT_MOB_DISTANCE,
                    LOOK_AT_MOB_PROBABILITY,
                    |target, _| target.as_mob().is_some(),
                ),
            );

            let mut target_selector = mob_base.target_selector().lock();
            target_selector.add_goal(
                2,
                HurtByTargetGoal::new()
                    .with_ignored_damage_selector(is_raider)
                    .set_alert_others([]),
            );
            target_selector.add_goal(
                3,
                NearestAttackableTargetGoal::new_for_players(true, |_, _| true),
            );
            target_selector.add_goal(
                4,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    is_abstract_villager(target) && !target.is_baby()
                }),
            );
            target_selector.add_goal(
                4,
                NearestAttackableTargetGoal::new(true, |target, _| {
                    target.entity_type() == &vanilla_entities::IRON_GOLEM
                }),
            );
        }

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            patrolling: PatrollingMonsterState::new(),
            raider: RaiderState::new(),
            entity_data: SyncMutex::new(entity_data),
            ticks: SyncMutex::new(RavagerTicks::default()),
        }
    }

    /// Vanilla `getAttackTick`.
    #[must_use]
    pub fn attack_tick(&self) -> i32 {
        self.ticks.lock().attack
    }

    /// Vanilla `getStunnedTick`.
    #[must_use]
    pub fn stunned_tick(&self) -> i32 {
        self.ticks.lock().stunned
    }

    /// Vanilla `getRoarTick`.
    #[must_use]
    pub fn roar_tick(&self) -> i32 {
        self.ticks.lock().roar
    }

    /// The movement-speed easing at the start of vanilla `aiStep`.
    fn update_movement_speed(&self) {
        let immobile = self.is_immobile();
        let has_target = self.target().is_some();
        let mut attributes = self.attributes().lock();
        if immobile {
            attributes.set_base_value(vanilla_attributes::MOVEMENT_SPEED, 0.0);
        } else {
            let max_speed = if has_target {
                ATTACK_MOVEMENT_SPEED
            } else {
                BASE_MOVEMENT_SPEED
            };
            let base_value = attributes
                .get_base_value(vanilla_attributes::MOVEMENT_SPEED)
                .unwrap_or(BASE_MOVEMENT_SPEED);
            attributes.set_base_value(
                vanilla_attributes::MOVEMENT_SPEED,
                base_value + 0.1 * (max_speed - base_value),
            );
        }
    }

    /// Breaks leaves the ravager walks into, or hops when nothing broke.
    fn trample_leaves(&self, world: &Arc<World>) {
        if !self.horizontal_collision() || !world.get_game_rule(&MOB_GRIEFING) {
            return;
        }
        let bb = self.bounding_box().inflate(0.2);
        let mut destroyed_block = false;
        // Vanilla `BlockPos.betweenClosed` of the floored corners.
        for x in bb.min_x().floor() as i32..=bb.max_x().floor() as i32 {
            for y in bb.min_y().floor() as i32..=bb.max_y().floor() as i32 {
                for z in bb.min_z().floor() as i32..=bb.max_z().floor() as i32 {
                    let pos = BlockPos::new(x, y, z);
                    // VANILLA: `block instanceof LeavesBlock`; the vanilla `leaves`
                    // tag lists exactly the `LeavesBlock` blocks.
                    if world
                        .get_block_state(pos)
                        .get_block()
                        .has_tag(&BlockTag::LEAVES)
                    {
                        // TODO: vanilla passes the ravager as the breaker for the
                        // block-destroy game event.
                        destroyed_block = world.destroy_block(pos, true) || destroyed_block;
                    }
                }
            }
        }
        if !destroyed_block && self.on_ground() {
            self.jump_from_ground();
        }
    }

    /// The roar, stun and attack countdowns from vanilla `aiStep`.
    fn tick_countdowns(&self, entity: &SharedEntity, world: &World) {
        let roar_now = {
            let mut ticks = self.ticks.lock();
            let mut roar_now = false;
            if ticks.roar > 0 {
                ticks.roar -= 1;
                roar_now = ticks.roar == ROAR_HIT_TICK;
            }
            roar_now
        };
        if roar_now {
            self.roar(entity, world);
        }

        let stun_ended = {
            let mut ticks = self.ticks.lock();
            if ticks.attack > 0 {
                ticks.attack -= 1;
            }
            if ticks.stunned > 0 {
                ticks.stunned -= 1;
                // Vanilla `stunEffect` only spawns client particles here: the
                // server level's `addParticle` does nothing.
                if ticks.stunned == 0 {
                    ticks.roar = ROAR_DURATION;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if stun_ended {
            self.play_sound(&sound_events::ENTITY_RAVAGER_ROAR, 1.0, 1.0);
        }
    }

    /// Vanilla `roar`: hurt and throw back everything nearby.
    fn roar(&self, entity: &SharedEntity, world: &World) {
        if !LivingEntity::is_alive(self) {
            return;
        }
        let mob_griefing = world.get_game_rule(&MOB_GRIEFING);
        let area = self.bounding_box().inflate(ROAR_RADIUS);
        let targets = world.get_entities_in_aabb_matching(&area, |candidate| {
            candidate.is_living_entity()
                && candidate.entity_type() != &vanilla_entities::RAVAGER
                && candidate.is_alive()
                && (mob_griefing || candidate.entity_type() != &vanilla_entities::ARMOR_STAND)
        });
        let source = DamageSource::environment(&vanilla_damage_types::MOB_ATTACK)
            .with_causing_entity(Arc::clone(entity))
            .with_direct_entity(Arc::clone(entity));
        for target in targets {
            if !is_abstract_illager(target.as_ref()) {
                let _ = target.hurt(world, &source, ROAR_DAMAGE);
            }
            if target.as_player().is_none() {
                self.strong_knockback(target.as_ref());
            }
        }
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.broadcast_entity_event(EntityStatus::RavagerRoared);
    }

    /// Vanilla `strongKnockback`.
    fn strong_knockback(&self, target: &dyn Entity) {
        let xd = target.position().x - self.position().x;
        let zd = target.position().z - self.position().z;
        let dd = xd.mul_add(xd, zd * zd).max(0.001);
        target.push_impulse(DVec3::new(xd / dd * 4.0, 0.2, zd / dd * 4.0));
    }
}

impl DefaultEntityPush for RavagerEntity {}

impl Entity for RavagerEntity {
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

    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {
        self.play_sound(&sound_events::ENTITY_RAVAGER_STEP, 0.15, 1.0);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        self.save_raider(nbt);
        let ticks = *self.ticks.lock();
        nbt.insert(TAG_ATTACK_TICK, ticks.attack);
        nbt.insert(TAG_STUN_TICK, ticks.stunned);
        nbt.insert(TAG_ROAR_TICK, ticks.roar);
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.load_raider(nbt);
        *self.ticks.lock() = RavagerTicks {
            attack: nbt.int(TAG_ATTACK_TICK).unwrap_or(0),
            stunned: nbt.int(TAG_STUN_TICK).unwrap_or(0),
            roar: nbt.int(TAG_ROAR_TICK).unwrap_or(0),
        };
    }
}

impl LivingEntity for RavagerEntity {
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
        Some(&sound_events::ENTITY_RAVAGER_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_RAVAGER_DEATH)
    }

    fn should_drop_experience(&self) -> bool {
        Monster::should_drop_experience_monster(self)
    }

    fn should_drop_loot(&self, world: &World) -> bool {
        Monster::should_drop_loot_monster(self, world)
    }

    fn is_immobile(&self) -> bool {
        let ticks = *self.ticks.lock();
        self.default_is_immobile() || ticks.attack > 0 || ticks.stunned > 0 || ticks.roar > 0
    }

    fn has_line_of_sight(&self, target: &dyn Entity) -> bool {
        let ticks = *self.ticks.lock();
        ticks.stunned <= 0 && ticks.roar <= 0 && self.default_has_line_of_sight(target)
    }

    // TODO: Vanilla `blockedByItem` (a shield block stuns the ravager half the
    // time, otherwise throws the defender back) comes with item blocking in
    // `hurtServer`.

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Monster::monster_ai_step(self, entity);
        if LivingEntity::is_alive(self) {
            self.update_movement_speed();
            if let Some(world) = self.level() {
                self.trample_leaves(&world);
                self.tick_countdowns(entity, &world);
            }
        }
        result
    }
}

impl Mob for RavagerEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_path_navigation(&self) {
        PathfinderMob::tick_pathfinder_path_navigation(self);
    }

    fn update_control_flags(&self) {
        let no_controller = self.controlling_passenger().is_none_or(|passenger| {
            !passenger.is_mob()
                || REGISTRY
                    .entity_types
                    .is_in_tag(passenger.entity_type(), &EntityTypeTag::RAIDERS)
        });
        let not_in_boat = self
            .vehicle()
            .is_none_or(|vehicle| !vehicle.entity_type().is_abstract_boat);

        let mut selector = self.mob_base.goal_selector().lock();
        selector.set_control(GoalControl::Move, no_controller);
        selector.set_control(GoalControl::Jump, no_controller && not_in_boat);
        selector.set_control(GoalControl::Look, no_controller);
        selector.set_control(GoalControl::Target, no_controller);
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_RAVAGER_AMBIENT)
    }

    fn max_head_y_rot(&self) -> f32 {
        45.0
    }

    fn do_hurt_target(&self, entity: &SharedEntity, world: &World, target: &SharedEntity) -> bool {
        self.ticks.lock().attack = ATTACK_DURATION;
        self.broadcast_entity_event(EntityStatus::StartAttacking);
        self.play_sound(&sound_events::ENTITY_RAVAGER_ATTACK, 1.0, 1.0);
        self.default_do_hurt_target(entity, world, target)
    }

    fn attack_bounding_box(&self, horizontal_expansion: f64) -> WorldAabb {
        self.default_attack_bounding_box(horizontal_expansion)
            .inflate_xyz(-ATTACK_BOX_DEFLATE, 0.0, -ATTACK_BOX_DEFLATE)
    }

    fn check_spawn_obstruction(&self, level: &dyn ServerLevelAccessor) -> bool {
        !level_contains_any_liquid(level, self.bounding_box())
    }

    fn remove_when_far_away(&self, dist_sqr: f64) -> bool {
        self.remove_when_far_away_raider(dist_sqr)
    }

    fn finalize_spawn(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_raider(spawn_reason);
        self.finalize_spawn_mob_base(world, spawn_reason, group_data)
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().raider.mob.mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().raider.mob.mob_flags.set(flags);
    }
}

impl PathfinderMob for RavagerEntity {}

impl Monster for RavagerEntity {
    fn update_no_action_time(&self) {
        self.update_no_action_time_raider();
    }
}

impl PatrollingMonster for RavagerEntity {
    fn patrolling_monster_state(&self) -> &PatrollingMonsterState {
        &self.patrolling
    }

    fn can_be_leader(&self) -> bool {
        false
    }

    fn can_join_patrol(&self) -> bool {
        self.can_join_patrol_raider()
    }
}

impl Raider for RavagerEntity {
    fn raider_state(&self) -> &RaiderState {
        &self.raider
    }

    fn is_celebrating(&self) -> bool {
        *self.entity_data.lock().raider.is_celebrating.get()
    }

    fn set_celebrating(&self, celebrating: bool) {
        self.entity_data
            .lock()
            .raider
            .is_celebrating
            .set(celebrating);
    }

    fn celebrate_sound(&self) -> SoundEventRef {
        &sound_events::ENTITY_RAVAGER_CELEBRATE
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Weak};

    use glam::DVec3;
    use simdnbt::borrow::read_compound;
    use simdnbt::owned::NbtCompound;
    use steel_registry::blocks::block_state_ext::BlockStateExt as _;
    use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities};
    use steel_utils::types::UpdateFlags;
    use steel_utils::{BlockPos, ChunkPos};

    use super::RavagerEntity;
    use crate::behavior::init_behaviors;
    use crate::entity::entities::{CowEntity, VindicatorEntity};
    use crate::entity::{Entity, LivingEntity, SharedEntity, next_entity_id};
    use crate::test_support::{fresh_test_world, insert_ready_full_chunk};
    use crate::world::World;

    fn add<T: Entity + 'static>(world: &Arc<World>, entity: T) -> (Arc<T>, SharedEntity) {
        let entity = Arc::new(entity);
        let shared: SharedEntity = Arc::<T>::clone(&entity);
        world
            .try_add_entity(Arc::clone(&shared))
            .expect("test entity should attach to the loaded chunk");
        (entity, shared)
    }

    fn ravager(world: &Arc<World>, position: DVec3) -> (Arc<RavagerEntity>, SharedEntity) {
        add(
            world,
            RavagerEntity::new(
                &vanilla_entities::RAVAGER,
                next_entity_id(),
                position,
                Arc::downgrade(world),
            ),
        )
    }

    fn stone_floor(world: &Arc<World>) {
        let stone = vanilla_blocks::STONE.default_state();
        for x in 0..16 {
            for z in 0..16 {
                world.set_block(BlockPos::new(x, 64, z), stone, UpdateFlags::UPDATE_ALL);
            }
        }
    }

    fn load_ticks(mob: &RavagerEntity, attack: i32, stun: i32, roar: i32) {
        let mut nbt = NbtCompound::new();
        nbt.insert("AttackTick", attack);
        nbt.insert("StunTick", stun);
        nbt.insert("RoarTick", roar);
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes))
            .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
        mob.load_additional((&borrowed).into());
    }

    #[test]
    fn countdowns_save_and_freeze_the_ravager() {
        init_vanilla_registry();
        let mob = RavagerEntity::new(&vanilla_entities::RAVAGER, 1, DVec3::ZERO, Weak::new());
        assert!(!mob.is_immobile());
        load_ticks(&mob, 3, 7, 11);
        assert_eq!(
            (mob.attack_tick(), mob.stunned_tick(), mob.roar_tick()),
            (3, 7, 11)
        );
        assert!(mob.is_immobile());

        let mut nbt = NbtCompound::new();
        mob.save_additional(&mut nbt);
        assert_eq!(nbt.int("AttackTick"), Some(3));
        assert_eq!(nbt.int("StunTick"), Some(7));
        assert_eq!(nbt.int("RoarTick"), Some(11));
    }

    #[test]
    fn roar_after_a_stun_hurts_nearby_mobs_but_spares_illagers() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("ravager_roar");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        stone_floor(world);

        let (ravager, ravager_entity) = ravager(world, DVec3::new(8.5, 65.0, 8.5));
        let (cow, _) = add(
            world,
            CowEntity::new(
                &vanilla_entities::COW,
                next_entity_id(),
                DVec3::new(10.5, 65.0, 8.5),
                Arc::downgrade(world),
            ),
        );
        let (vindicator, _) = add(
            world,
            VindicatorEntity::new(
                &vanilla_entities::VINDICATOR,
                next_entity_id(),
                DVec3::new(6.5, 65.0, 8.5),
                Arc::downgrade(world),
            ),
        );
        load_ticks(&ravager, 0, 1, 0);

        // The stun ends on the first tick and starts a 20-tick roar wind-up
        // that hits on its tenth tick.
        for _ in 0..10 {
            Arc::clone(&ravager_entity).tick();
            assert!(cow.get_health() >= cow.get_max_health(), "no roar yet");
        }
        Arc::clone(&ravager_entity).tick();
        assert!(
            cow.get_health() < cow.get_max_health(),
            "the roar hurts the cow"
        );
        assert!(
            cow.velocity().x > 0.0,
            "the roar throws the cow away from the ravager"
        );
        assert!(
            vindicator.get_health() >= vindicator.get_max_health(),
            "illagers take no roar damage"
        );
        assert!(
            vindicator.velocity().x < 0.0,
            "illagers are still thrown back"
        );
    }

    #[test]
    fn walking_into_leaves_tramples_them() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("ravager_leaves");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        stone_floor(world);
        let leaves = vanilla_blocks::OAK_LEAVES.default_state();
        for y in 65..68 {
            for z in 6..11 {
                world.set_block(BlockPos::new(10, y, z), leaves, UpdateFlags::UPDATE_ALL);
            }
        }

        // Ravagers are 1.95 wide: centered 0.98 from the leaves, they touch them.
        let (_, ravager_entity) = ravager(world, DVec3::new(9.02, 65.0, 8.5));
        ravager_entity.set_velocity(DVec3::new(0.2, 0.0, 0.0));
        Arc::clone(&ravager_entity).tick();

        assert!(
            world.get_block_state(BlockPos::new(10, 65, 8)).is_air(),
            "the ravager breaks the leaves it runs into"
        );
    }
}
