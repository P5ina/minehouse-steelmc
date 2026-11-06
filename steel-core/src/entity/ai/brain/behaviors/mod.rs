//! Shared brain behaviors (vanilla `net.minecraft.world.entity.ai.behavior`).

mod attack;
mod cooldown;
mod do_nothing;
pub mod gate;
mod look;
mod make_love;
mod move_to_target;
mod panic;
mod swim;
mod temptation;
pub mod utils;
mod walk;

pub use attack::{ChargeAttack, start_attacking};
pub use cooldown::CountDownCooldownTicks;
pub use do_nothing::DoNothing;
pub use gate::run_one;
pub use look::{LookAtTargetSink, RandomLookAround, set_entity_look_target_sometimes};
pub use make_love::AnimalMakeLove;
pub use move_to_target::MoveToTargetSink;
pub use panic::AnimalPanic;
pub use swim::Swim;
pub use temptation::FollowTemptation;
pub use walk::{baby_follow_adult, random_stroll, random_swim, set_walk_target_from_look_target};

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
