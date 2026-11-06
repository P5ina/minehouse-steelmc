use steel_utils::Downcast;

use crate::entity::ai::goal::selector::{Goal, GoalControls};
use crate::entity::entities::CreeperEntity;
use crate::entity::{PathfinderMob, SharedEntity};

/// Squared distance under which a creeper starts swelling at its target.
const START_DISTANCE_SQR: f64 = 9.0;
/// Squared distance beyond which a swelling creeper calms down again.
const STOP_DISTANCE_SQR: f64 = 49.0;

/// Vanilla `SwellGoal`: a creeper near its target stops walking and swells.
pub struct SwellGoal {
    target: Option<SharedEntity>,
}

impl SwellGoal {
    pub const fn new() -> Self {
        Self { target: None }
    }
}

fn creeper(mob: &dyn PathfinderMob) -> Option<&CreeperEntity> {
    let creeper = mob.downcast_ref::<CreeperEntity>();
    if creeper.is_none() {
        tracing::warn!("SwellGoal assigned to non-creeper entity");
    }
    creeper
}

impl Goal for SwellGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(creeper) = creeper(mob) else {
            return false;
        };
        creeper.swell_dir() > 0
            || mob.target().is_some_and(|target| {
                target
                    .as_living_entity()
                    .is_some_and(|living| !living.is_dead_or_dying())
                    && mob.distance_to_sqr(target.position()) < START_DISTANCE_SQR
            })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        mob.mob_base().navigation().lock().stop();
        self.target = mob.target();
    }

    fn stop(&mut self, _mob: &dyn PathfinderMob) {
        self.target = None;
    }

    fn requires_update_every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(creeper) = creeper(mob) else {
            return;
        };
        let living_target = self
            .target
            .as_ref()
            .and_then(|target| target.as_living_entity())
            .filter(|living| !living.is_dead_or_dying());
        let dir = match living_target {
            Some(target) if mob.distance_to_sqr(target.position()) > STOP_DISTANCE_SQR => -1,
            Some(target) if !mob.has_line_of_sight_cached(target) => -1,
            Some(_) => 1,
            None => -1,
        };
        creeper.set_swell_dir(dir);
    }
}
