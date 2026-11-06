//! Vanilla `Phantom` inner goals: target the highest visible player, circle
//! the anchor, and swoop.

use std::f32::consts::PI;
use std::sync::Arc;

use glam::DVec3;
use steel_math::trig;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::{level_events, sound_events, vanilla_entities};
use steel_utils::types::GameType;
use steel_utils::{BlockPos, Downcast as _};

use super::{AttackPhase, PhantomEntity};
use crate::chunk::heightmap::HeightmapType;
use crate::entity::ai::goal::{Goal, GoalControls, reduced_tick_delay};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{Entity, LivingEntity, Mob, PathfinderMob, SharedEntity};
use crate::player::Player;
use crate::world::{LevelReader as _, World};

const TARGET_SCAN_RANGE: f64 = 64.0;
const TARGET_SCAN_HORIZONTAL: f64 = 16.0;
const TARGET_SCAN_VERTICAL: f64 = 64.0;
const CAT_SEARCH_TICK_DELAY: i32 = 20;
const CAT_SEARCH_RANGE: f64 = 16.0;
const BITE_REACH: f64 = 0.2;
const TOUCHING_TARGET_DISTANCE_SQR: f64 = 4.0;

/// Vanilla `Goal.adjustedTickDelay` for a goal that does not tick every tick.
const fn adjusted_tick_delay(ticks: i32) -> i32 {
    reduced_tick_delay(ticks)
}

fn phantom(mob: &dyn PathfinderMob) -> Option<&PhantomEntity> {
    mob.downcast_ref::<PhantomEntity>()
}

/// Vanilla `Phantom.canAttack(level, target, TargetingConditions.DEFAULT)`.
fn can_attack_default(world: &World, phantom: &PhantomEntity, target: &dyn LivingEntity) -> bool {
    TargetingConditions::for_combat().test(world, Some(phantom), target)
}

fn can_attack_current_target(phantom: &PhantomEntity) -> bool {
    let (Some(target), Some(world)) = (phantom.target(), phantom.level()) else {
        return false;
    };
    target
        .as_living_entity()
        .is_some_and(|target| can_attack_default(&world, phantom, target))
}

/// Vanilla `Phantom.PhantomAttackPlayerTargetGoal`: every three seconds,
/// target the highest player within 64 blocks it can see.
pub(super) struct PhantomAttackPlayerTargetGoal {
    next_scan_tick: i32,
}

impl PhantomAttackPlayerTargetGoal {
    pub(super) const fn new() -> Self {
        Self {
            next_scan_tick: reduced_tick_delay(20),
        }
    }
}

impl Goal for PhantomAttackPlayerTargetGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self.next_scan_tick > 0 {
            self.next_scan_tick -= 1;
            return false;
        }
        self.next_scan_tick = reduced_tick_delay(60);
        let (Some(phantom), Some(world)) = (phantom(mob), mob.level()) else {
            return false;
        };
        let attack_targeting = TargetingConditions::for_combat().range(TARGET_SCAN_RANGE);
        let search_box = phantom.bounding_box().inflate_xyz(
            TARGET_SCAN_HORIZONTAL,
            TARGET_SCAN_VERTICAL,
            TARGET_SCAN_HORIZONTAL,
        );
        // Vanilla `getNearbyPlayers`: players standing in the box that pass the conditions.
        let mut players: Vec<Arc<Player>> = Vec::new();
        world.players.iter_players(|_, player| {
            if search_box.contains(player.position())
                && attack_targeting.test(&world, Some(phantom), player.as_ref())
            {
                players.push(Arc::clone(player));
            }
            true
        });
        // Vanilla sorts by Y, highest first; the sort is stable.
        players.sort_by(|a, b| b.position().y.total_cmp(&a.position().y));
        for player in players {
            if can_attack_default(&world, phantom, player.as_ref()) {
                let target: SharedEntity = player;
                phantom.set_target(Some(&target));
                return true;
            }
        }
        false
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        phantom(mob).is_some_and(can_attack_current_target)
    }
}

/// Vanilla `Phantom.PhantomAttackStrategyGoal`: circle above the target, then
/// swoop every 8 to 12 seconds.
pub(super) struct PhantomAttackStrategyGoal {
    next_sweep_tick: i32,
}

impl PhantomAttackStrategyGoal {
    pub(super) const fn new() -> Self {
        Self { next_sweep_tick: 0 }
    }

    /// Vanilla `setAnchorAboveTarget`.
    fn set_anchor_above_target(phantom: &PhantomEntity) {
        let (Some(target), Some(world)) = (phantom.target(), phantom.level()) else {
            return;
        };
        let mut state = phantom.state().lock();
        if state.anchor_point.is_none() {
            return;
        }
        let mut anchor = target
            .block_position()
            .above_n(20 + rand::random_range(0..20));
        let sea_level = world.sea_level();
        if anchor.y() < sea_level {
            anchor = BlockPos::new(anchor.x(), sea_level + 1, anchor.z());
        }
        state.anchor_point = Some(anchor);
    }
}

impl Goal for PhantomAttackStrategyGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        phantom(mob).is_some_and(can_attack_current_target)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(phantom) = phantom(mob) else {
            return;
        };
        self.next_sweep_tick = adjusted_tick_delay(10);
        phantom.state().lock().attack_phase = AttackPhase::Circle;
        Self::set_anchor_above_target(phantom);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let (Some(phantom), Some(world)) = (phantom(mob), mob.level()) else {
            return;
        };
        let mut state = phantom.state().lock();
        if let Some(anchor) = state.anchor_point {
            state.anchor_point = Some(
                world
                    .heightmap_pos(HeightmapType::MotionBlocking, anchor)
                    .above_n(10 + rand::random_range(0..20)),
            );
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(phantom) = phantom(mob) else {
            return;
        };
        if phantom.state().lock().attack_phase != AttackPhase::Circle {
            return;
        }
        self.next_sweep_tick -= 1;
        if self.next_sweep_tick <= 0 {
            phantom.state().lock().attack_phase = AttackPhase::Swoop;
            Self::set_anchor_above_target(phantom);
            self.next_sweep_tick = adjusted_tick_delay((8 + rand::random_range(0..4)) * 20);
            phantom.play_sound(
                &sound_events::ENTITY_PHANTOM_SWOOP,
                10.0,
                0.95 + rand::random::<f32>() * 0.1,
            );
        }
    }
}

/// Vanilla `PhantomMoveTargetGoal.touchingTarget`.
fn touching_target(phantom: &PhantomEntity) -> bool {
    phantom
        .move_target_point()
        .distance_squared(phantom.position())
        < TOUCHING_TARGET_DISTANCE_SQR
}

/// Vanilla `Phantom.PhantomCircleAroundAnchorGoal`.
pub(super) struct PhantomCircleAroundAnchorGoal {
    angle: f32,
    distance: f32,
    height: f32,
    clockwise: f32,
}

impl PhantomCircleAroundAnchorGoal {
    pub(super) const fn new() -> Self {
        Self {
            angle: 0.0,
            distance: 0.0,
            height: 0.0,
            clockwise: 0.0,
        }
    }

    /// Vanilla `selectNext`: the next point 15 degrees around the anchor.
    fn select_next(&mut self, phantom: &PhantomEntity) {
        let block_position = phantom.block_position();
        let mut state = phantom.state().lock();
        let anchor = *state.anchor_point.get_or_insert(block_position);
        self.angle += self.clockwise * 15.0 * (PI / 180.0);
        state.move_target_point = DVec3::new(
            f64::from(anchor.x()),
            f64::from(anchor.y()),
            f64::from(anchor.z()),
        ) + DVec3::new(
            f64::from(self.distance * trig::cos(self.angle)),
            f64::from(-4.0 + self.height),
            f64::from(self.distance * trig::sin(self.angle)),
        );
    }
}

impl Goal for PhantomCircleAroundAnchorGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        phantom(mob).is_some_and(|phantom| {
            phantom.target().is_none() || phantom.state().lock().attack_phase == AttackPhase::Circle
        })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(phantom) = phantom(mob) else {
            return;
        };
        self.distance = 5.0 + rand::random::<f32>() * 10.0;
        self.height = -4.0 + rand::random::<f32>() * 9.0;
        self.clockwise = if rand::random::<bool>() { 1.0 } else { -1.0 };
        self.select_next(phantom);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let (Some(phantom), Some(world)) = (phantom(mob), mob.level()) else {
            return;
        };
        if rand::random_range(0..adjusted_tick_delay(350)) == 0 {
            self.height = -4.0 + rand::random::<f32>() * 9.0;
        }
        if rand::random_range(0..adjusted_tick_delay(250)) == 0 {
            self.distance += 1.0;
            if self.distance > 15.0 {
                self.distance = 5.0;
                self.clockwise = -self.clockwise;
            }
        }
        if rand::random_range(0..adjusted_tick_delay(450)) == 0 {
            self.angle = rand::random::<f32>() * 2.0 * PI;
            self.select_next(phantom);
        }
        if touching_target(phantom) {
            self.select_next(phantom);
        }

        let position = phantom.position();
        let block_position = phantom.block_position();
        if phantom.move_target_point().y < position.y
            && !world.get_block_state(block_position.below()).is_air()
        {
            self.height = self.height.max(1.0);
            self.select_next(phantom);
        }
        if phantom.move_target_point().y > position.y
            && !world.get_block_state(block_position.above()).is_air()
        {
            self.height = self.height.min(-1.0);
            self.select_next(phantom);
        }
    }
}

/// Vanilla `Phantom.PhantomSweepAttackGoal`: dive at the target and bite it,
/// unless a cat is near.
pub(super) struct PhantomSweepAttackGoal {
    is_scared_of_cat: bool,
    cat_search_tick: i32,
}

impl PhantomSweepAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            is_scared_of_cat: false,
            cat_search_tick: 0,
        }
    }

    fn swooping_at_target(phantom: &PhantomEntity) -> bool {
        phantom.target().is_some() && phantom.state().lock().attack_phase == AttackPhase::Swoop
    }
}

impl Goal for PhantomSweepAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        phantom(mob).is_some_and(Self::swooping_at_target)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let (Some(phantom), Some(world)) = (phantom(mob), mob.level()) else {
            return false;
        };
        let Some(target) = phantom.target() else {
            return false;
        };
        if !target.is_alive() {
            return false;
        }
        if target
            .as_player()
            .is_some_and(|player| player.game_mode() == GameType::Creative)
            || target.is_spectator()
        {
            return false;
        }
        if !Self::swooping_at_target(phantom) {
            return false;
        }
        if phantom.tick_count() > self.cat_search_tick {
            self.cat_search_tick = phantom.tick_count() + CAT_SEARCH_TICK_DELAY;
            let cats = world.get_entities_in_aabb_matching(
                &phantom.bounding_box().inflate(CAT_SEARCH_RANGE),
                |entity| entity.entity_type() == &vanilla_entities::CAT && entity.is_alive(),
            );
            for cat in &cats {
                // Vanilla `Cat.hiss`.
                if let Some(cat) = cat.as_living_entity() {
                    cat.make_sound(Some(&sound_events::ENTITY_CAT_HISS));
                }
            }
            self.is_scared_of_cat = !cats.is_empty();
        }
        !self.is_scared_of_cat
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(phantom) = phantom(mob) else {
            return;
        };
        phantom.set_target(None);
        phantom.state().lock().attack_phase = AttackPhase::Circle;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let (Some(phantom), Some(world)) = (phantom(mob), mob.level()) else {
            return;
        };
        let Some(target) = phantom.target() else {
            return;
        };
        let target_position = target.position();
        let bounding_box = target.bounding_box();
        phantom.state().lock().move_target_point = DVec3::new(
            target_position.x,
            target_position.y + (bounding_box.max_y() - bounding_box.min_y()) * 0.5,
            target_position.z,
        );
        if phantom
            .bounding_box()
            .inflate(f64::from(BITE_REACH as f32))
            .intersects(bounding_box)
        {
            let _ = phantom.do_hurt_target(entity, &world, &target);
            phantom.state().lock().attack_phase = AttackPhase::Circle;
            if !phantom.is_silent() {
                world.level_event(
                    level_events::SOUND_PHANTOM_BITE,
                    phantom.block_position(),
                    0,
                    None,
                );
            }
        } else if phantom.horizontal_collision() || phantom.hurt_time() > 0 {
            phantom.state().lock().attack_phase = AttackPhase::Circle;
        }
    }
}
