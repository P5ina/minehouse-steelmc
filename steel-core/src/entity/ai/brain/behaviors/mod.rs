//! Shared brain behaviors (vanilla `net.minecraft.world.entity.ai.behavior`).

mod attack;
mod combat;
mod cooldown;
mod do_nothing;
mod door;
mod fight;
mod find_land;
pub mod gate;
mod interact;
mod long_jump;
mod look;
mod make_love;
mod memory_ops;
mod move_to_target;
mod panic;
mod poi;
mod ram;
mod stroll;
mod swim;
mod temptation;
pub mod utils;
mod walk;

pub use attack::{ChargeAttack, start_attacking};
pub use combat::{
    get_nearest_target, is_other_target_much_further_away_than_current_attack_target, trigger_if,
};
pub use cooldown::CountDownCooldownTicks;
pub use do_nothing::DoNothing;
pub use door::interact_with_door;
pub use fight::{
    erase_memory_if, is_breeding, melee_attack,
    set_walk_target_from_attack_target_if_target_out_of_reach, stop_attacking_if_target_invalid,
};
pub use find_land::{try_find_land, try_find_land_near_liquid, try_lay_spawn_on_fluid_near_land};
pub use gate::{run_one, trigger_one_shuffled};
pub use interact::{interact_with_type, set_look_and_interact};
pub use long_jump::{LongJumpMidJump, LongJumpToRandomPos, default_acceptable_landing_spot};
pub use look::{
    LookAtTargetSink, RandomLookAround, set_entity_look_target, set_entity_look_target_sometimes,
};
pub use make_love::AnimalMakeLove;
pub use memory_ops::become_passive_if_memory_present;
pub use move_to_target::MoveToTargetSink;
pub use panic::AnimalPanic;
pub use poi::{
    SCAN_RANGE, acquire_poi, find_path_to_pois, stroll_around_poi, stroll_to_poi, validate_bed_poi,
    validate_nearby_poi,
};
pub use ram::PrepareRamNearestTarget;
pub use stroll::{
    inside_brownian_walk, set_walk_target_away_from_entity, village_bound_random_stroll,
    village_bound_random_stroll_within,
};
pub use swim::Swim;
pub use temptation::FollowTemptation;
pub use walk::{
    baby_follow_adult, random_stroll, random_swim, set_walk_target_away_from_pos,
    set_walk_target_from_look_target, try_find_liquid,
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
