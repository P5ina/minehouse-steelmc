//! Goals of vanilla `AbstractCubeMob`. Each one only works on a cube mob, like
//! vanilla's `getMoveControl() instanceof CubeMobMoveControl` checks.

use steel_registry::vanilla_mob_effects;

use crate::entity::ai::goal::{Goal, GoalControls, reduced_tick_delay};
use crate::entity::{Mob, MobBase, PathfinderMob, SharedEntity};

const GROW_TIRED_TICKS: i32 = 300;
const ATTACK_LOOK_MAX_ROT: f32 = 10.0;
const FLOAT_JUMP_CHANCE: f32 = 0.8;
const FLOAT_SPEED_MODIFIER: f64 = 1.2;

/// Vanilla `CubeMobAttackGoal`: faces the target and hops at it, tiring after 15 s.
pub(crate) struct CubeMobAttackGoal {
    grow_tired_timer: i32,
}

impl CubeMobAttackGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            grow_tired_timer: 0,
        }
    }
}

fn can_attack_target(mob: &dyn PathfinderMob) -> Option<SharedEntity> {
    let target = mob.target()?;
    let living = target.as_living_entity()?;
    Mob::can_attack(mob, living).then_some(target)
}

impl Goal for CubeMobAttackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.as_cube_mob().is_some() && can_attack_target(mob).is_some()
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.grow_tired_timer = reduced_tick_delay(GROW_TIRED_TICKS);
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if can_attack_target(mob).is_none() {
            return false;
        }
        self.grow_tired_timer -= 1;
        self.grow_tired_timer > 0
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(target) = mob.target() {
            mob.mob_look_at(target.as_ref(), ATTACK_LOOK_MAX_ROT, ATTACK_LOOK_MAX_ROT);
        }
        if let Some(cube) = mob.as_cube_mob() {
            cube.set_move_direction(cube.rotation().0, cube.can_deal_damage());
        }
    }
}

/// Vanilla `CubeMobFloatGoal`: keeps jumping up through liquids.
pub(crate) struct CubeMobFloatGoal;

impl CubeMobFloatGoal {
    #[must_use]
    pub(crate) fn new(mob_base: &MobBase) -> Self {
        mob_base.navigation().lock().set_can_float(true);
        Self
    }
}

impl Goal for CubeMobFloatGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        (mob.is_in_water() || mob.is_in_lava()) && mob.as_cube_mob().is_some()
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if rand::random::<f32>() < FLOAT_JUMP_CHANCE {
            mob.jump_control_jump();
        }
        if let Some(cube) = mob.as_cube_mob() {
            cube.set_wanted_movement(FLOAT_SPEED_MODIFIER);
        }
    }
}

/// Vanilla `CubeMobKeepOnJumpingGoal`: always hop forward unless riding.
pub(crate) struct CubeMobKeepOnJumpingGoal;

impl Goal for CubeMobKeepOnJumpingGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.is_passenger()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(cube) = mob.as_cube_mob() {
            cube.set_wanted_movement(1.0);
        }
    }
}

/// Vanilla `CubeMobRandomDirectionGoal`: turns to a random heading every 2 to 5 s.
pub(crate) struct CubeMobRandomDirectionGoal {
    chosen_degrees: f32,
    next_randomize_time: i32,
}

impl CubeMobRandomDirectionGoal {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            chosen_degrees: 0.0,
            next_randomize_time: 0,
        }
    }
}

impl Goal for CubeMobRandomDirectionGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.target().is_none()
            && (mob.on_ground()
                || mob.is_in_water()
                || mob.is_in_lava()
                || mob.has_mob_effect(vanilla_mob_effects::LEVITATION))
            && mob.as_cube_mob().is_some()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.next_randomize_time -= 1;
        if self.next_randomize_time <= 0 {
            // Vanilla `adjustedTickDelay`: this goal does not tick every tick.
            self.next_randomize_time = reduced_tick_delay(40 + rand::random_range(0..60));
            self.chosen_degrees = rand::random_range(0..360) as f32;
        }
        if let Some(cube) = mob.as_cube_mob() {
            cube.set_move_direction(self.chosen_degrees, false);
        }
    }
}
