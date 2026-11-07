//! Vanilla `Drowned`: an underwater zombie that swims after targets in the
//! water, walks to the beach at night and throws tridents.

use std::f64::consts::FRAC_PI_2;
use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::{Rng, RngExt as _};
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::Direction;
use steel_registry::blocks::shapes::VoxelShape;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_biome_tags::BiomeTag;
use steel_registry::vanilla_entity_data::DrownedEntityData;
use steel_registry::{
    sound_events, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::random::Random;
use steel_utils::random::legacy_random::LegacyRandom;
use steel_utils::types::{Difficulty, InteractionHand};
use steel_utils::{BlockPos, Downcast as _, DowncastType, DowncastTypeKey};

use super::{ZombieState, ZombieVariant, zombie_mob_base};
use crate::behavior::items::projectile_weapon::spawn_arrow_using_shoot;
use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::entity::MoverType;
use crate::entity::ai::control::MoveControlOperation;
use crate::entity::ai::goal::move_to_block::MoveToBlockGoal;
use crate::entity::ai::goal::random_pos::default_random_pos_towards;
use crate::entity::ai::goal::{
    Goal, GoalControls, HurtByTargetGoal, NearestAttackableTargetGoal, RandomStrollGoal,
    RangedAttackGoal, ZombieAttackGoal,
};
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::ai::path::Path;
use crate::entity::ai::path::PathType;
use crate::entity::entities::ThrownTridentEntity;
use crate::entity::mob::rotlerp;
use crate::entity::ranged_attack_mob::default_ranged_attack_uncertainty;
use crate::entity::spawn::create_entity_instance;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntitySpawnReason, LivingEntity, LivingEntityBase, Mob,
    MobBase, Monster, PathfinderMob, RangedAttackMob, SharedEntity, SpawnGroupData, SpawnRule,
};
use crate::fluid::FluidStateExt as _;
use crate::inventory::equipment::EquipmentSlot;
use crate::physics::MoveResult;
use crate::physics::merged_face_occludes;
use crate::world::{LevelReader as _, ServerLevelAccessor, World};

/// Vanilla `Drowned.NAUTILUS_SHELL_CHANCE`.
const NAUTILUS_SHELL_CHANCE: f32 = 0.03;
/// Vanilla `Drowned.ZOMBIE_NAUTILUS_JOCKEY_CHANCE`.
const ZOMBIE_NAUTILUS_JOCKEY_CHANCE: f32 = 0.5;
/// Vanilla `Drowned.populateDefaultEquipmentSlots`: chance of any held item.
const HELD_ITEM_CHANCE: f32 = 0.9;
const TRIDENT_ATTACK_INTERVAL: i32 = 40;
const TRIDENT_ATTACK_RADIUS: f32 = 10.0;
const TRIDENT_SHOOT_POWER: f32 = 1.6;
const MORE_FREQUENT_SPAWN_BOUND: u32 = 15;
const DEFAULT_SPAWN_BOUND: u32 = 40;

/// A hostile drowned.
#[entity_behavior(class = "Drowned", spawn_rule)]
pub struct DrownedEntity {
    pub(super) base: EntityBase,
    pub(super) entity_type: EntityTypeRef,
    pub(super) living_base: LivingEntityBase,
    pub(super) mob_base: MobBase,
    pub(super) entity_data: SyncMutex<DrownedEntityData>,
    pub(super) zombie_state: SyncMutex<ZombieState>,
}

// SAFETY: The owner-scoped type key uniquely identifies DrownedEntity.
unsafe impl DowncastType for DrownedEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/drowned");
}

impl DrownedEntity {
    /// Vanilla `SpawnPlacements`: `Drowned.checkDrownedSpawnRules`.
    pub(crate) const SPAWN_RULE: SpawnRule = |_entity_type, level, spawn_reason, pos, random| {
        check_drowned_spawn_rules(level, spawn_reason, pos, random)
    };

    /// Creates a new drowned.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved drowned.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = DrownedEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            mob_base: zombie_mob_base(ZombieVariant::Drowned),
            entity_data: SyncMutex::new(entity_data),
            zombie_state: SyncMutex::new(ZombieState::new()),
        }
    }

    /// Vanilla `Drowned.finalizeSpawn`: the zombie spawn, a rare nautilus
    /// shell in the off hand, and a zombie nautilus mount for half of the
    /// adult trident holders spawned naturally or in structures outside the
    /// rivers.
    pub(super) fn finalize_spawn_drowned(
        &self,
        world: &Arc<World>,
        spawn_reason: EntitySpawnReason,
        group_data: Option<SpawnGroupData>,
    ) -> Option<SpawnGroupData> {
        let mut random = LegacyRandom::from_seed(rand::random());
        let group_data = self.finalize_zombie_spawn(world, &mut random, spawn_reason, group_data);
        if !self.has_item_in_slot(EquipmentSlot::OffHand)
            && random.next_f32() < NAUTILUS_SHELL_CHANCE
        {
            let _ = self.living_base.equipment().lock().set(
                EquipmentSlot::OffHand,
                ItemStack::new(&vanilla_items::NAUTILUS_SHELL),
            );
            self.set_guaranteed_drop(EquipmentSlot::OffHand);
        }
        if matches!(
            spawn_reason,
            EntitySpawnReason::Natural | EntitySpawnReason::Structure
        ) && self
            .equipment_in_slot(EquipmentSlot::MainHand)
            .is(&vanilla_items::TRIDENT)
            && random.next_f32() < ZOMBIE_NAUTILUS_JOCKEY_CHANCE
            && !self.is_baby()
            && !world
                .biome(self.block_position())
                .is_some_and(|biome| biome.has_tag(&BiomeTag::MORE_FREQUENT_DROWNED_SPAWNS))
        {
            self.spawn_zombie_nautilus_mount(world, spawn_reason);
        }
        group_data
    }

    /// The zombie nautilus half of vanilla `Drowned.finalizeSpawn`. Steel
    /// links riders only once both are in the world, so the drowned mounts on
    /// its first tick.
    fn spawn_zombie_nautilus_mount(&self, world: &Arc<World>, spawn_reason: EntitySpawnReason) {
        let Ok(nautilus) =
            create_entity_instance(world, &vanilla_entities::ZOMBIE_NAUTILUS, self.position())
        else {
            return;
        };
        let (yaw, _) = self.rotation();
        nautilus.set_rotation((yaw, 0.0));
        if let Some(nautilus_mob) = nautilus.as_mob() {
            if spawn_reason == EntitySpawnReason::Structure {
                nautilus_mob.set_persistence_required();
            }
            let _ = nautilus_mob.finalize_spawn(world, spawn_reason, None);
        }
        if world.try_add_entity(Arc::clone(&nautilus)).is_ok() {
            self.zombie_state.lock().pending_jockey_mount = Some(nautilus);
        }
    }

    /// Vanilla `Drowned.okTarget`: by day only targets in the water.
    #[must_use]
    pub fn ok_target(&self, target: Option<&SharedEntity>) -> bool {
        let Some(target) = target else {
            return false;
        };
        let bright = self.level().is_some_and(|world| world.is_bright_outside());
        !bright || target.is_in_water()
    }

    /// Vanilla `Drowned.wantsToSwim`.
    #[must_use]
    pub fn wants_to_swim(&self) -> bool {
        if self.is_searching_for_land() {
            return true;
        }
        self.target().is_some_and(|target| target.is_in_water())
    }

    /// Vanilla `Drowned.isSearchingForLand`.
    #[must_use]
    pub fn is_searching_for_land(&self) -> bool {
        self.zombie_state.lock().searching_for_land
    }

    /// Vanilla `Drowned.setSearchingForLand`.
    pub fn set_searching_for_land(&self, searching_for_land: bool) {
        self.zombie_state.lock().searching_for_land = searching_for_land;
    }

    /// Vanilla `Drowned.setRangedAttackUncertainty`; negative restores the default.
    pub fn set_ranged_attack_uncertainty(&self, uncertainty: f32) {
        self.zombie_state.lock().ranged_attack_uncertainty = uncertainty;
    }

    /// Vanilla `Drowned.closeToNextPos`: within two blocks of the path's end.
    fn close_to_next_pos(&self) -> bool {
        let target = self.mob_base.navigation().lock().path().map(Path::target);
        target.is_some_and(|pos| {
            self.distance_to_sqr(DVec3::new(
                f64::from(pos.x()),
                f64::from(pos.y()),
                f64::from(pos.z()),
            )) < 4.0
        })
    }

    /// Vanilla `Drowned.travelInWater`.
    pub(super) fn travel_in_water_drowned(
        &self,
        entity: &SharedEntity,
        input: DVec3,
        base_gravity: f64,
        is_falling: bool,
        old_y: f64,
    ) -> Option<MoveResult> {
        if self.is_under_water() && self.wants_to_swim() {
            self.move_relative(0.01, input);
            let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
            self.set_velocity(self.velocity() * 0.9);
            result
        } else {
            self.default_travel_in_water(entity, input, base_gravity, is_falling, old_y)
        }
    }

    /// Vanilla `Drowned.DrownedMoveControl.tick`.
    pub(super) fn tick_drowned_move_control(&self) {
        if !(self.wants_to_swim() && self.is_in_water()) {
            if !self.on_ground() {
                self.set_velocity(self.velocity() + DVec3::new(0.0, -0.008, 0.0));
            }
            self.default_tick_move_control();
            return;
        }

        let target_above = self
            .target()
            .is_some_and(|target| target.position().y > self.position().y);
        if target_above || self.is_searching_for_land() {
            self.set_velocity(self.velocity() + DVec3::new(0.0, 0.002, 0.0));
        }

        let move_control = self.mob_base.controls().lock().move_control;
        if move_control.operation() != MoveControlOperation::MoveTo
            || self.mob_base.navigation().lock().is_done()
        {
            self.set_mob_speed(0.0);
            return;
        }

        let wanted = move_control.wanted_position();
        let position = self.position();
        let xd = wanted.x - position.x;
        let mut yd = wanted.y - position.y;
        let zd = wanted.z - position.z;
        let dd = (xd * xd + yd * yd + zd * zd).sqrt();
        yd /= dd;
        let y_rot_d = (zd.atan2(xd) as f32).to_degrees() - 90.0;
        let (y_rot, x_rot) = self.rotation();
        let y_rot = rotlerp(y_rot, y_rot_d, 90.0);
        self.set_rotation((y_rot, x_rot));
        self.set_y_body_rot(y_rot);
        let movement_speed = self
            .attributes()
            .lock()
            .get_value(vanilla_attributes::MOVEMENT_SPEED)
            .unwrap_or(0.0);
        let target_speed = (move_control.speed_modifier() * movement_speed) as f32;
        let speed = self.get_speed();
        let new_speed = speed + 0.125 * (target_speed - speed);
        self.set_mob_speed(new_speed);
        let new_speed = f64::from(new_speed);
        self.set_velocity(
            self.velocity()
                + DVec3::new(
                    new_speed * xd * 0.005,
                    new_speed * yd * 0.1,
                    new_speed * zd * 0.005,
                ),
        );
    }
}

impl RangedAttackMob for DrownedEntity {
    /// Vanilla `Drowned.performRangedAttack`.
    fn perform_ranged_attack(&self, entity: &SharedEntity, target: &SharedEntity, _power: f32) {
        let Some(world) = self.level() else {
            return;
        };
        let main_hand = self.equipment_in_slot(EquipmentSlot::MainHand);
        let trident_stack = if main_hand.is(&vanilla_items::TRIDENT) {
            main_hand
        } else {
            ItemStack::new(&vanilla_items::TRIDENT)
        };
        let trident = ThrownTridentEntity::thrown_by(&world, entity, &trident_stack);
        let target_position = target.position();
        let position = self.position();
        let xd = target_position.x - position.x;
        let yd = target_position.y + target.bounding_box().height() / 3.0 - trident.position().y;
        let zd = target_position.z - position.z;
        let distance = (xd * xd + zd * zd).sqrt();
        let uncertainty = self.ranged_attack_uncertainty(&world);
        let trident: SharedEntity = Arc::new(trident);
        spawn_arrow_using_shoot(
            &world,
            &trident,
            &trident_stack,
            entity,
            DVec3::new(xd, yd + distance * f64::from(0.2_f32), zd),
            TRIDENT_SHOOT_POWER,
            uncertainty,
        );
        let pitch = 1.0 / (rand::random::<f32>() * 0.4 + 0.8);
        self.play_sound(&sound_events::ENTITY_DROWNED_SHOOT, 1.0, pitch);
    }

    /// Vanilla `Drowned.rangedAttackUncertainty`.
    fn ranged_attack_uncertainty(&self, world: &World) -> f32 {
        let uncertainty = self.zombie_state.lock().ranged_attack_uncertainty;
        if uncertainty < 0.0 {
            default_ranged_attack_uncertainty(world.difficulty())
        } else {
            uncertainty
        }
    }
}

/// Vanilla `Drowned.checkDrownedSpawnRules`.
fn check_drowned_spawn_rules(
    level: &dyn ServerLevelAccessor,
    spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    random: &mut dyn Rng,
) -> bool {
    let is_water = |pos: BlockPos| level.get_block_state(pos).get_fluid_state().is_water();
    if !is_water(pos.below()) && !spawn_reason.is_spawner() {
        return false;
    }
    let can_monster_spawn = level.difficulty() != Difficulty::Peaceful
        && (spawn_reason.ignores_light_requirements()
            || <DrownedEntity as Monster>::is_dark_enough_to_spawn(level, pos, random))
        && (spawn_reason.is_spawner() || is_water(pos));
    if can_monster_spawn
        && (spawn_reason.is_spawner() || spawn_reason == EntitySpawnReason::Reinforcement)
    {
        return true;
    }
    let more_frequent = level
        .biome(pos)
        .is_some_and(|biome| biome.has_tag(&BiomeTag::MORE_FREQUENT_DROWNED_SPAWNS));
    if more_frequent {
        random.random_range(0..MORE_FREQUENT_SPAWN_BOUND) == 0 && can_monster_spawn
    } else {
        random.random_range(0..DEFAULT_SPAWN_BOUND) == 0
            && pos.y() < level.sea_level() - 5
            && can_monster_spawn
    }
}

/// Vanilla `Drowned.populateDefaultEquipmentSlots`: replaces the zombie's
/// armor and iron weapon with a rare trident or fishing rod.
pub(super) fn populate_drowned_equipment(living_base: &LivingEntityBase, random: &mut impl Random) {
    if f64::from(random.next_f32()) > f64::from(HELD_ITEM_CHANCE) {
        let item = if random.next_i32_bounded(16) < 10 {
            &vanilla_items::TRIDENT
        } else {
            &vanilla_items::FISHING_ROD
        };
        let _ = living_base
            .equipment()
            .lock()
            .set(EquipmentSlot::MainHand, ItemStack::new(item));
    }
}

/// Vanilla `Drowned` constructor, `createNavigation` and `addBehaviourGoals`.
pub(super) fn add_behaviour_goals(mob_base: &MobBase) {
    *mob_base.navigation().lock() = PathNavigation::amphibious();
    mob_base
        .pathfinding_malus()
        .lock()
        .set(PathType::Water, 0.0);

    let mut goal_selector = mob_base.goal_selector().lock();
    goal_selector.add_goal(1, DrownedGoToWaterGoal::new(1.0));
    goal_selector.add_goal(
        2,
        DrownedTridentAttackGoal(RangedAttackGoal::new(
            1.0,
            TRIDENT_ATTACK_INTERVAL,
            TRIDENT_ATTACK_RADIUS,
        )),
    );
    goal_selector.add_goal(2, DrownedAttackGoal(ZombieAttackGoal::new(1.0, false)));
    goal_selector.add_goal(5, DrownedGoToBeachGoal::new(1.0));
    goal_selector.add_goal(6, DrownedSwimUpGoal::new(1.0));
    goal_selector.add_goal(7, RandomStrollGoal::new(1.0));
    drop(goal_selector);

    let mut target_selector = mob_base.target_selector().lock();
    target_selector.add_goal(
        1,
        HurtByTargetGoal::new()
            .set_alert_others_matching(ZombieVariant::Drowned.alert_matcher(), []),
    );
    target_selector.add_goal(
        2,
        NearestAttackableTargetGoal::new_for_players_with_interval(
            10,
            true,
            false,
            |target, world| !world.is_bright_outside() || target.is_in_water(),
        ),
    );
    target_selector.add_goal(
        3,
        NearestAttackableTargetGoal::new(false, |target, _| {
            target.entity_type() == &vanilla_entities::VILLAGER
                || target.entity_type() == &vanilla_entities::WANDERING_TRADER
        }),
    );
    target_selector.add_goal(
        3,
        NearestAttackableTargetGoal::new(true, |target, _| {
            target.entity_type() == &vanilla_entities::IRON_GOLEM
        }),
    );
    target_selector.add_goal(
        3,
        NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
            target.entity_type() == &vanilla_entities::AXOLOTL
        }),
    );
    target_selector.add_goal(
        5,
        NearestAttackableTargetGoal::new_with_interval(10, true, false, |target, _| {
            target.entity_type() == &vanilla_entities::TURTLE
                && target.is_baby()
                && !target.is_in_water()
        }),
    );
}

fn as_drowned(mob: &dyn PathfinderMob) -> Option<&DrownedEntity> {
    let entity: &dyn Entity = mob;
    entity.downcast_ref::<DrownedEntity>()
}

/// Vanilla `Drowned.DrownedAttackGoal`.
struct DrownedAttackGoal(ZombieAttackGoal);

impl Goal for DrownedAttackGoal {
    fn controls(&self) -> GoalControls {
        self.0.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_use(mob)
            && as_drowned(mob).is_some_and(|drowned| drowned.ok_target(mob.target().as_ref()))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_continue_to_use(mob)
            && as_drowned(mob).is_some_and(|drowned| drowned.ok_target(mob.target().as_ref()))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.0.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.0.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.0.tick(mob, entity);
    }
}

/// Vanilla `Drowned.DrownedTridentAttackGoal`.
struct DrownedTridentAttackGoal(RangedAttackGoal);

impl Goal for DrownedTridentAttackGoal {
    fn controls(&self) -> GoalControls {
        self.0.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_use(mob)
            && mob
                .equipment_in_slot(EquipmentSlot::MainHand)
                .is(&vanilla_items::TRIDENT)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.0.start(mob);
        mob.set_aggressive(true);
        mob.start_using_held_item(InteractionHand::MainHand);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.0.stop(mob);
        mob.stop_using_held_item();
        mob.set_aggressive(false);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.0.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.0.tick(mob, entity);
    }
}

/// Vanilla `Drowned.DrownedGoToBeachGoal`.
struct DrownedGoToBeachGoal(MoveToBlockGoal);

impl DrownedGoToBeachGoal {
    fn new(speed_modifier: f64) -> Self {
        Self(MoveToBlockGoal::with_vertical_search_range(
            speed_modifier,
            8,
            2,
            is_beach_target,
        ))
    }
}

/// Vanilla `DrownedGoToBeachGoal.isValidTarget`: two air blocks over a block
/// whose collision top is a full face.
fn is_beach_target(world: &World, pos: BlockPos) -> bool {
    let above = pos.above();
    if !world.get_block_state(above).is_air() || !world.get_block_state(above.above()).is_air() {
        return false;
    }
    let state = world.get_block_state(pos);
    let shape = BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, pos, BlockCollisionContext::empty());
    is_face_full(shape, Direction::Up)
}

/// Vanilla `Block.isFaceFull`.
fn is_face_full(shape: VoxelShape, direction: Direction) -> bool {
    merged_face_occludes(shape, VoxelShape::EMPTY, direction)
}

impl Goal for DrownedGoToBeachGoal {
    fn controls(&self) -> GoalControls {
        self.0.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if !self.0.can_use(mob) {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        !world.is_bright_outside()
            && mob.is_in_water()
            && mob.position().y >= f64::from(world.sea_level() - 3)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.0.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(drowned) = as_drowned(mob) {
            drowned.set_searching_for_land(false);
        }
        self.0.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.0.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.0.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.0.tick(mob, entity);
    }
}

/// Vanilla `Drowned.DrownedGoToWaterGoal`: by day, walk back into the water.
struct DrownedGoToWaterGoal {
    wanted: DVec3,
    speed_modifier: f64,
}

impl DrownedGoToWaterGoal {
    const fn new(speed_modifier: f64) -> Self {
        Self {
            wanted: DVec3::ZERO,
            speed_modifier,
        }
    }

    /// Vanilla `getWaterPos`: ten random tries near the mob.
    fn water_pos(mob: &dyn PathfinderMob, world: &World) -> Option<DVec3> {
        let mut random = LegacyRandom::from_seed(rand::random());
        let pos = mob.block_position();
        for _ in 0..10 {
            let random_pos = pos.offset(
                random.next_i32_bounded(20) - 10,
                2 - random.next_i32_bounded(8),
                random.next_i32_bounded(20) - 10,
            );
            if world.get_block_state(random_pos).get_block() == &vanilla_blocks::WATER {
                return Some(DVec3::new(
                    f64::from(random_pos.x()) + 0.5,
                    f64::from(random_pos.y()),
                    f64::from(random_pos.z()) + 0.5,
                ));
            }
        }
        None
    }
}

impl Goal for DrownedGoToWaterGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        if !world.is_bright_outside() || mob.is_in_water() {
            return false;
        }
        let Some(pos) = Self::water_pos(mob, &world) else {
            return false;
        };
        self.wanted = pos;
        true
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.mob_base().navigation().lock().is_done()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.move_to_pos(self.wanted, self.speed_modifier);
    }
}

/// Vanilla `Drowned.DrownedSwimUpGoal`: at night, swim up towards the surface.
struct DrownedSwimUpGoal {
    speed_modifier: f64,
    stuck: bool,
}

impl DrownedSwimUpGoal {
    const fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            stuck: false,
        }
    }

    fn can_use_now(mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        !world.is_bright_outside()
            && mob.is_in_water()
            && mob.position().y < f64::from(world.sea_level() - 2)
    }
}

impl Goal for DrownedSwimUpGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::default()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        Self::can_use_now(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        Self::can_use_now(mob) && !self.stuck
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(drowned) = as_drowned(mob) {
            drowned.set_searching_for_land(true);
        }
        self.stuck = false;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(drowned) = as_drowned(mob) {
            drowned.set_searching_for_land(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(world) = mob.level() else {
            return;
        };
        let sea_level = world.sea_level();
        let position = mob.position();
        if position.y >= f64::from(sea_level - 1) {
            return;
        }
        let close = as_drowned(mob).is_some_and(DrownedEntity::close_to_next_pos);
        if !(mob.mob_base().navigation().lock().is_done() || close) {
            return;
        }
        let towards = DVec3::new(position.x, f64::from(sea_level - 1), position.z);
        let Some(next) = default_random_pos_towards(mob, 4, 8, towards, FRAC_PI_2) else {
            self.stuck = true;
            return;
        };
        let _ = mob.move_to_pos(next, self.speed_modifier);
    }
}
