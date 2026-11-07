//! Rabbit-specific goals.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::entity_variant::RabbitVariant;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{level_events, vanilla_blocks, vanilla_entities, vanilla_game_events};
use steel_utils::Downcast as _;
use steel_utils::types::UpdateFlags;

use super::RabbitEntity;
use crate::entity::ai::goal::{AvoidEntityGoal, Goal, GoalControls, MoveToBlockGoal, PanicGoal};
use crate::entity::{Entity, PathfinderMob, SharedEntity, entity_selector};
use crate::world::World;
use crate::world::game_event::GameEventContext;

/// Vanilla `CarrotBlock.MAX_AGE`.
const CARROT_MAX_AGE: u8 = 7;

fn rabbit(mob: &dyn PathfinderMob) -> Option<&RabbitEntity> {
    mob.as_entity_event_source().downcast_ref::<RabbitEntity>()
}

/// Vanilla `Rabbit.RabbitPanicGoal`: also speeds up the next hop.
pub(super) struct RabbitPanicGoal {
    panic: PanicGoal,
    speed_modifier: f64,
}

impl RabbitPanicGoal {
    pub(super) const fn new(speed_modifier: f64) -> Self {
        Self {
            panic: PanicGoal::new(speed_modifier),
            speed_modifier,
        }
    }
}

impl Goal for RabbitPanicGoal {
    fn controls(&self) -> GoalControls {
        self.panic.controls()
    }

    fn is_panic_goal(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.panic.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.panic.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.panic.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.panic.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.panic.tick(mob, entity);
        if let Some(rabbit) = rabbit(mob) {
            rabbit.set_speed_modifier(self.speed_modifier);
        }
    }
}

/// Vanilla `Rabbit.RabbitAvoidEntityGoal`: the killer bunny flees nothing.
pub(super) struct RabbitAvoidEntityGoal {
    avoid: AvoidEntityGoal,
}

impl RabbitAvoidEntityGoal {
    fn new(max_dist: f32, is_avoided: fn(&dyn Entity) -> bool) -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(
                max_dist,
                super::FLEE_SPEED_MOD,
                super::FLEE_SPEED_MOD,
                move |target, _| {
                    let entity = target.as_entity_event_source();
                    is_avoided(entity) && entity_selector::no_creative_or_spectator(entity)
                },
            ),
        }
    }

    /// `RabbitAvoidEntityGoal(rabbit, Player.class, 8.0F, 2.2, 2.2)`.
    pub(super) fn players() -> Self {
        Self::new(8.0, |entity| entity.as_player().is_some())
    }

    /// `RabbitAvoidEntityGoal(rabbit, Wolf.class, 10.0F, 2.2, 2.2)`.
    pub(super) fn wolves() -> Self {
        Self::new(10.0, |entity| {
            entity.entity_type() == &vanilla_entities::WOLF
        })
    }

    /// `RabbitAvoidEntityGoal(rabbit, Monster.class, 4.0F, 2.2, 2.2)`.
    pub(super) fn monsters() -> Self {
        Self::new(4.0, Entity::is_monster)
    }
}

impl Goal for RabbitAvoidEntityGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        rabbit(mob).is_some_and(|rabbit| rabbit.variant() != RabbitVariant::Evil)
            && self.avoid.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.avoid.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.avoid.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.avoid.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.avoid.tick(mob, entity);
    }
}

/// Vanilla `Rabbit.RaidGardenGoal`: walks to a fully grown carrot and nibbles
/// it down one stage, or eats it when it is a sprout.
pub(super) struct RaidGardenGoal {
    move_to: MoveToBlockGoal,
    wants_to_raid: Arc<AtomicBool>,
    can_raid: Arc<AtomicBool>,
}

impl RaidGardenGoal {
    pub(super) fn new() -> Self {
        let wants_to_raid = Arc::new(AtomicBool::new(false));
        let can_raid = Arc::new(AtomicBool::new(false));
        let target_wants_to_raid = Arc::clone(&wants_to_raid);
        let target_can_raid = Arc::clone(&can_raid);
        Self {
            move_to: MoveToBlockGoal::new(f64::from(0.7_f32), 16, move |world, pos| {
                is_valid_target(world, pos, &target_wants_to_raid, &target_can_raid)
            }),
            wants_to_raid,
            can_raid,
        }
    }

    /// The part of vanilla `tick` after reaching the farmland.
    fn raid(&mut self, mob: &dyn PathfinderMob, rabbit: &RabbitEntity) {
        let Some(world) = mob.level() else {
            return;
        };
        let crops_pos = self.move_to.block_pos().above();
        let state = world.get_block_state(crops_pos);
        if self.can_raid.load(Ordering::Relaxed) && state.get_block() == &vanilla_blocks::CARROTS {
            let carrot_age = state.get_value(&BlockStateProperties::AGE_7);
            if carrot_age == 0 {
                world.set_block(
                    crops_pos,
                    vanilla_blocks::AIR.default_state(),
                    UpdateFlags::UPDATE_CLIENTS,
                );
                world.destroy_block_by_entity(crops_pos, true, mob.as_entity_event_source());
            } else {
                world.set_block(
                    crops_pos,
                    state.set_value(&BlockStateProperties::AGE_7, carrot_age - 1),
                    UpdateFlags::UPDATE_CLIENTS,
                );
                world.game_event(
                    &vanilla_game_events::BLOCK_CHANGE,
                    crops_pos,
                    &GameEventContext::new(Some(mob.as_entity_event_source()), None),
                );
                world.level_event(
                    level_events::PARTICLES_DESTROY_BLOCK,
                    crops_pos,
                    level_events::encode_block_state_data(u32::from(state.0)),
                    None,
                );
            }
            rabbit.ate_carrot();
        }

        self.can_raid.store(false, Ordering::Relaxed);
        *self.move_to.next_start_tick_mut() = 10;
    }
}

/// Vanilla `RaidGardenGoal.isValidTarget`: farmland under a ripe carrot,
/// while the rabbit is hungry.
fn is_valid_target(
    world: &World,
    pos: steel_utils::BlockPos,
    wants_to_raid: &AtomicBool,
    can_raid: &AtomicBool,
) -> bool {
    if world
        .get_block_state(pos)
        .get_block()
        .has_tag(&BlockTag::SUPPORTS_CROPS)
        && wants_to_raid.load(Ordering::Relaxed)
        && !can_raid.load(Ordering::Relaxed)
    {
        let state = world.get_block_state(pos.above());
        if state.get_block() == &vanilla_blocks::CARROTS
            && state.get_value(&BlockStateProperties::AGE_7) >= CARROT_MAX_AGE
        {
            can_raid.store(true, Ordering::Relaxed);
            return true;
        }
    }
    false
}

impl Goal for RaidGardenGoal {
    fn controls(&self) -> GoalControls {
        self.move_to.controls()
    }

    fn requires_update_every_tick(&self) -> bool {
        self.move_to.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if *self.move_to.next_start_tick_mut() <= 0 {
            let Some(world) = mob.level() else {
                return false;
            };
            if !world.get_game_rule(&MOB_GRIEFING) {
                return false;
            }
            self.can_raid.store(false, Ordering::Relaxed);
            self.wants_to_raid.store(
                rabbit(mob).is_some_and(RabbitEntity::wants_more_food),
                Ordering::Relaxed,
            );
        }
        self.move_to.can_use(mob)
    }

    /// Vanilla quirk kept as is: once `canRaid` is set the target check
    /// fails, so the raid ends on the next goal update.
    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_raid.load(Ordering::Relaxed) && self.move_to.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.move_to.tick(mob, entity);
        let block_pos = self.move_to.block_pos();
        mob.mob_base().controls().lock().look_control.set_look_at(
            glam::DVec3::new(
                f64::from(block_pos.x()) + 0.5,
                f64::from(block_pos.y() + 1),
                f64::from(block_pos.z()) + 0.5,
            ),
            10.0,
            mob.max_head_x_rot(),
        );
        if self.move_to.is_reached_target()
            && let Some(rabbit) = rabbit(mob)
        {
            self.raid(mob, rabbit);
        }
    }
}
