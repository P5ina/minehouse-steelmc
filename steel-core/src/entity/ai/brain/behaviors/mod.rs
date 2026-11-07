//! Shared brain behaviors (vanilla `net.minecraft.world.entity.ai.behavior`).

mod attack;
mod cooldown;
mod do_nothing;
mod fight;
pub mod gate;
mod interact;
mod items;
mod long_jump;
mod look;
mod make_love;
mod move_to_target;
mod panic;
mod ram;
mod stroll;
mod swim;
mod temptation;
pub mod utils;
mod walk;

pub use attack::{ChargeAttack, start_attacking};
pub use cooldown::CountDownCooldownTicks;
pub use do_nothing::DoNothing;
pub use fight::{
    erase_memory_if, is_breeding, melee_attack,
    set_walk_target_from_attack_target_if_target_out_of_reach, stop_attacking_if_target_invalid,
};
pub use gate::run_one;
pub use interact::{interact_with_type, set_look_and_interact};
pub use items::{GoAndGiveItemsToTarget, go_to_wanted_item, stay_close_to_target};
pub use long_jump::{LongJumpMidJump, LongJumpToRandomPos};
pub use look::{
    LookAtTargetSink, RandomLookAround, set_entity_look_target, set_entity_look_target_sometimes,
};
pub use make_love::AnimalMakeLove;
pub use move_to_target::MoveToTargetSink;
pub use panic::AnimalPanic;
pub use ram::PrepareRamNearestTarget;
pub use stroll::{
    inside_brownian_walk, set_walk_target_away_from_entity, village_bound_random_stroll,
    village_bound_random_stroll_within,
};
pub use swim::Swim;
pub use temptation::FollowTemptation;
pub use walk::{
    baby_follow_adult, random_fly, random_stroll, random_swim, set_walk_target_from_look_target,
    try_find_liquid,
};

/// An inclusive tick range sampled uniformly (vanilla `UniformInt`).
#[derive(Clone, Copy, Debug)]
pub struct UniformInt {
    pub min_inclusive: i32,
    pub max_inclusive: i32,
}

impl UniformInt {
    #[must_use]
    pub const fn of(min_inclusive: i32, max_inclusive: i32) -> Self {
        Self {
            min_inclusive,
            max_inclusive,
        }
    }

    /// A random value in the range.
    #[must_use]
    pub fn sample(self) -> i32 {
        rand::random_range(self.min_inclusive..=self.max_inclusive)
    }
}
