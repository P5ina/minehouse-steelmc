use std::sync::Arc;

use crate::entity::PathfinderMob;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::world::World;

/// Jumps to stay afloat in water or lava (vanilla `Swim`).
pub struct Swim {
    chance: f32,
}

impl Swim {
    /// Jumps with probability `chance` each tick while in deep fluid.
    #[must_use]
    pub const fn new(chance: f32) -> Self {
        Self { chance }
    }

    // Vanilla checks `isInFluidDeeperThan(getFluidJumpThreshold(),
    // ENTITY_FLOATABLE)`; the floatable fluid tag is water, as in `FloatGoal`.
    fn in_deep_fluid(body: &(impl PathfinderMob + ?Sized)) -> bool {
        body.is_in_water() && body.fluid_contact().water_height() > body.get_fluid_jump_threshold()
            || body.is_in_lava()
    }
}

impl<E: PathfinderMob + ?Sized> Behavior<E> for Swim {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &[]
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        Self::in_deep_fluid(body)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, body: &E, _time: i64) -> bool {
        Self::in_deep_fluid(body)
    }

    fn tick(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if rand::random::<f32>() < self.chance {
            body.mob_base().controls().lock().jump_control.jump();
        }
    }
}
