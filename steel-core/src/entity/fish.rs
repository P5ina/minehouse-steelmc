//! Shared vanilla `WaterAnimal`, `AbstractFish` and `AbstractSchoolingFish` layers.

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::Rng;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_math::RAD_TO_DEG;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::{vanilla_attributes, vanilla_blocks, vanilla_damage_types};
use steel_utils::BlockPos;
use steel_utils::locks::SyncMutex;

use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::{AvoidEntityGoal, FishSwimGoal, GoalSelector, PanicGoal};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::PathType;
use crate::entity::damage::DamageSource;
use crate::entity::mob::rotlerp;
use crate::entity::{
    Entity, EntitySpawnReason, LivingEntity, Mob, MobBase, PathfinderMob, SharedEntity,
    SpawnGroupData,
};
use crate::physics::{MoveResult, MoverType};
use crate::world::{ServerLevelAccessor, World};

/// Vanilla `WaterAnimal.AMBIENT_SOUND_INTERVAL`.
pub const WATER_ANIMAL_AMBIENT_SOUND_INTERVAL: i32 = 120;
const FISH_MAX_HEALTH: f64 = 3.0;
const FISH_MAX_SPAWN_CLUSTER_SIZE: i32 = 8;
const FISH_AVOID_PLAYER_DISTANCE: f32 = 8.0;
const FISH_MOVE_CONTROL_MAX_TURN: f32 = 90.0;
const MAX_AIR_SUPPLY: i32 = 300;
const SCHOOL_SEARCH_RADIUS: f64 = 8.0;
const SCHOOL_LEADER_RANGE_SQR: f64 = 121.0;
/// How far below sea level surface water creatures still spawn.
const SURFACE_SPAWN_DEPTH: i32 = 13;

/// Vanilla `WaterAnimal.checkSurfaceWaterAnimalSpawnRules`: at most 13 blocks
/// below sea level, with water below and a water block above.
pub(crate) fn check_surface_water_animal_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    let sea_level = level.sea_level();
    pos.y() >= sea_level - SURFACE_SPAWN_DEPTH
        && pos.y() <= sea_level
        && level
            .get_block_state(pos.below())
            .get_fluid_state()
            .fluid_id
            .has_tag(&FluidTag::WATER)
        && level.get_block_state(pos.above()).get_block() == &vanilla_blocks::WATER
}

/// Vanilla `AgeableWaterCreature.checkSurfaceAgeableWaterCreatureSpawnRules`,
/// the same check as [`check_surface_water_animal_spawn_rules`].
pub(crate) fn check_surface_ageable_water_creature_spawn_rules(
    entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    random: &mut dyn Rng,
) -> bool {
    check_surface_water_animal_spawn_rules(entity_type, level, spawn_reason, pos, random)
}

/// Configures a vanilla `WaterAnimal`: water costs nothing to path through.
pub fn init_water_animal(mob_base: &MobBase) {
    mob_base
        .pathfinding_malus()
        .lock()
        .set(PathType::Water, 0.0);
}

/// Configures a vanilla `AbstractFish` mob base: water-bound navigation, health
/// and the shared fish goals.
pub fn init_abstract_fish(mob_base: &MobBase, living_max_health: impl FnOnce(f64)) {
    init_water_animal(mob_base);
    living_max_health(FISH_MAX_HEALTH);
    *mob_base.navigation().lock() = PathNavigation::water_bound(false);
    register_abstract_fish_goals(&mut mob_base.goal_selector().lock());
}

fn register_abstract_fish_goals(goal_selector: &mut GoalSelector) {
    goal_selector.add_goal(0, PanicGoal::new(1.25));
    goal_selector.add_goal(
        2,
        AvoidEntityGoal::with_selector(FISH_AVOID_PLAYER_DISTANCE, 1.6, 1.4, |target, _| {
            target.as_player().is_some() && !target.is_spectator()
        }),
    );
    goal_selector.add_goal(4, FishSwimGoal::new());
}

/// Vanilla `WaterAnimal` air handling: out of water the air supply drains and
/// the mob drowns, in water it refills at once.
pub fn handle_water_animal_air_supply<M: LivingEntity + ?Sized>(mob: &M, pre_tick_air_supply: i32) {
    if LivingEntity::is_alive(mob) && !mob.is_in_water() {
        mob.set_air_supply(pre_tick_air_supply - 1);
        if mob.should_take_drowning_damage() {
            mob.set_air_supply(0);
            if let Some(world) = mob.level() {
                mob.hurt(
                    &world,
                    &DamageSource::environment(&vanilla_damage_types::DROWN),
                    2.0,
                );
            }
        }
    } else {
        mob.set_air_supply(MAX_AIR_SUPPLY);
    }
}

/// Vanilla `WaterAnimal.checkSpawnObstruction`: only other entities obstruct.
pub fn water_animal_spawn_unobstructed<M: Mob + ?Sized>(
    mob: &M,
    level: &dyn ServerLevelAccessor,
) -> bool {
    level.is_unobstructed_by_entities(mob.as_entity_event_source())
}

/// Vanilla `WaterAnimal.getBaseExperienceReward`.
#[must_use]
pub fn water_animal_experience_reward() -> i32 {
    1 + rand::random_range(0..3)
}

/// Vanilla `AbstractFish` behavior shared by cod, salmon, pufferfish and
/// tropical fish.
///
/// TODO(bucketable): bucket pickup (`Bucketable.bucketMobPickup`) and mob
/// bucket items come with the shared `Bucketable` layer.
pub trait AbstractFish: PathfinderMob {
    /// Whether this fish was released from a bucket.
    fn is_from_bucket(&self) -> bool;

    /// Sets the synced `FROM_BUCKET` flag.
    fn set_from_bucket(&self, from_bucket: bool);

    /// The sound played when the fish flops on land.
    fn flop_sound(&self) -> SoundEventRef;

    /// Vanilla `AbstractFish.canRandomSwim`.
    fn can_random_swim(&self) -> bool {
        self.as_schooling_fish()
            .is_none_or(|fish| !fish.is_follower())
    }

    /// Vanilla `AbstractFish.requiresCustomPersistence`.
    fn fish_requires_custom_persistence(&self) -> bool {
        self.is_passenger() || self.is_leashed() || self.is_from_bucket()
    }

    /// Vanilla `AbstractFish.removeWhenFarAway`.
    fn fish_remove_when_far_away(&self) -> bool {
        !self.is_from_bucket() && self.custom_name().is_none()
    }

    /// Vanilla `AbstractFish.aiStep`: flops on land, then runs the mob step.
    fn fish_ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        if !self.is_in_water() && self.on_ground() && self.vertical_collision() {
            let velocity = self.velocity();
            self.set_velocity(DVec3::new(
                velocity.x + f64::from((rand::random::<f32>() * 2.0 - 1.0) * 0.05),
                velocity.y + f64::from(0.4_f32),
                velocity.z + f64::from((rand::random::<f32>() * 2.0 - 1.0) * 0.05),
            ));
            self.set_on_ground(false);
            self.mark_velocity_sync();
            self.make_sound(Some(self.flop_sound()));
        }

        self.mob_ai_step(entity)
    }

    /// Vanilla `AbstractFish.travelInWater`.
    fn fish_travel_in_water(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        self.move_relative(0.01, input);
        let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
        self.set_velocity(self.velocity() * 0.9);
        if self.target().is_none() {
            self.set_velocity(self.velocity() + DVec3::new(0.0, -0.005, 0.0));
        }
        result
    }

    /// Vanilla `AbstractFish.FishMoveControl.tick`. Unlike the base move
    /// control it keeps `MOVE_TO` until navigation finishes.
    fn tick_fish_move_control(&self) {
        if self.is_eye_in_water() {
            self.set_velocity(self.velocity() + DVec3::new(0.0, 0.005, 0.0));
        }

        let move_control = self.mob_base().controls().lock().move_control;
        let navigating = !self.mob_base().navigation().lock().is_done();
        if !matches!(move_control.operation(), MoveControlOperation::MoveTo) || !navigating {
            self.set_mob_speed(0.0);
            return;
        }

        let movement_speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED);
        let target_speed = (move_control.speed_modifier() * movement_speed) as f32;
        let speed = self.get_speed();
        self.set_mob_speed(speed + 0.125 * (target_speed - speed));

        let wanted = move_control.wanted_position();
        let position = self.position();
        let xd = wanted.x - position.x;
        let yd = wanted.y - position.y;
        let zd = wanted.z - position.z;
        if yd != 0.0 {
            let dd = (xd * xd + yd * yd + zd * zd).sqrt();
            let speed = f64::from(self.get_speed());
            self.set_velocity(self.velocity() + DVec3::new(0.0, speed * (yd / dd) * 0.1, 0.0));
        }

        if xd != 0.0 || zd != 0.0 {
            let y_rot_d = (zd.atan2(xd) as f32) * RAD_TO_DEG - 90.0;
            let (y_rot, x_rot) = self.rotation();
            let y_rot = rotlerp(y_rot, y_rot_d, FISH_MOVE_CONTROL_MAX_TURN);
            self.set_rotation((y_rot, x_rot));
            self.set_y_body_rot(y_rot);
        }
    }

    /// Vanilla `AbstractFish.addAdditionalSaveData`.
    fn save_fish(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        nbt.insert("FromBucket", i8::from(self.is_from_bucket()));
    }

    /// Vanilla `AbstractFish.readAdditionalSaveData`.
    fn load_fish(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        self.set_from_bucket(nbt.byte("FromBucket").is_some_and(|value| value != 0));
    }
}

/// Runtime school membership of a schooling fish.
#[derive(Debug)]
pub struct SchoolState {
    leader: Option<Weak<dyn Entity>>,
    school_size: i32,
}

impl SchoolState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            leader: None,
            school_size: 1,
        }
    }
}

impl Default for SchoolState {
    fn default() -> Self {
        Self::new()
    }
}

/// `SpawnGroupData` for a school spawned together: everyone follows the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchoolSpawnGroupData {
    /// Entity id of the school leader.
    pub leader: i32,
    /// Vanilla `TropicalFish.TropicalFishGroupData.variant`: the packed
    /// variant shared by a tropical fish school, `None` for other fish.
    pub tropical_fish_variant: Option<i32>,
}

/// Vanilla `AbstractSchoolingFish`.
pub trait AbstractSchoolingFish: AbstractFish {
    /// The fish's school membership.
    fn school(&self) -> &SyncMutex<SchoolState>;

    /// Vanilla `AbstractSchoolingFish.getMaxSchoolSize`.
    fn max_school_size(&self) -> i32 {
        FISH_MAX_SPAWN_CLUSTER_SIZE
    }

    /// The leader, if this fish follows one that is still loaded.
    fn leader(&self) -> Option<SharedEntity> {
        self.school().lock().leader.as_ref().and_then(Weak::upgrade)
    }

    fn is_follower(&self) -> bool {
        self.leader().is_some_and(|leader| {
            leader
                .as_living_entity()
                .is_some_and(LivingEntity::is_alive)
        })
    }

    fn start_following(&self, leader: &SharedEntity) {
        self.school().lock().leader = Some(Arc::downgrade(leader));
        if let Some(leader) = leader.as_schooling_fish() {
            leader.school().lock().school_size += 1;
        }
    }

    fn stop_following(&self) {
        let leader = self.school().lock().leader.take();
        if let Some(leader) = leader.and_then(|leader| leader.upgrade())
            && let Some(leader) = leader.as_schooling_fish()
        {
            leader.school().lock().school_size -= 1;
        }
    }

    fn school_size(&self) -> i32 {
        self.school().lock().school_size
    }

    fn has_followers(&self) -> bool {
        self.school_size() > 1
    }

    fn can_be_followed(&self) -> bool {
        self.has_followers() && self.school_size() < self.max_school_size()
    }

    fn in_range_of_leader(&self) -> bool {
        self.leader().is_some_and(|leader| {
            self.position().distance_squared(leader.position()) <= SCHOOL_LEADER_RANGE_SQR
        })
    }

    fn path_to_leader(&self) {
        if !self.is_follower() {
            return;
        }
        if let Some(leader) = self.leader() {
            self.move_to_pos_with_reach(leader.position(), 1, 1.0);
        }
    }

    /// Vanilla `addFollowers`: the stream is limited before `this` is filtered out.
    fn add_followers(&self, this: &SharedEntity, fish: Vec<SharedEntity>) {
        let limit = usize::try_from(self.max_school_size() - self.school_size()).unwrap_or(0);
        for other in fish
            .into_iter()
            .take(limit)
            .filter(|other| other.id() != self.id())
        {
            if let Some(other) = other.as_schooling_fish() {
                other.start_following(this);
            }
        }
    }

    /// Same-type schooling fish within the vanilla 8-block search box.
    fn nearby_school_fish(&self, world: &World) -> Vec<SharedEntity> {
        let entity_type = self.entity_type();
        let search_box = self.bounding_box().inflate_xyz(
            SCHOOL_SEARCH_RADIUS,
            SCHOOL_SEARCH_RADIUS,
            SCHOOL_SEARCH_RADIUS,
        );
        world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.entity_type() == entity_type && entity.as_schooling_fish().is_some()
        })
    }

    /// Vanilla `AbstractSchoolingFish.tick` addition: a lone leader resets
    /// its school size.
    fn tick_school(&self) {
        if !self.has_followers() || rand::random_range(0..200) != 1 {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        if self.nearby_school_fish(&world).len() <= 1 {
            self.school().lock().school_size = 1;
        }
    }

    /// Vanilla `AbstractSchoolingFish.finalizeSpawn`.
    fn finalize_spawn_schooling_fish(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        self.finalize_spawn_mob_base(world, spawn_reason, group_data);
        match group_data {
            None => Some(SpawnGroupData::School(SchoolSpawnGroupData {
                leader: self.id(),
                tropical_fish_variant: None,
            })),
            Some(SpawnGroupData::School(school)) => {
                if let Some(leader) = world.get_entity_by_id(school.leader) {
                    self.start_following(&leader);
                }
                group_data
            }
            Some(other) => Some(other),
        }
    }
}
