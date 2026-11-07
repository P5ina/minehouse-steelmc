//! Goals shared by raiders: Vanilla `Raider` inner goals, `PathfindToRaidGoal`
//! and `AbstractIllager.RaiderOpenDoorGoal`.

use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use glam::DVec3;
use rustc_hash::FxHashMap;
use steel_registry::item_stack::ItemStack;
use steel_registry::{RegistryEntry as _, vanilla_attributes, vanilla_entities, vanilla_poi_types};
use steel_utils::{BlockPos, Downcast as _};

use super::open_door::OpenDoorGoal;
use super::random_pos::default_random_pos_towards;
use super::reduced_tick_delay;
use super::selector::{Goal, GoalControls, GoalSelector};
use crate::entity::ai::path::Path;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::objects::items::ItemEntity;
use crate::entity::{
    Entity, LivingEntity, PathfinderMob, Raider, SharedEntity, ominous_banner_instance,
};
use crate::inventory::prelude::EquipmentSlot;
use crate::poi::OccupationStatus;
use crate::world::raid::{Raid, Raids};

/// Vanilla `HoldGroundAttackGoal.shoutTargeting` range.
const SHOUT_RANGE: f64 = 8.0;
/// Vanilla `HoldGroundAttackGoal.tick`: one ambient sound every 50 ticks on average.
const AMBIENT_SOUND_CHANCE: i32 = 50;

/// Vanilla `Raider.HoldGroundAttackGoal`: a patrolling raider that spots a
/// target stands still, rallies the raiders around it, and only charges once
/// the target comes within `hostile_radius`.
pub(crate) struct HoldGroundAttackGoal {
    hostile_radius_sqr: f32,
    shout_targeting: TargetingConditions,
}

impl HoldGroundAttackGoal {
    #[must_use]
    pub(crate) const fn new(hostile_radius: f32) -> Self {
        Self {
            hostile_radius_sqr: hostile_radius * hostile_radius,
            shout_targeting: TargetingConditions::for_non_combat()
                .range(SHOUT_RANGE)
                .ignore_line_of_sight()
                .ignore_invisibility_testing(),
        }
    }

    /// Vanilla `getNearbyEntities(Raider.class, shoutTargeting, mob, box.inflate(8))`.
    fn nearby_raiders(&self, mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
        let Some(world) = mob.level() else {
            return Vec::new();
        };
        let search_box = mob
            .bounding_box()
            .inflate_xyz(SHOUT_RANGE, SHOUT_RANGE, SHOUT_RANGE);
        world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity.as_raider().is_some()
                && entity.as_living_entity().is_some_and(|raider| {
                    self.shout_targeting.test(world.as_ref(), Some(mob), raider)
                })
        })
    }
}

impl Goal for HoldGroundAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(raider) = mob.as_raider() else {
            return false;
        };
        raider.current_raid().is_none()
            && raider.is_patrolling()
            && mob.target().is_some()
            && !mob.is_aggressive()
            && mob
                .last_hurt_by_mob()
                .is_none_or(|attacker| attacker.entity_type() != &vanilla_entities::PLAYER)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
        let target = mob.target();
        for entity in self.nearby_raiders(mob) {
            if let Some(other) = entity.as_mob() {
                let _ = other.set_target(target.as_ref());
            }
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(target) = mob.target() else {
            return;
        };
        for entity in self.nearby_raiders(mob) {
            if let Some(other) = entity.as_mob() {
                let _ = other.set_target(Some(&target));
                other.set_aggressive(true);
            }
        }
        mob.set_aggressive(true);
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        if mob.position().distance_squared(target.position()) > f64::from(self.hostile_radius_sqr) {
            let target_position = target.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(target_position.x, target.get_eye_y(), target_position.z),
                30.0,
                30.0,
            );
            if rand::random_range(0..AMBIENT_SOUND_CHANCE) == 0 {
                mob.play_ambient_sound();
            }
        } else {
            mob.set_aggressive(true);
        }
    }
}

/// Vanilla `AbstractIllager.RaiderOpenDoorGoal`: illagers only open doors
/// during a raid.
pub(crate) struct RaiderOpenDoorGoal {
    inner: OpenDoorGoal,
}

impl RaiderOpenDoorGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            inner: OpenDoorGoal::new(false),
        }
    }
}

impl Goal for RaiderOpenDoorGoal {
    fn controls(&self) -> GoalControls {
        self.inner.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_use(mob) && mob.as_raider().is_some_and(Raider::has_active_raid)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.inner.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.inner.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.inner.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.inner.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.inner.tick(mob, entity);
    }
}

/// Vanilla `ObtainRaidLeaderBannerGoal`: how long a banner the raider could
/// not reach is skipped, in ticks.
const UNREACHABLE_BANNER_COOLDOWN: i64 = 600;
/// Vanilla `ObtainRaidLeaderBannerGoal.start` speed.
const OBTAIN_BANNER_SPEED: f64 = 1.15;
/// Vanilla `ObtainRaidLeaderBannerGoal.tick`: the pickup reach.
const OBTAIN_BANNER_PICKUP_DISTANCE: f64 = 1.414;
/// Vanilla `ObtainRaidLeaderBannerGoal.canUse`: the vertical search inflation.
const OBTAIN_BANNER_SEARCH_HEIGHT: f64 = 8.0;

/// Vanilla `Raider.ObtainRaidLeaderBannerGoal`: with no living wave leader, a
/// raider that can lead walks to a dropped ominous banner and takes it.
pub(crate) struct ObtainRaidLeaderBannerGoal {
    unreachable_banner_cache: FxHashMap<i32, i64>,
    path_to_banner: Option<Path>,
    pursued_banner: Option<SharedEntity>,
}

impl ObtainRaidLeaderBannerGoal {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            unreachable_banner_cache: FxHashMap::default(),
            path_to_banner: None,
            pursued_banner: None,
        }
    }

    /// Vanilla `cannotPickUpBanner`.
    fn cannot_pick_up_banner(mob: &dyn PathfinderMob) -> bool {
        let Some(raider) = mob.as_raider() else {
            return true;
        };
        let Some(raid) = raider.current_raid().filter(|raid| raid.is_active()) else {
            return true;
        };
        if raid.is_over() || !raider.can_be_leader() {
            return true;
        }
        let wears_banner = {
            let equipment = mob.living_base().equipment().lock();
            ItemStack::matches(
                equipment.get_ref(EquipmentSlot::Head),
                &ominous_banner_instance(),
            )
        };
        if wears_banner {
            return true;
        }
        raid.leader(raider.wave()).is_some_and(|leader| {
            mob.level()
                .and_then(|world| world.get_entity_by_uuid(&leader))
                .is_some_and(|leader| leader.is_alive())
        })
    }
}

/// Vanilla `Raider.ALLOWED_ITEMS`: a live ominous banner without pickup delay.
fn is_ominous_banner_item(entity: &dyn Entity) -> bool {
    entity.downcast_ref::<ItemEntity>().is_some_and(|item| {
        !item.has_pickup_delay()
            && item.is_alive()
            && ItemStack::matches(&item.get_item(), &ominous_banner_instance())
    })
}

impl Goal for ObtainRaidLeaderBannerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if Self::cannot_pick_up_banner(mob) {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        let follow_range = mob
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(0.0);
        let search_box =
            mob.bounding_box()
                .inflate_xyz(follow_range, OBTAIN_BANNER_SEARCH_HEIGHT, follow_range);
        let game_time = world.game_time();
        let mut temp_cache = FxHashMap::default();
        for banner in world.get_entities_in_aabb_matching(&search_box, is_ominous_banner_item) {
            let unreachable_until = self
                .unreachable_banner_cache
                .get(&banner.id())
                .copied()
                .unwrap_or(i64::MIN);
            if game_time < unreachable_until {
                temp_cache.insert(banner.id(), unreachable_until);
                continue;
            }
            if let Some(path) = mob.create_path_to(banner.block_position(), 1)
                && path.can_reach()
            {
                self.path_to_banner = Some(path);
                self.pursued_banner = Some(banner);
                return true;
            }
            temp_cache.insert(banner.id(), game_time + UNREACHABLE_BANNER_COOLDOWN);
        }
        self.unreachable_banner_cache = temp_cache;
        false
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(banner) = &self.pursued_banner else {
            return false;
        };
        if self.path_to_banner.is_none() || banner.is_removed() {
            return false;
        }
        !mob.mob_base().navigation().lock().is_done() && !Self::cannot_pick_up_banner(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.move_to_path(self.path_to_banner.clone(), OBTAIN_BANNER_SPEED);
    }

    fn stop(&mut self, _mob: &dyn PathfinderMob) {
        self.path_to_banner = None;
        self.pursued_banner = None;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(banner) = &self.pursued_banner else {
            return;
        };
        if banner.position().distance_squared(mob.position())
            >= OBTAIN_BANNER_PICKUP_DISTANCE * OBTAIN_BANNER_PICKUP_DISTANCE
        {
            return;
        }
        if let Some(world) = mob.level()
            && let Some(item_entity) = banner.downcast_ref::<ItemEntity>()
        {
            mob.pick_up_item(&world, item_entity);
        }
    }
}

/// Vanilla `PathfindToRaidGoal.RECRUITMENT_SEARCH_TICK_DELAY`.
const RECRUITMENT_SEARCH_TICK_DELAY: i32 = 20;
/// Vanilla `PathfindToRaidGoal.recruitNearby` search inflation.
const RECRUITMENT_RANGE: f64 = 16.0;

/// Vanilla `PathfindToRaidGoal`: a raid member outside the village walks
/// towards the raid center, recruiting raiders it meets on the way.
pub(crate) struct PathfindToRaidGoal {
    recruitment_tick: i32,
}

impl PathfindToRaidGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            recruitment_tick: 0,
        }
    }

    /// The `canContinueToUse` part shared with `canUse`.
    fn raid_outside_village(mob: &dyn PathfinderMob) -> bool {
        let Some(raider) = mob.as_raider() else {
            return false;
        };
        raider
            .current_raid()
            .is_some_and(|raid| raid.is_active() && !raid.is_over())
            && mob
                .level()
                .is_some_and(|world| !world.is_village(mob.block_position()))
    }

    /// Vanilla `recruitNearby`.
    fn recruit_nearby(mob: &dyn PathfinderMob, raid: &Arc<Raid>) {
        if !raid.is_active() {
            return;
        }
        let Some(world) = mob.level() else {
            return;
        };
        let search_box = mob.bounding_box().inflate(RECRUITMENT_RANGE);
        let recruits = world.get_entities_in_aabb_matching(&search_box, |entity| {
            entity
                .as_raider()
                .is_some_and(|raider| !raider.has_active_raid() && Raids::can_join_raid(raider))
        });
        for recruit in &recruits {
            raid.join_raid(&world, raid.groups_spawned(), recruit, None);
        }
    }
}

impl Goal for PathfindToRaidGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_none()
            && !mob.has_controlling_passenger()
            && Self::raid_outside_village(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        Self::raid_outside_village(mob)
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(raid) = mob
            .as_raider()
            .and_then(Raider::current_raid)
            .filter(|raid| raid.is_active())
        else {
            return;
        };
        if mob.tick_count() > self.recruitment_tick {
            self.recruitment_tick = mob.tick_count() + RECRUITMENT_SEARCH_TICK_DELAY;
            Self::recruit_nearby(mob, &raid);
        }
        if !mob.is_path_finding()
            && let Some(towards) = default_random_pos_towards(
                mob,
                15,
                4,
                block_bottom_center(raid.center()),
                FRAC_PI_2,
            )
        {
            mob.move_to_pos(towards, 1.0);
        }
    }
}

/// Vanilla `RaiderMoveThroughVillageGoal`: the HOME POI search radius.
const VILLAGE_HOME_SEARCH_RADIUS: i32 = 48;
/// Vanilla `RaiderMoveThroughVillageGoal.updateVisited`: the visited list size.
const MAX_VISITED_HOMES: usize = 2;

/// Vanilla `Raider.RaiderMoveThroughVillageGoal`: raid members walk from home
/// to home through the village, skipping the last few they reached.
pub(crate) struct RaiderMoveThroughVillageGoal {
    speed_modifier: f64,
    distance_to_poi: i32,
    poi_pos: Option<BlockPos>,
    visited: Vec<BlockPos>,
    stuck: bool,
}

impl RaiderMoveThroughVillageGoal {
    #[must_use]
    pub(crate) const fn new(speed_modifier: f64, distance_to_poi: i32) -> Self {
        Self {
            speed_modifier,
            distance_to_poi,
            poi_pos: None,
            visited: Vec::new(),
            stuck: false,
        }
    }

    /// Vanilla `isValidRaid`.
    fn is_valid_raid(mob: &dyn PathfinderMob) -> bool {
        mob.as_raider()
            .and_then(Raider::current_raid)
            .is_some_and(|raid| raid.is_active() && !raid.is_over())
    }

    /// Vanilla `hasSuitablePoi`: `PoiManager.getRandom(HOME, hasNotVisited, ANY, pos, 48)`.
    fn has_suitable_poi(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        let Some(home) = vanilla_poi_types::HOME.try_id() else {
            return false;
        };
        let pos = mob.block_position();
        let radius_sqr = i64::from(VILLAGE_HOME_SEARCH_RADIUS * VILLAGE_HOME_SEARCH_RADIUS);
        let candidates: Vec<BlockPos> = world
            .poi_storage
            .lock()
            .get_in_horizontal_square(
                &|poi_type| poi_type == home,
                pos,
                VILLAGE_HOME_SEARCH_RADIUS,
                OccupationStatus::Any,
            )
            .into_iter()
            .map(|(poi_pos, _)| poi_pos)
            .filter(|poi_pos| block_dist_sqr(*poi_pos, pos) <= radius_sqr)
            .filter(|poi_pos| !self.visited.contains(poi_pos))
            .collect();
        if candidates.is_empty() {
            return false;
        }
        self.poi_pos = Some(candidates[rand::random_range(0..candidates.len())]);
        true
    }

    /// Vanilla `updateVisited`.
    fn update_visited(&mut self) {
        if self.visited.len() > MAX_VISITED_HOMES {
            self.visited.remove(0);
        }
    }
}

impl Goal for RaiderMoveThroughVillageGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.update_visited();
        Self::is_valid_raid(mob) && self.has_suitable_poi(mob) && mob.target().is_none()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(poi_pos) = self.poi_pos else {
            return false;
        };
        if mob.mob_base().navigation().lock().is_done() {
            return false;
        }
        mob.target().is_none()
            && !poi_pos.closer_to_center_than(
                mob.position(),
                mob.bounding_box().width() + f64::from(self.distance_to_poi),
            )
            && !self.stuck
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(poi_pos) = self.poi_pos
            && poi_pos.closer_to_center_than(mob.position(), f64::from(self.distance_to_poi))
        {
            self.visited.push(poi_pos);
        }
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.set_no_action_time(0);
        if let Some(poi_pos) = self.poi_pos {
            mob.move_to_pos(
                DVec3::new(
                    f64::from(poi_pos.x()),
                    f64::from(poi_pos.y()),
                    f64::from(poi_pos.z()),
                ),
                self.speed_modifier,
            );
        }
        self.stuck = false;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(poi_pos) = self.poi_pos else {
            return;
        };
        if !mob.mob_base().navigation().lock().is_done() {
            return;
        }
        let poi_vec = block_bottom_center(poi_pos);
        let next_pos = default_random_pos_towards(mob, 16, 7, poi_vec, PI / 10.0)
            .or_else(|| default_random_pos_towards(mob, 8, 7, poi_vec, FRAC_PI_2));
        let Some(next_pos) = next_pos else {
            self.stuck = true;
            return;
        };
        mob.move_to_pos(next_pos, self.speed_modifier);
    }
}

/// Vanilla `RaiderCelebration.tick`: one celebrate sound per 100 ticks on average.
const CELEBRATE_SOUND_CHANCE: i32 = 100;
/// Vanilla `RaiderCelebration.tick`: one jump per 50 ticks on average.
const CELEBRATE_JUMP_CHANCE: i32 = 50;

/// Vanilla `Raider.RaiderCelebration`: after a lost raid the raiders cheer
/// and jump around.
pub(crate) struct RaiderCelebrationGoal;

impl Goal for RaiderCelebrationGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        LivingEntity::is_alive(mob)
            && mob.target().is_none()
            && mob
                .as_raider()
                .and_then(Raider::current_raid)
                .is_some_and(|raid| raid.is_loss())
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(raider) = mob.as_raider() {
            raider.set_celebrating(true);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(raider) = mob.as_raider() {
            raider.set_celebrating(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if !mob.is_silent()
            && rand::random_range(0..reduced_tick_delay(CELEBRATE_SOUND_CHANCE)) == 0
            && let Some(raider) = mob.as_raider()
        {
            mob.make_sound(Some(raider.celebrate_sound()));
        }
        if !mob.is_passenger()
            && rand::random_range(0..reduced_tick_delay(CELEBRATE_JUMP_CHANCE)) == 0
        {
            mob.mob_base().controls().lock().jump_control.jump();
        }
    }
}

/// Vanilla `Raider.registerGoals`, added right after `PatrollingMonster`'s.
pub(crate) fn add_raider_goals(goal_selector: &mut GoalSelector) {
    goal_selector.add_goal(1, ObtainRaidLeaderBannerGoal::new());
    goal_selector.add_goal(3, PathfindToRaidGoal::new());
    goal_selector.add_goal(4, RaiderMoveThroughVillageGoal::new(1.05, 1));
    goal_selector.add_goal(5, RaiderCelebrationGoal);
}

/// Vanilla `Vec3.atBottomCenterOf`.
fn block_bottom_center(pos: BlockPos) -> DVec3 {
    DVec3::new(
        f64::from(pos.x()) + 0.5,
        f64::from(pos.y()),
        f64::from(pos.z()) + 0.5,
    )
}

/// Vanilla `Vec3i.distSqr` between two block positions.
fn block_dist_sqr(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.x() - b.x());
    let dy = i64::from(a.y() - b.y());
    let dz = i64::from(a.z() - b.z());
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
#[path = "raider_goals_tests.rs"]
mod tests;
