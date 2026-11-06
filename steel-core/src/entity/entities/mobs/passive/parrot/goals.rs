//! The parrot's own goals: vanilla `Parrot.ParrotWanderGoal` and
//! `LandOnOwnersShoulderGoal`.

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_utils::{BlockPos, Downcast as _};

use super::ParrotEntity;
use crate::entity::ai::goal::{
    Goal, GoalControls, RandomStrollGoal, land_random_pos, random_flying_pos,
};
use crate::entity::leash::Leashable as _;
use crate::entity::{Entity, PathfinderMob, SharedEntity, TamableAnimal};

/// Vanilla `WaterAvoidingRandomStrollGoal.PROBABILITY`.
const TREE_SKIP_PROBABILITY: f32 = 0.001;
const TREE_SEARCH_HORIZONTAL: f64 = 3.0;
const TREE_SEARCH_VERTICAL: f64 = 6.0;
/// Vanilla `LandRandomPos.getPos(mob, 15, 15)` for a parrot in water.
const WATER_ESCAPE_DIST: i32 = 15;

fn as_parrot(mob: &dyn PathfinderMob) -> Option<&ParrotEntity> {
    mob.downcast_ref::<ParrotEntity>()
}

/// Vanilla `Parrot.ParrotWanderGoal`: usually flies to a perch on a nearby
/// tree, otherwise wanders through the air.
pub(super) struct ParrotWanderGoal {
    stroll: RandomStrollGoal,
}

impl ParrotWanderGoal {
    pub(super) const fn new(speed_modifier: f64) -> Self {
        Self {
            stroll: RandomStrollGoal::new(speed_modifier),
        }
    }
}

/// Vanilla `ParrotWanderGoal.getPosition`. The tree perch replaces the escape
/// from water whenever the chance roll passes, as in vanilla.
fn parrot_wander_pos(mob: &dyn PathfinderMob) -> Option<DVec3> {
    let mut pos = None;
    if mob.is_in_water() {
        pos = land_random_pos(mob, WATER_ESCAPE_DIST, WATER_ESCAPE_DIST);
    }
    if rand::random::<f32>() >= TREE_SKIP_PROBABILITY {
        pos = tree_pos(mob);
    }
    pos.or_else(|| random_flying_pos(mob))
}

/// Vanilla `getTreePos`: the first air spot with headroom on leaves or logs.
fn tree_pos(mob: &dyn PathfinderMob) -> Option<DVec3> {
    let world = mob.level()?;
    let mob_pos = mob.block_position();
    let position = mob.position();
    let floor = |value: f64| value.floor() as i32;
    BlockPos::between_closed_coords(
        floor(position.x - TREE_SEARCH_HORIZONTAL),
        floor(position.y - TREE_SEARCH_VERTICAL),
        floor(position.z - TREE_SEARCH_HORIZONTAL),
        floor(position.x + TREE_SEARCH_HORIZONTAL),
        floor(position.y + TREE_SEARCH_VERTICAL),
        floor(position.z + TREE_SEARCH_HORIZONTAL),
    )
    .find(|&pos| {
        if pos == mob_pos {
            return false;
        }
        // Every vanilla `LeavesBlock` is in the leaves tag.
        let below = world.get_block_state(pos.below()).get_block();
        let can_sit_on = below.has_tag(&BlockTag::LEAVES) || below.has_tag(&BlockTag::LOGS);
        can_sit_on
            && world.get_block_state(pos).is_air()
            && world.get_block_state(pos.above()).is_air()
    })
    .map(|pos| {
        DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()),
            f64::from(pos.z()) + 0.5,
        )
    })
}

impl Goal for ParrotWanderGoal {
    fn controls(&self) -> GoalControls {
        self.stroll.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.stroll.can_use_with_position(mob, parrot_wander_pos)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.stroll.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.stroll.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.stroll.stop(mob);
    }
}

/// Vanilla `LandOnOwnersShoulderGoal`: a tame parrot that bumps into its
/// walking owner hops onto their shoulder.
pub(super) struct LandOnOwnersShoulderGoal {
    is_sitting_on_shoulder: bool,
}

impl LandOnOwnersShoulderGoal {
    pub(super) const fn new() -> Self {
        Self {
            is_sitting_on_shoulder: false,
        }
    }
}

impl Goal for LandOnOwnersShoulderGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(parrot) = as_parrot(mob) else {
            return false;
        };
        let Some(owner) = parrot.owner() else {
            return false;
        };
        let Some(player) = owner.as_player() else {
            return false;
        };
        let owner_can_be_sat_on = !player.is_spectator()
            && !player.is_flying()
            && !player.is_in_water()
            && !player.is_in_powder_snow();
        !parrot.is_ordered_to_sit() && owner_can_be_sat_on && parrot.can_sit_on_shoulder()
    }

    fn is_interruptable(&self) -> bool {
        !self.is_sitting_on_shoulder
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.is_sitting_on_shoulder = false;
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(parrot) = as_parrot(mob) else {
            return;
        };
        if self.is_sitting_on_shoulder || parrot.is_in_sitting_pose() || parrot.is_leashed() {
            return;
        }
        if let Some(owner) = parrot.owner()
            && let Some(player) = owner.as_player()
            && parrot.bounding_box().intersects(player.bounding_box())
        {
            self.is_sitting_on_shoulder = parrot.set_entity_on_shoulder(player);
        }
    }
}
