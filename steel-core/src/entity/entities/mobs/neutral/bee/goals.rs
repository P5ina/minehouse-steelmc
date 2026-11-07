//! The bee's own goals, vanilla `Bee`'s inner goal classes.

use std::f32::consts::FRAC_PI_2 as FRAC_PI_2_F32;
use std::f64::consts::FRAC_PI_2;

use glam::DVec3;
use rustc_hash::FxHashMap;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, IntProperty};
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_poi_type_tags::PoiTag;
use steel_registry::{
    REGISTRY, RegistryExt as _, TaggedRegistryExt as _, level_events, sound_events, vanilla_blocks,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use super::{
    BeeEntity, COOLDOWN_BEFORE_LOCATING_NEW_FLOWER, COOLDOWN_BEFORE_LOCATING_NEW_HIVE,
    DEFAULT_WANDER_DISTANCE_REDUCTION, HIVE_CLOSE_ENOUGH_DISTANCE, HIVE_SEARCH_DISTANCE,
    MAX_CROPS_GROWABLE, PATHFIND_TO_HIVE_WHEN_CLOSER_THAN, RESTRICTED_WANDER_DISTANCE_REDUCTION,
    TOO_FAR_DISTANCE, attracts_bees, is_loaded, random_flower_retry_cooldown,
};
use crate::behavior::BLOCK_BEHAVIORS;
use crate::block_entity::entities::BeehiveBlockEntity;
use crate::entity::ai::goal::random_pos::{air_and_water_random_pos, hover_random_pos};
use crate::entity::ai::goal::{
    Goal, GoalControls, HurtByTargetGoal, MeleeAttackGoal, NearestAttackableTargetGoal,
    reduced_tick_delay,
};
use crate::entity::ai::path::Path;
use crate::entity::neutral_mob::NeutralMob as _;
use crate::entity::{Entity, Mob as _, PathfinderMob, SharedEntity};
use crate::poi::OccupationStatus;

/// Vanilla `LookControl.setLookAt(x, y, z)` turns the head at `getHeadRotSpeed`.
const HEAD_ROT_SPEED: f32 = 10.0;
/// Vanilla `BeeAttackGoal` speed modifier, `1.4F` widened to a double.
const ATTACK_SPEED_MODIFIER: f32 = 1.4;
const TRAVEL_TICKS_LIMIT: i32 = 2400;
const MAX_BLACKLISTED_TARGETS: usize = 3;
const TICKS_BEFORE_HIVE_DROP: i32 = 60;
const MIN_POLLINATION_TICKS: i32 = 400;
const MAX_POLLINATING_TICKS: i32 = 600;
const ARRIVAL_THRESHOLD: f64 = 0.1;
const POSITION_CHANGE_CHANCE: i32 = 25;
const POLLINATE_SPEED_MODIFIER: f32 = 0.35;
const HOVER_HEIGHT_WITHIN_FLOWER: f32 = 0.6;
const HOVER_POS_OFFSET: f32 = 1.0 / 3.0;
const FLOWER_SEARCH_RADIUS: i32 = 5;
/// Unreachable flowers are skipped for this many ticks.
const UNREACHABLE_FLOWER_TICKS: i64 = 600;
const GROW_CHANCE: i32 = 30;
const MIN_VALIDATE_COOLDOWN: i32 = 20;
const MAX_VALIDATE_COOLDOWN: i32 = 40;
const AGE_7: &IntProperty = &BlockStateProperties::AGE_7;
const AGE_3: &IntProperty = &BlockStateProperties::AGE_3;
const AGE_1: &IntProperty = &BlockStateProperties::AGE_1;

fn bee(mob: &dyn PathfinderMob) -> Option<&BeeEntity> {
    mob.downcast_ref::<BeeEntity>()
}

/// Vanilla `Goal.adjustedTickDelay` for goals that do not update every tick.
const fn adjusted_tick_delay(ticks: i32) -> i32 {
    reduced_tick_delay(ticks)
}

/// Vanilla `BlockPos.closerToCenterThan`.
fn closer_to_center_than(pos: BlockPos, position: DVec3, distance: f64) -> bool {
    let center = DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()) + 0.5,
        f64::from(pos.z()) + 0.5,
    );
    center.distance_squared(position) < distance * distance
}

fn random_validate_cooldown() -> i32 {
    rand::random_range(MIN_VALIDATE_COOLDOWN..=MAX_VALIDATE_COOLDOWN)
}

/// Vanilla `Bee.BaseBeeGoal`: an angry bee drops everything else.
fn base_can_use(bee: &BeeEntity, can_bee_use: bool) -> bool {
    can_bee_use && !bee.is_angry()
}

/// Vanilla `BeeAttackGoal`: only an angry bee that still has its stinger attacks.
pub(super) struct BeeAttackGoal {
    attack: MeleeAttackGoal,
}

impl BeeAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            attack: MeleeAttackGoal::new(ATTACK_SPEED_MODIFIER as f64, true),
        }
    }
}

fn bee_can_attack(mob: &dyn PathfinderMob) -> bool {
    bee(mob).is_some_and(|bee| bee.is_angry() && !bee.has_stung())
}

impl Goal for BeeAttackGoal {
    fn controls(&self) -> GoalControls {
        self.attack.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_use(mob) && bee_can_attack(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_continue_to_use(mob) && bee_can_attack(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.attack.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.attack.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.attack.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.attack.tick(mob, entity);
    }
}

/// Vanilla `BeeBecomeAngryTargetGoal`: an angry bee targets the player it is
/// angry at.
pub(super) struct BeeBecomeAngryTargetGoal {
    target: NearestAttackableTargetGoal,
}

impl BeeBecomeAngryTargetGoal {
    pub(super) fn new() -> Self {
        Self {
            target: NearestAttackableTargetGoal::new_for_players_with_interval(
                10,
                true,
                false,
                |_, _| true,
            )
            .with_mob_selector(|mob, target, world| {
                mob.as_neutral_mob()
                    .is_some_and(|bee| bee.is_angry_at(target, world))
            }),
        }
    }
}

impl Goal for BeeBecomeAngryTargetGoal {
    fn controls(&self) -> GoalControls {
        self.target.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee_can_attack(mob) && self.target.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if bee_can_attack(mob) && mob.target().is_some() {
            self.target.can_continue_to_use(mob)
        } else {
            false
        }
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.target.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.target.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.target.tick(mob, entity);
    }
}

/// Vanilla `BeeHurtByOtherGoal`: a hurt bee calls the bees around that can see
/// the attacker, and gives up once it calms down.
pub(super) struct BeeHurtByOtherGoal {
    hurt_by: HurtByTargetGoal,
}

pub(super) fn bee_hurt_by_other_goal() -> BeeHurtByOtherGoal {
    BeeHurtByOtherGoal {
        hurt_by: HurtByTargetGoal::new()
            .set_alert_others([])
            .with_alert_condition(|mob, _other, hurt_by_mob| {
                mob.has_line_of_sight(hurt_by_mob.as_ref())
            }),
    }
}

impl Goal for BeeHurtByOtherGoal {
    fn controls(&self) -> GoalControls {
        self.hurt_by.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.hurt_by.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(BeeEntity::is_angry) && self.hurt_by.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.stop(mob);
    }
}

/// Vanilla `BeeEnterHiveGoal`: a bee next to its hive goes in.
pub(super) struct BeeEnterHiveGoal;

impl Goal for BeeEnterHiveGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(bee) = bee(mob) else {
            return false;
        };
        let can_bee_use = (|| {
            let hive_pos = bee.hive_pos()?;
            if !bee.wants_to_enter_hive()
                || !closer_to_center_than(hive_pos, bee.position(), HIVE_CLOSE_ENOUGH_DISTANCE)
            {
                return None;
            }
            let block_entity = bee.beehive_block_entity()?;
            let hive = block_entity.downcast_ref::<BeehiveBlockEntity>()?;
            if !hive.is_full() {
                return Some(());
            }
            bee.clear_hive_pos();
            None
        })()
        .is_some();
        base_can_use(bee, can_bee_use)
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        if let Some(block_entity) = bee.beehive_block_entity()
            && let Some(hive) = block_entity.downcast_ref::<BeehiveBlockEntity>()
        {
            hive.add_occupant(bee);
        }
    }
}

/// Vanilla `BeeGoToHiveGoal`: flies home, giving up on hives it cannot reach.
pub(super) struct BeeGoToHiveGoal {
    traveling_ticks: i32,
    last_path: Option<Path>,
    ticks_stuck: i32,
}

impl BeeGoToHiveGoal {
    pub(super) const fn new() -> Self {
        Self {
            traveling_ticks: 0,
            last_path: None,
            ticks_stuck: 0,
        }
    }

    fn can_bee_use(bee: &BeeEntity) -> bool {
        let Some(hive_pos) = bee.hive_pos() else {
            return false;
        };
        !bee.is_too_far_away(hive_pos)
            && !bee.has_home()
            && bee.wants_to_enter_hive()
            && !Self::has_reached_target(bee, hive_pos)
            && bee.level().is_some_and(|world| {
                world
                    .get_block_state(hive_pos)
                    .get_block()
                    .has_tag(&BlockTag::BEEHIVES)
            })
    }

    fn has_reached_target(bee: &BeeEntity, target: BlockPos) -> bool {
        if bee.closer_than(target, 2) {
            return true;
        }
        bee.mob_base()
            .navigation()
            .lock()
            .path()
            .is_some_and(|path| path.target() == target && path.can_reach() && path.is_done())
    }

    fn pathfind_directly_towards(bee: &BeeEntity, target: BlockPos) -> bool {
        let close_enough = if bee.closer_than(target, 3) { 1 } else { 2 };
        bee.mob_base()
            .navigation()
            .lock()
            .set_max_visited_nodes_multiplier(10.0);
        let _ = bee.move_to_pos_with_reach(
            DVec3::new(
                f64::from(target.x()),
                f64::from(target.y()),
                f64::from(target.z()),
            ),
            close_enough,
            1.0,
        );
        bee.mob_base()
            .navigation()
            .lock()
            .path()
            .is_some_and(Path::can_reach)
    }

    fn blacklist_target(bee: &BeeEntity, target: BlockPos) {
        let mut state = bee.state.lock();
        state.blacklisted_hives.push(target);
        while state.blacklisted_hives.len() > MAX_BLACKLISTED_TARGETS {
            state.blacklisted_hives.remove(0);
        }
    }

    fn drop_and_blacklist_hive(bee: &BeeEntity) {
        if let Some(hive_pos) = bee.hive_pos() {
            Self::blacklist_target(bee, hive_pos);
        }
        bee.drop_hive();
    }
}

impl Goal for BeeGoToHiveGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(|bee| base_can_use(bee, Self::can_bee_use(bee)))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob)
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.traveling_ticks = 0;
        self.ticks_stuck = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.traveling_ticks = 0;
        self.ticks_stuck = 0;
        let mut navigation = mob.mob_base().navigation().lock();
        navigation.stop();
        navigation.reset_max_visited_nodes_multiplier();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(bee) = bee(mob) else {
            return;
        };
        let Some(hive_pos) = bee.hive_pos() else {
            return;
        };
        self.traveling_ticks += 1;
        if self.traveling_ticks > adjusted_tick_delay(TRAVEL_TICKS_LIMIT) {
            Self::drop_and_blacklist_hive(bee);
            return;
        }
        if !bee.mob_base().navigation().lock().is_done() {
            return;
        }
        if !bee.closer_than(hive_pos, PATHFIND_TO_HIVE_WHEN_CLOSER_THAN) {
            if bee.is_too_far_away(hive_pos) {
                bee.drop_hive();
            } else {
                bee.pathfind_randomly_towards(hive_pos);
            }
            return;
        }
        if !Self::pathfind_directly_towards(bee, hive_pos) {
            Self::drop_and_blacklist_hive(bee);
            return;
        }
        let current_path = bee.mob_base().navigation().lock().path().cloned();
        let same_as_last = match (&self.last_path, &current_path) {
            (Some(last), Some(current)) => current.same_as(last),
            _ => false,
        };
        if same_as_last {
            self.ticks_stuck += 1;
            if self.ticks_stuck > TICKS_BEFORE_HIVE_DROP {
                bee.drop_hive();
                self.ticks_stuck = 0;
            }
        } else {
            self.last_path = current_path;
        }
    }
}

/// Vanilla `BeeGoToKnownFlowerGoal`: a bee that found no nectar for a while
/// flies back to the flower it remembers.
pub(super) struct BeeGoToKnownFlowerGoal {
    traveling_ticks: i32,
}

impl BeeGoToKnownFlowerGoal {
    pub(super) const fn new() -> Self {
        Self {
            traveling_ticks: 0,
        }
    }

    fn can_bee_use(bee: &BeeEntity) -> bool {
        bee.saved_flower_pos().is_some_and(|flower_pos| {
            !bee.has_home() && bee.wants_to_go_to_known_flower() && !bee.closer_than(flower_pos, 2)
        })
    }
}

impl Goal for BeeGoToKnownFlowerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(|bee| base_can_use(bee, Self::can_bee_use(bee)))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob)
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.traveling_ticks = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.traveling_ticks = 0;
        let mut navigation = mob.mob_base().navigation().lock();
        navigation.stop();
        navigation.reset_max_visited_nodes_multiplier();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(bee) = bee(mob) else {
            return;
        };
        let Some(flower_pos) = bee.saved_flower_pos() else {
            return;
        };
        self.traveling_ticks += 1;
        if self.traveling_ticks > adjusted_tick_delay(TRAVEL_TICKS_LIMIT) {
            bee.drop_flower();
        } else if bee.mob_base().navigation().lock().is_done() {
            if bee.is_too_far_away(flower_pos) {
                bee.drop_flower();
            } else {
                bee.pathfind_randomly_towards(flower_pos);
            }
        }
    }
}

/// Vanilla `BeeGrowCropGoal`: a bee carrying nectar grows the crops it flies over.
pub(super) struct BeeGrowCropGoal;

impl BeeGrowCropGoal {
    fn can_bee_use(bee: &BeeEntity) -> bool {
        if bee.crops_grown_since_pollination() >= MAX_CROPS_GROWABLE {
            return false;
        }
        if rand::random::<f32>() < 0.3 {
            return false;
        }
        bee.has_nectar() && bee.is_hive_valid()
    }

    /// The grown state of a bee-growable plant, vanilla's `CropBlock`,
    /// `StemBlock` and sweet berry cases.
    pub(super) fn grown_state(state: BlockStateId) -> Option<BlockStateId> {
        let block = state.get_block();
        if block == &vanilla_blocks::WHEAT
            || block == &vanilla_blocks::CARROTS
            || block == &vanilla_blocks::POTATOES
        {
            let age = state.get_value(AGE_7);
            (age < AGE_7.max).then(|| state.set_value(AGE_7, age + 1))
        } else if block == &vanilla_blocks::BEETROOTS {
            let age = state.get_value(AGE_3);
            (age < AGE_3.max).then(|| state.set_value(AGE_3, age + 1))
        } else if block == &vanilla_blocks::TORCHFLOWER_CROP {
            // `TorchflowerCropBlock`: maximum age 2 is the torchflower itself.
            let age = state.get_value(AGE_1);
            Some(if age + 1 >= 2 {
                vanilla_blocks::TORCHFLOWER.default_state()
            } else {
                state.set_value(AGE_1, age + 1)
            })
        } else if block == &vanilla_blocks::MELON_STEM || block == &vanilla_blocks::PUMPKIN_STEM {
            let age = state.get_value(AGE_7);
            (age < 7).then(|| state.set_value(AGE_7, age + 1))
        } else if block == &vanilla_blocks::SWEET_BERRY_BUSH {
            let age = state.get_value(AGE_3);
            (age < 3).then(|| state.set_value(AGE_3, age + 1))
        } else {
            None
        }
    }
}

impl Goal for BeeGrowCropGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(|bee| base_can_use(bee, Self::can_bee_use(bee)))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob)
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(bee) = bee(mob) else {
            return;
        };
        if rand::random_range(0..adjusted_tick_delay(GROW_CHANCE)) != 0 {
            return;
        }
        let Some(world) = bee.level() else {
            return;
        };
        for i in 1..=2 {
            let below_pos = bee.block_position().below_n(i);
            let below_state = world.get_block_state(below_pos);
            let below_block = below_state.get_block();
            if !below_block.has_tag(&BlockTag::BEE_GROWABLES) {
                continue;
            }
            let grow_state = if below_block == &vanilla_blocks::CAVE_VINES
                || below_block == &vanilla_blocks::CAVE_VINES_PLANT
            {
                BLOCK_BEHAVIORS
                    .get_behavior(below_block)
                    .as_bonemealable()
                    .and_then(|bonemealable| {
                        if !bonemealable.is_valid_bonemeal_target(
                            below_state,
                            world.as_ref(),
                            below_pos,
                        ) {
                            return None;
                        }
                        bonemealable.perform_bonemeal(
                            below_state,
                            &world,
                            &mut rand::rng(),
                            below_pos,
                        );
                        Some(world.get_block_state(below_pos))
                    })
            } else {
                Self::grown_state(below_state)
            };
            if let Some(grow_state) = grow_state {
                world.level_event(level_events::PARTICLES_BEE_GROWTH, below_pos, 15, None);
                world.set_block(below_pos, grow_state, UpdateFlags::UPDATE_ALL);
                bee.increment_crops_grown_since_pollination();
            }
        }
    }
}

/// Vanilla `BeeLocateHiveGoal`: finds the nearest hive with room.
pub(super) struct BeeLocateHiveGoal;

impl BeeLocateHiveGoal {
    fn find_nearby_hives_with_space(bee: &BeeEntity) -> Vec<BlockPos> {
        let Some(world) = bee.level() else {
            return Vec::new();
        };
        let bee_pos = bee.block_position();
        let radius_sqr = i64::from(HIVE_SEARCH_DISTANCE) * i64::from(HIVE_SEARCH_DISTANCE);
        let candidates = world.poi_storage.lock().get_in_square(
            &|poi_type| {
                REGISTRY.poi_types.by_id(poi_type).is_some_and(|poi_type| {
                    REGISTRY.poi_types.is_in_tag(poi_type, &PoiTag::BEE_HOME)
                })
            },
            bee_pos,
            HIVE_SEARCH_DISTANCE,
            OccupationStatus::Any,
        );
        let dist_sqr = |pos: &BlockPos| {
            let dx = i64::from(pos.x() - bee_pos.x());
            let dy = i64::from(pos.y() - bee_pos.y());
            let dz = i64::from(pos.z() - bee_pos.z());
            dx * dx + dy * dy + dz * dz
        };
        let mut hives = candidates
            .into_iter()
            .map(|(pos, _)| pos)
            .filter(|pos| dist_sqr(pos) <= radius_sqr)
            .filter(|pos| BeeEntity::does_hive_have_space(&world, *pos))
            .collect::<Vec<_>>();
        hives.sort_by_key(dist_sqr);
        hives
    }
}

impl Goal for BeeLocateHiveGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(|bee| {
            let cooldown = bee.state.lock().remaining_cooldown_before_locating_new_hive;
            base_can_use(
                bee,
                cooldown == 0 && !bee.has_hive() && bee.wants_to_enter_hive(),
            )
        })
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        bee.state.lock().remaining_cooldown_before_locating_new_hive =
            COOLDOWN_BEFORE_LOCATING_NEW_HIVE;
        let hives = Self::find_nearby_hives_with_space(bee);
        let Some(&nearest) = hives.first() else {
            return;
        };
        let mut state = bee.state.lock();
        if let Some(pos) = hives
            .iter()
            .find(|pos| !state.blacklisted_hives.contains(pos))
        {
            state.hive_pos = Some(*pos);
            return;
        }
        state.blacklisted_hives.clear();
        state.hive_pos = Some(nearest);
    }
}

/// Vanilla `BeePollinateGoal`: hovers over a nearby flower until it has nectar.
pub(super) struct BeePollinateGoal {
    successful_pollinating_ticks: i32,
    last_sound_played_tick: i32,
    hover_pos: Option<DVec3>,
    pollinating_ticks: i32,
    unreachable_flower_cache: FxHashMap<BlockPos, i64>,
}

impl BeePollinateGoal {
    pub(super) fn new() -> Self {
        Self {
            successful_pollinating_ticks: 0,
            last_sound_played_tick: 0,
            hover_pos: None,
            pollinating_ticks: 0,
            unreachable_flower_cache: FxHashMap::default(),
        }
    }

    const fn has_pollinated_long_enough(&self) -> bool {
        self.successful_pollinating_ticks > MIN_POLLINATION_TICKS
    }

    fn can_bee_use(&mut self, bee: &BeeEntity) -> bool {
        if bee
            .state
            .lock()
            .remaining_cooldown_before_locating_new_flower
            > 0
            || bee.has_nectar()
        {
            return false;
        }
        let Some(world) = bee.level() else {
            return false;
        };
        if world.is_raining() {
            return false;
        }
        if let Some(flower_pos) = self.find_nearby_flower(bee) {
            bee.set_saved_flower_pos(flower_pos);
            let _ = bee.move_to_pos(
                DVec3::new(
                    f64::from(flower_pos.x()) + 0.5,
                    f64::from(flower_pos.y()) + 0.5,
                    f64::from(flower_pos.z()) + 0.5,
                ),
                f64::from(1.2_f32),
            );
            return true;
        }
        bee.state
            .lock()
            .remaining_cooldown_before_locating_new_flower = random_flower_retry_cooldown();
        false
    }

    fn can_bee_continue_to_use(&self, bee: &BeeEntity) -> bool {
        if !bee.is_pollinating() || !bee.has_saved_flower_pos() {
            return false;
        }
        if bee.level().is_some_and(|world| world.is_raining()) {
            return false;
        }
        if self.has_pollinated_long_enough() {
            rand::random::<f32>() < 0.2
        } else {
            true
        }
    }

    fn find_nearby_flower(&mut self, bee: &BeeEntity) -> Option<BlockPos> {
        let world = bee.level()?;
        let game_time = world.game_time();
        let mut temp_cache = FxHashMap::default();
        let mut result = None;
        for pos in bee.block_position().within_manhattan(
            FLOWER_SEARCH_RADIUS,
            FLOWER_SEARCH_RADIUS,
            FLOWER_SEARCH_RADIUS,
        ) {
            if !attracts_bees(world.get_block_state(pos)) {
                continue;
            }
            let unreachable_until = self
                .unreachable_flower_cache
                .get(&pos)
                .copied()
                .unwrap_or(i64::MIN);
            if game_time < unreachable_until {
                temp_cache.insert(pos, unreachable_until);
                continue;
            }
            if bee
                .create_path_to(pos, 1)
                .is_some_and(|path| path.can_reach())
            {
                result = Some(pos);
                break;
            }
            temp_cache.insert(pos, game_time + UNREACHABLE_FLOWER_TICKS);
        }
        self.unreachable_flower_cache = temp_cache;
        result
    }

    fn set_wanted_pos(&self, bee: &BeeEntity) {
        if let Some(hover_pos) = self.hover_pos {
            bee.set_wanted_position(hover_pos, f64::from(POLLINATE_SPEED_MODIFIER));
        }
    }

    fn offset() -> f64 {
        f64::from((rand::random::<f32>() * 2.0 - 1.0) * HOVER_POS_OFFSET)
    }
}

impl Goal for BeePollinateGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(bee) = bee(mob) else {
            return false;
        };
        if bee.is_angry() {
            return false;
        }
        self.can_bee_use(bee)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        bee(mob).is_some_and(|bee| base_can_use(bee, self.can_bee_continue_to_use(bee)))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.successful_pollinating_ticks = 0;
        self.pollinating_ticks = 0;
        self.last_sound_played_tick = 0;
        if let Some(bee) = bee(mob) {
            bee.set_pollinating(true);
            bee.reset_ticks_without_nectar_since_exiting_hive();
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        if self.has_pollinated_long_enough() {
            bee.set_has_nectar(true);
        }
        bee.set_pollinating(false);
        bee.mob_base().navigation().lock().stop();
        bee.state
            .lock()
            .remaining_cooldown_before_locating_new_flower = COOLDOWN_BEFORE_LOCATING_NEW_FLOWER;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(bee) = bee(mob) else {
            return;
        };
        let Some(saved_flower_pos) = bee.saved_flower_pos() else {
            return;
        };
        self.pollinating_ticks += 1;
        if self.pollinating_ticks > MAX_POLLINATING_TICKS {
            bee.drop_flower();
            bee.set_pollinating(false);
            bee.state
                .lock()
                .remaining_cooldown_before_locating_new_flower =
                COOLDOWN_BEFORE_LOCATING_NEW_FLOWER;
            return;
        }
        let flower_pos = DVec3::new(
            f64::from(saved_flower_pos.x()) + 0.5,
            f64::from(saved_flower_pos.y()) + f64::from(HOVER_HEIGHT_WITHIN_FLOWER),
            f64::from(saved_flower_pos.z()) + 0.5,
        );
        if flower_pos.distance(bee.position()) > 1.0 {
            self.hover_pos = Some(flower_pos);
            self.set_wanted_pos(bee);
            return;
        }
        let hover_pos = *self.hover_pos.get_or_insert(flower_pos);
        let arrived_at_hover_pos = bee.position().distance(hover_pos) <= ARRIVAL_THRESHOLD;
        let mut should_set_wanted_pos = true;
        if !arrived_at_hover_pos && self.pollinating_ticks > MAX_POLLINATING_TICKS {
            bee.drop_flower();
            return;
        }
        if arrived_at_hover_pos {
            if rand::random_range(0..POSITION_CHANGE_CHANCE) == 0 {
                self.hover_pos = Some(DVec3::new(
                    flower_pos.x + Self::offset(),
                    flower_pos.y,
                    flower_pos.z + Self::offset(),
                ));
                bee.mob_base().navigation().lock().stop();
            } else {
                should_set_wanted_pos = false;
            }
            let max_head_x_rot = bee.max_head_x_rot();
            bee.mob_base().controls().lock().look_control.set_look_at(
                flower_pos,
                HEAD_ROT_SPEED,
                max_head_x_rot,
            );
        }
        if should_set_wanted_pos {
            self.set_wanted_pos(bee);
        }
        self.successful_pollinating_ticks += 1;
        if rand::random::<f32>() < 0.05
            && self.successful_pollinating_ticks > self.last_sound_played_tick + 60
        {
            self.last_sound_played_tick = self.successful_pollinating_ticks;
            bee.play_sound(&sound_events::ENTITY_BEE_POLLINATE, 1.0, 1.0);
        }
    }
}

/// Vanilla `BeeWanderGoal`: hovers around, drifting back toward a far hive.
pub(super) struct BeeWanderGoal;

impl BeeWanderGoal {
    fn wander_threshold(bee: &BeeEntity) -> i32 {
        let distance_reduction = if !bee.has_hive() && !bee.has_saved_flower_pos() {
            DEFAULT_WANDER_DISTANCE_REDUCTION
        } else {
            RESTRICTED_WANDER_DISTANCE_REDUCTION
        };
        TOO_FAR_DISTANCE - distance_reduction
    }

    fn find_pos(bee: &BeeEntity) -> Option<DVec3> {
        let wander_direction = match bee.hive_pos() {
            Some(hive_pos)
                if bee.is_hive_valid()
                    && !bee.closer_than(hive_pos, Self::wander_threshold(bee)) =>
            {
                let hive_center = DVec3::new(
                    f64::from(hive_pos.x()) + 0.5,
                    f64::from(hive_pos.y()) + 0.5,
                    f64::from(hive_pos.z()) + 0.5,
                );
                (hive_center - bee.position()).normalize_or_zero()
            }
            _ => bee.look_angle(),
        };
        hover_random_pos(
            bee,
            8,
            7,
            wander_direction.x,
            wander_direction.z,
            FRAC_PI_2_F32,
            3,
            1,
        )
        .or_else(|| {
            air_and_water_random_pos(
                bee,
                8,
                4,
                -2,
                wander_direction.x,
                wander_direction.z,
                FRAC_PI_2,
            )
        })
    }
}

impl Goal for BeeWanderGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.mob_base().navigation().lock().is_done() && rand::random_range(0..10) == 0
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.mob_base().navigation().lock().is_done()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        if let Some(target) = Self::find_pos(bee) {
            let path = bee.create_path_to(BlockPos::containing(target.x, target.y, target.z), 1);
            let _ = bee.move_to_path(path, 1.0);
        }
    }
}

/// Vanilla `ValidateFlowerGoal`: forgets a flower that is gone.
pub(super) struct ValidateFlowerGoal {
    validate_flower_cooldown: i32,
    last_validate_tick: i64,
}

impl ValidateFlowerGoal {
    pub(super) fn new() -> Self {
        Self {
            validate_flower_cooldown: random_validate_cooldown(),
            last_validate_tick: -1,
        }
    }
}

impl Goal for ValidateFlowerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(bee) = bee(mob) else {
            return false;
        };
        let game_time = bee.level().map_or(0, |world| world.game_time());
        base_can_use(
            bee,
            game_time > self.last_validate_tick + i64::from(self.validate_flower_cooldown),
        )
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        let Some(world) = bee.level() else {
            return;
        };
        if let Some(flower_pos) = bee.saved_flower_pos()
            && is_loaded(&world, flower_pos)
            && !attracts_bees(world.get_block_state(flower_pos))
        {
            bee.drop_flower();
        }
        self.last_validate_tick = world.game_time();
    }
}

/// Vanilla `ValidateHiveGoal`: forgets a hive that is gone.
pub(super) struct ValidateHiveGoal {
    validate_hive_cooldown: i32,
    last_validate_tick: i64,
}

impl ValidateHiveGoal {
    pub(super) fn new() -> Self {
        Self {
            validate_hive_cooldown: random_validate_cooldown(),
            last_validate_tick: -1,
        }
    }
}

impl Goal for ValidateHiveGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(bee) = bee(mob) else {
            return false;
        };
        let game_time = bee.level().map_or(0, |world| world.game_time());
        base_can_use(
            bee,
            game_time > self.last_validate_tick + i64::from(self.validate_hive_cooldown),
        )
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(bee) = bee(mob) else {
            return;
        };
        let Some(world) = bee.level() else {
            return;
        };
        if let Some(hive_pos) = bee.hive_pos()
            && is_loaded(&world, hive_pos)
            && !bee.is_hive_valid()
        {
            bee.drop_hive();
        }
        self.last_validate_tick = world.game_time();
    }
}
