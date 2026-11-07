//! Shared brain behaviors (vanilla `net.minecraft.world.entity.ai.behavior`).

mod combat;
mod cooldown;
mod do_nothing;
pub mod gate;
mod long_jump;
mod look;
mod make_love;
mod memory_ops;
mod move_to_target;
mod panic;
mod ram;
mod swim;
mod temptation;
pub mod utils;
mod walk;

pub use combat::{
    get_nearest_target, is_other_target_much_further_away_than_current_attack_target, melee_attack,
    set_walk_target_from_attack_target_if_target_out_of_reach, start_attacking,
    stop_attacking_if_target_invalid, trigger_if,
};
pub use cooldown::CountDownCooldownTicks;
pub use do_nothing::DoNothing;
pub use gate::run_one;
pub use long_jump::{LongJumpMidJump, LongJumpToRandomPos};
pub use look::{LookAtTargetSink, RandomLookAround, set_entity_look_target_sometimes};
pub use make_love::AnimalMakeLove;
pub use memory_ops::{become_passive_if_memory_present, erase_memory_if};
pub use move_to_target::MoveToTargetSink;
pub use panic::AnimalPanic;
pub use ram::PrepareRamNearestTarget;
pub use swim::Swim;
pub use temptation::FollowTemptation;
pub use walk::{
    baby_follow_adult, random_stroll, set_walk_target_away_from_entity,
    set_walk_target_away_from_pos, set_walk_target_from_look_target,
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
