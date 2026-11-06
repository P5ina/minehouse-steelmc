//! The cat's own goals: vanilla `Cat.CatRelaxOnOwnerGoal`, `Cat.CatTemptGoal`,
//! `Cat.CatAvoidEntityGoal`, `CatLieOnBlockGoal` and `CatSitOnBlockGoal`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BedPart, BlockStateProperties};
use steel_registry::loot_table::LootContext;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::{vanilla_blocks, vanilla_loot_tables};
use steel_utils::{BlockPos, Downcast as _, WorldAabb};
use uuid::Uuid;

use super::{CatEntity, is_cat_food};
use crate::entity::ai::goal::{
    AvoidEntityGoal, Goal, GoalControls, MoveToBlockGoal, TemptGoal, reduced_tick_delay,
};
use crate::entity::entity_selector;
use crate::entity::leash::Leashable;
use crate::entity::{
    Entity, LivingEntity, Mob, PathfinderMob, SharedEntity, TamableAnimal, entity_loot_ref,
    living_loot_equipment,
};
use crate::world::World;

use super::super::ocelot::{SPRINT_SPEED_MOD, WALK_SPEED_MOD};

/// Vanilla `Cat.TEMPT_SPEED_MOD`.
pub(super) const TEMPT_SPEED_MOD: f64 = 0.6;
const AVOID_PLAYER_DISTANCE: f32 = 16.0;
/// The owner must sleep within 10 blocks for the cat to join them.
const RELAX_OWNER_DISTANCE_SQR: f64 = 100.0;
const RELAX_SPEED: f64 = 1.1;
const RELAX_ON_BED_DISTANCE_SQR: f64 = 2.5;
const RELAX_LIE_DOWN_TICKS: i32 = 16;
const RELAX_OCCUPIED_RANGE: f64 = 2.0;
/// Vanilla gives a gift only to owners who slept long enough to wake normally.
const GIFT_MIN_SLEEP_TICKS: i32 = 100;
const SELECT_PLAYER_TICKS: i32 = 600;
const FORGET_PLAYER_TICKS: i32 = 500;
const LIE_ON_BLOCK_SEARCH_RANGE: i32 = 8;
const LIE_ON_BLOCK_VERTICAL_RANGE: i32 = 6;
const LIE_ON_BLOCK_VERTICAL_START: i32 = -2;
/// Vanilla `CatLieOnBlockGoal.nextStartTick`.
const LIE_ON_BLOCK_NEXT_START_TICK: i32 = 40;
const SIT_ON_BLOCK_SEARCH_RANGE: i32 = 8;

fn as_cat(mob: &dyn PathfinderMob) -> Option<&CatEntity> {
    mob.downcast_ref::<CatEntity>()
}

fn is_tame_cat(mob: &dyn PathfinderMob) -> bool {
    as_cat(mob).is_some_and(TamableAnimal::is_tame)
}

/// Vanilla `Cat.CatRelaxOnOwnerGoal`: a tame cat curls up on its sleeping
/// owner's bed and may leave a gift when the owner wakes.
pub(super) struct CatRelaxOnOwnerGoal {
    owner: Option<SharedEntity>,
    goal_pos: Option<BlockPos>,
    on_bed_ticks: i32,
}

impl CatRelaxOnOwnerGoal {
    pub(super) const fn new() -> Self {
        Self {
            owner: None,
            goal_pos: None,
            on_bed_ticks: 0,
        }
    }

    fn owner_is_sleeping(&self) -> bool {
        self.owner
            .as_ref()
            .and_then(|owner| owner.as_player())
            .is_some_and(LivingEntity::is_sleeping)
    }

    /// Vanilla `spaceIsOccupied`: another cat already lies on the bed.
    fn space_is_occupied(cat: &CatEntity, world: &World, goal_pos: BlockPos) -> bool {
        let area = WorldAabb::new(
            f64::from(goal_pos.x()),
            f64::from(goal_pos.y()),
            f64::from(goal_pos.z()),
            f64::from(goal_pos.x() + 1),
            f64::from(goal_pos.y() + 1),
            f64::from(goal_pos.z() + 1),
        )
        .inflate(RELAX_OCCUPIED_RANGE);
        let cat_uuid = cat.uuid();
        !world
            .get_entities_in_aabb_matching(&area, |entity| {
                entity.uuid() != cat_uuid
                    && entity
                        .downcast_ref::<CatEntity>()
                        .is_some_and(|other| other.is_lying() || other.is_relax_state_one())
            })
            .is_empty()
    }

    /// Vanilla `giveMorningGift`: hops to a random spot nearby and drops a roll
    /// of `gameplay/cat_morning_gift` in front of itself.
    fn give_morning_gift(cat: &CatEntity, world: &Arc<World>) {
        let origin = cat
            .as_leashable()
            .and_then(Leashable::leash_holder)
            .map_or_else(|| cat.block_position(), |holder| holder.block_position());
        let _ = cat.random_teleport_avoiding(
            world,
            f64::from(origin.x() + rand::random_range(0..11) - 5),
            f64::from(origin.y() + rand::random_range(0..5) - 2),
            f64::from(origin.z() + rand::random_range(0..11) - 5),
            false,
            &BlockTag::CAT_DOES_NOT_TELEPORT_TO,
        );

        let cat_pos = cat.block_position();
        let position = cat.position();
        let mut rng = rand::rng();
        let equipment = living_loot_equipment(cat);
        // Vanilla `LootContextParamSets.GIFT` carries only ORIGIN and THIS_ENTITY.
        let mut context = LootContext::new(&mut rng)
            .with_origin(position.x, position.y, position.z)
            .with_this_entity(entity_loot_ref(cat, Some(&equipment)));
        let items = vanilla_loot_tables::GAMEPLAY_CAT_MORNING_GIFT.get_random_items(&mut context);
        let body_rot = cat.y_body_rot().to_radians();
        let drop_at = DVec3::new(
            f64::from(cat_pos.x()) - f64::from(body_rot.sin()),
            f64::from(cat_pos.y()),
            f64::from(cat_pos.z()) + f64::from(body_rot.cos()),
        );
        for item in items {
            let _ = world.spawn_item(drop_at, item);
        }
    }
}

impl Goal for CatRelaxOnOwnerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(cat) = as_cat(mob) else {
            return false;
        };
        if !cat.is_tame() || cat.is_ordered_to_sit() {
            return false;
        }
        let Some(owner) = cat.owner() else {
            return false;
        };
        if owner.as_player().is_none() {
            return false;
        }
        self.owner = Some(Arc::clone(&owner));
        if !self.owner_is_sleeping() {
            return false;
        }
        if cat.position().distance_squared(owner.position()) > RELAX_OWNER_DISTANCE_SQR {
            return false;
        }
        let Some(world) = cat.level() else {
            return false;
        };
        let owner_pos = owner.block_position();
        let owner_state = world.get_block_state(owner_pos);
        if !owner_state.get_block().has_tag(&BlockTag::CATS_CAN_LIE_ON) {
            return false;
        }
        let goal_pos = owner_state
            .try_get_value(&BlockStateProperties::HORIZONTAL_FACING)
            .map_or(owner_pos, |bed_dir| owner_pos.relative(bed_dir.opposite()));
        self.goal_pos = Some(goal_pos);
        !Self::space_is_occupied(cat, &world, goal_pos)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(cat) = as_cat(mob) else {
            return false;
        };
        let Some(goal_pos) = self.goal_pos else {
            return false;
        };
        cat.is_tame()
            && !cat.is_ordered_to_sit()
            && self.owner_is_sleeping()
            && cat
                .level()
                .is_some_and(|world| !Self::space_is_occupied(cat, &world, goal_pos))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(cat) = as_cat(mob) else {
            return;
        };
        if let Some(goal_pos) = self.goal_pos {
            cat.set_in_sitting_pose(false);
            let _ = cat.move_to_pos(block_corner(goal_pos), RELAX_SPEED);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(cat) = as_cat(mob) else {
            return;
        };
        cat.set_lying(false);
        let slept_long_enough = self
            .owner
            .as_ref()
            .and_then(|owner| owner.as_player())
            .is_some_and(|player| player.sleep_counter() >= GIFT_MIN_SLEEP_TICKS);
        if slept_long_enough
            && let Some(world) = cat.level()
            && rand::random::<f32>() < world.cat_waking_up_gift_chance()
        {
            Self::give_morning_gift(cat, &world);
        }
        self.on_bed_ticks = 0;
        cat.set_relax_state_one(false);
        cat.mob_base().navigation().lock().stop();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(cat) = as_cat(mob) else {
            return;
        };
        let (Some(owner), Some(goal_pos)) = (self.owner.as_ref(), self.goal_pos) else {
            return;
        };
        cat.set_in_sitting_pose(false);
        let _ = cat.move_to_pos(block_corner(goal_pos), RELAX_SPEED);
        if cat.position().distance_squared(owner.position()) < RELAX_ON_BED_DISTANCE_SQR {
            self.on_bed_ticks += 1;
            if self.on_bed_ticks > reduced_tick_delay(RELAX_LIE_DOWN_TICKS) {
                cat.set_lying(true);
                cat.set_relax_state_one(false);
            } else {
                cat.mob_look_at(owner.as_ref(), 45.0, 45.0);
                cat.set_relax_state_one(true);
            }
        } else {
            cat.set_lying(false);
        }
    }
}

/// Vanilla `navigation.moveTo(pos.getX(), pos.getY(), pos.getZ(), ...)`.
fn block_corner(pos: BlockPos) -> DVec3 {
    DVec3::new(f64::from(pos.x()), f64::from(pos.y()), f64::from(pos.z()))
}

/// Vanilla `Cat.CatTemptGoal`: wild cats follow fish, but now and then pick
/// one player who may move around them without scaring them off.
pub(super) struct CatTemptGoal {
    tempt: TemptGoal,
    selected_player: Option<Uuid>,
}

impl CatTemptGoal {
    pub(super) fn new(running: Arc<AtomicBool>) -> Self {
        Self {
            tempt: TemptGoal::new(TEMPT_SPEED_MOD, is_cat_food, true).with_running_flag(running),
            selected_player: None,
        }
    }

    const fn select_player(&mut self, player: Option<Uuid>) {
        self.selected_player = player;
        self.tempt.set_scare_exempt_player(player);
    }
}

impl Goal for CatTemptGoal {
    fn controls(&self) -> GoalControls {
        self.tempt.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.tempt.can_use(mob) && !is_tame_cat(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.tempt.can_continue_to_use(mob) && !is_tame_cat(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.tempt.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.tempt.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.tempt.tick(mob, entity);
        if self.selected_player.is_none()
            && rand::random_range(0..reduced_tick_delay(SELECT_PLAYER_TICKS)) == 0
        {
            let player = self.tempt.player().map(|player| player.uuid());
            self.select_player(player);
        } else if rand::random_range(0..reduced_tick_delay(FORGET_PLAYER_TICKS)) == 0 {
            self.select_player(None);
        }
    }
}

/// Vanilla `Cat.CatAvoidEntityGoal<Player>`: wild cats run from players.
pub(super) struct CatAvoidPlayersGoal {
    avoid: AvoidEntityGoal,
}

impl CatAvoidPlayersGoal {
    pub(super) fn new() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(
                AVOID_PLAYER_DISTANCE,
                WALK_SPEED_MOD,
                SPRINT_SPEED_MOD,
                |target, _| {
                    target.as_player().is_some()
                        && entity_selector::no_creative_or_spectator(
                            target.as_entity_event_source(),
                        )
                },
            ),
        }
    }
}

impl Goal for CatAvoidPlayersGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !is_tame_cat(mob) && self.avoid.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !is_tame_cat(mob) && self.avoid.can_continue_to_use(mob)
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

/// Vanilla `CatLieOnBlockGoal`: a tame cat lies down on beds and warm blocks.
pub(super) struct CatLieOnBlockGoal {
    move_to: MoveToBlockGoal,
}

impl CatLieOnBlockGoal {
    pub(super) fn new(speed_modifier: f64) -> Self {
        Self {
            move_to: MoveToBlockGoal::with_vertical_search_range(
                speed_modifier,
                LIE_ON_BLOCK_SEARCH_RANGE,
                LIE_ON_BLOCK_VERTICAL_RANGE,
                |world, pos| {
                    world.get_block_state(pos.above()).is_air()
                        && world
                            .get_block_state(pos)
                            .get_block()
                            .has_tag(&BlockTag::CATS_CAN_LIE_ON)
                },
            )
            .with_vertical_search_start(LIE_ON_BLOCK_VERTICAL_START)
            .with_fixed_next_start_tick(LIE_ON_BLOCK_NEXT_START_TICK),
        }
    }
}

impl Goal for CatLieOnBlockGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::JUMP | GoalControls::MOVE
    }

    fn requires_update_every_tick(&self) -> bool {
        self.move_to.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        as_cat(mob).is_some_and(|cat| cat.is_tame() && !cat.is_ordered_to_sit() && !cat.is_lying())
            && self.move_to.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.move_to.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.start(mob);
        if let Some(cat) = as_cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.stop(mob);
        if let Some(cat) = as_cat(mob) {
            cat.set_lying(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.move_to.tick(mob, entity);
        let Some(cat) = as_cat(mob) else {
            return;
        };
        cat.set_in_sitting_pose(false);
        if !self.move_to.is_reached_target() {
            cat.set_lying(false);
        } else if !cat.is_lying() {
            cat.set_lying(true);
        }
    }
}

/// Vanilla `CatSitOnBlockGoal.isValidTarget`.
pub(super) fn is_sit_target(world: &World, pos: BlockPos) -> bool {
    if !world.get_block_state(pos.above()).is_air() {
        return false;
    }
    let state = world.get_block_state(pos);
    let block = state.get_block();
    if !block.has_tag(&BlockTag::CATS_CAN_SIT_ON) {
        return false;
    }
    if block == &vanilla_blocks::CHEST {
        // Vanilla `ChestBlockEntity.getOpenCount(level, pos) < 1`. Steel does
        // not count chest viewers yet (see `ChestBlock::use_without_item`),
        // so every chest has no viewers.
        return true;
    }
    if block == &vanilla_blocks::FURNACE {
        return state.get_value(&BlockStateProperties::LIT);
    }
    if block.has_tag(&BlockTag::BEDS) {
        return state
            .try_get_value(&BlockStateProperties::BED_PART)
            .is_none_or(|part| part != BedPart::Head);
    }
    true
}

/// Vanilla `CatSitOnBlockGoal`: a tame cat sits on chests, lit furnaces and
/// the foot of beds.
pub(super) struct CatSitOnBlockGoal {
    move_to: MoveToBlockGoal,
}

impl CatSitOnBlockGoal {
    pub(super) fn new(speed_modifier: f64) -> Self {
        Self {
            move_to: MoveToBlockGoal::new(speed_modifier, SIT_ON_BLOCK_SEARCH_RANGE, is_sit_target),
        }
    }
}

impl Goal for CatSitOnBlockGoal {
    fn controls(&self) -> GoalControls {
        self.move_to.controls()
    }

    fn requires_update_every_tick(&self) -> bool {
        self.move_to.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        as_cat(mob).is_some_and(|cat| cat.is_tame() && !cat.is_ordered_to_sit())
            && self.move_to.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.move_to.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.start(mob);
        if let Some(cat) = as_cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.stop(mob);
        if let Some(cat) = as_cat(mob) {
            cat.set_in_sitting_pose(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.move_to.tick(mob, entity);
        if let Some(cat) = as_cat(mob) {
            cat.set_in_sitting_pose(self.move_to.is_reached_target());
        }
    }
}
