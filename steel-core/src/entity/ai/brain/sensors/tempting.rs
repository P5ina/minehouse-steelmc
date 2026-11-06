use std::sync::Arc;

use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_attributes;

use crate::entity::ai::brain::memory_types::TEMPTING_PLAYER;
use crate::entity::ai::brain::{Memories, Sensor};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::{Animal, Entity, LivingEntity, PathfinderMob, SharedEntity};
use crate::world::World;

type Temptation<E> = Box<dyn Fn(&E, &ItemStack) -> bool + Send + Sync>;

/// Remembers the closest player holding a tempting item
/// (vanilla `TemptingSensor`).
pub struct TemptingSensor<E: ?Sized> {
    temptations: Temptation<E>,
}

impl<E: ?Sized> TemptingSensor<E> {
    /// Tempted by items matching `temptation`.
    #[must_use]
    pub fn new(temptation: impl Fn(&ItemStack) -> bool + Send + Sync + 'static) -> Self {
        Self {
            temptations: Box::new(move |_, item| temptation(item)),
        }
    }
}

impl<E: Animal> TemptingSensor<E> {
    /// Tempted by the animal's food (vanilla `TemptingSensor.forAnimal`).
    #[must_use]
    pub fn for_animal() -> Self {
        Self {
            temptations: Box::new(Animal::is_food),
        }
    }
}

impl<E: PathfinderMob> Sensor<E> for TemptingSensor<E> {
    fn register_memories(&self, memories: &mut Memories) {
        memories.register(&TEMPTING_PLAYER);
    }

    fn do_tick(&mut self, world: &Arc<World>, body: &E) {
        let Some(memories) = body.brain_memories() else {
            return;
        };
        let range = body
            .attributes()
            .lock()
            .required_value(vanilla_attributes::TEMPT_RANGE);
        let targeting = TargetingConditions::for_non_combat()
            .ignore_line_of_sight()
            .range(range);
        let position = body.position();
        let mut nearest: Option<(SharedEntity, f64)> = None;
        world.players.iter_players(|_, player| {
            if player.is_spectator()
                || !targeting.test(world.as_ref(), Some(body), player.as_ref())
                || !player.is_holding(&mut |item| (self.temptations)(body, item))
                || body.has_passenger(player.as_ref())
            {
                return true;
            }
            let distance = player.position().distance_squared(position);
            if nearest.as_ref().is_none_or(|(_, best)| distance < *best) {
                nearest = Some((Arc::clone(player) as SharedEntity, distance));
            }
            true
        });
        memories
            .lock()
            .set(&TEMPTING_PLAYER, nearest.map(|(player, _)| player));
    }
}
