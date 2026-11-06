use std::sync::Arc;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_utils::{BlockPos, Identifier};

use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::entity::ai::brain::MemoryStatus;
use crate::entity::ai::brain::behavior::Behavior;
use crate::entity::ai::brain::memory::MemoryRequirement;
use crate::entity::ai::brain::memory_types::{HURT_BY, IS_PANICKING, WALK_TARGET, WalkTarget};
use crate::entity::ai::goal::random_pos::land_random_pos;
use crate::entity::{LivingEntity, PathfinderMob};
use crate::fluid::FluidStateExt as _;
use crate::world::World;

const PANIC_ENTRY: [MemoryRequirement; 2] = [
    MemoryRequirement::new(&IS_PANICKING, MemoryStatus::Registered),
    MemoryRequirement::new(&HURT_BY, MemoryStatus::Registered),
];

type PanicCauses<E> = Box<dyn Fn(&E) -> Identifier + Send>;
type PositionGetter<E> = Box<dyn Fn(&E) -> Option<DVec3> + Send>;

/// Runs from danger after panic-causing damage (vanilla `AnimalPanic`).
pub struct AnimalPanic<E: ?Sized> {
    speed_multiplier: f32,
    panic_causing_damage_types: PanicCauses<E>,
    position_getter: PositionGetter<E>,
}

impl<E: PathfinderMob + ?Sized> AnimalPanic<E> {
    /// Panics on `#panic_causes` and flees to a random land position.
    #[must_use]
    pub fn new(speed_multiplier: f32) -> Self {
        Self::with_damage_types(speed_multiplier, |_| DamageTypeTag::PANIC_CAUSES)
    }

    /// Panics on the damage tag chosen per body and flees to a random land
    /// position.
    #[must_use]
    pub fn with_damage_types(
        speed_multiplier: f32,
        panic_causing_damage_types: impl Fn(&E) -> Identifier + Send + 'static,
    ) -> Self {
        Self::with_position_getter(speed_multiplier, panic_causing_damage_types, |body| {
            land_random_pos(body, 5, 4)
        })
    }

    #[must_use]
    pub fn with_position_getter(
        speed_multiplier: f32,
        panic_causing_damage_types: impl Fn(&E) -> Identifier + Send + 'static,
        position_getter: impl Fn(&E) -> Option<DVec3> + Send + 'static,
    ) -> Self {
        Self {
            speed_multiplier,
            panic_causing_damage_types: Box::new(panic_causing_damage_types),
            position_getter: Box::new(position_getter),
        }
    }

    fn panic_pos(&self, world: &World, body: &E) -> Option<DVec3> {
        if body.is_on_fire()
            && let Some(water) = look_for_water(world, body)
        {
            return Some(DVec3::new(
                f64::from(water.x()) + 0.5,
                f64::from(water.y()),
                f64::from(water.z()) + 0.5,
            ));
        }
        (self.position_getter)(body)
    }
}

impl<E: PathfinderMob + ?Sized> Behavior<E> for AnimalPanic<E> {
    fn entry_condition(&self) -> &[MemoryRequirement] {
        &PANIC_ENTRY
    }

    fn duration(&self) -> (i32, i32) {
        (100, 120)
    }

    fn check_extra_start_conditions(&mut self, _world: &Arc<World>, body: &E) -> bool {
        let Some(memories) = body.brain_memories() else {
            return false;
        };
        let tag = (self.panic_causing_damage_types)(body);
        let memories = memories.lock();
        memories
            .get_ref(&HURT_BY)
            .is_some_and(|source| source.is(&tag))
            || memories.has_value(&IS_PANICKING)
    }

    fn can_still_use(&mut self, _world: &Arc<World>, _body: &E, _time: i64) -> bool {
        true
    }

    fn start(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            let mut memories = memories.lock();
            memories.set(&IS_PANICKING, Some(true));
            memories.erase(&WALK_TARGET);
        }
        body.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, _world: &Arc<World>, body: &E, _time: i64) {
        if let Some(memories) = body.brain_memories() {
            memories.lock().erase(&IS_PANICKING);
        }
    }

    fn tick(&mut self, world: &Arc<World>, body: &E, _time: i64) {
        if !body.mob_base().navigation().lock().is_done() {
            return;
        }
        if let Some(position) = self.panic_pos(world, body)
            && let Some(memories) = body.brain_memories()
        {
            memories.lock().set(
                &WALK_TARGET,
                Some(WalkTarget::to_point(position, self.speed_multiplier, 0)),
            );
        }
    }
}

/// Vanilla `AnimalPanic.lookForWater`: the nearest water within 5 blocks
/// horizontally and 1 vertically, in Manhattan order; two-block-wide mobs
/// need a 2x2 pool.
fn look_for_water(world: &World, body: &(impl LivingEntity + ?Sized)) -> Option<BlockPos> {
    let mob_position = body.block_position();
    let state = world.get_block_state(mob_position);
    if !BLOCK_BEHAVIORS
        .get_behavior(state.get_block())
        .get_collision_shape(state, world, mob_position, BlockCollisionContext::empty())
        .is_empty()
    {
        return None;
    }

    let is_water = |pos: BlockPos| world.get_block_state(pos).get_fluid_state().is_water();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "vanilla Mth.ceil of the bounding box width"
    )]
    let wide = body.bounding_box().width().ceil() as i32 == 2;
    mob_position.within_manhattan(5, 1, 5).find(|&pos| {
        is_water(pos)
            && (!wide
                || is_water(pos.south()) && is_water(pos.east()) && is_water(pos.south().east()))
    })
}
