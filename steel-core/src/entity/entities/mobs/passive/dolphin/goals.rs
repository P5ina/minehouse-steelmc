//! Goals of vanilla `Dolphin`, and vanilla `DolphinJumpGoal`.

use std::f32::consts::PI;
use std::f64::consts::{PI as PI_F64, TAU};
use std::sync::Arc;

use glam::DVec3;
use steel_math::wrap_degrees;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_fluid_tags::FluidTag;
use steel_registry::vanilla_structure_tags::StructureTag;
use steel_registry::{REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_mob_effects};
use steel_utils::direction::Direction;
use steel_utils::entity_events::EntityStatus;
use steel_utils::{BlockPos, Downcast as _};

use super::DolphinEntity;
use crate::behavior::BlockStateBehaviorExt as _;
use crate::entity::ai::goal::{Goal, GoalControls, default_random_pos_towards, reduced_tick_delay};
use crate::entity::ai::path::PathComputationType;
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::objects::items::ItemEntity;
use crate::entity::{Entity, LivingEntity as _, MobEffectInstance, PathfinderMob, SharedEntity};
use crate::player::Player;
use crate::world::World;
use crate::world::structure_search::{StructureSearch, StructureSearchPoll};
use steel_registry::equipment::EquipmentSlot;

/// Vanilla `DolphinJumpGoal.STEPS_TO_CHECK`.
const JUMP_STEPS_TO_CHECK: [i32; 6] = [0, 1, 4, 5, 6, 7];
const JUMP_HORIZONTAL_PUSH: f64 = 0.6;
const JUMP_VERTICAL_PUSH: f64 = 0.7;
const JUMP_SETTLE_Y_SPEED_SQR: f64 = 0.03_f32 as f64;
const JUMP_SETTLE_X_ROT: f32 = 10.0;
const JUMP_X_ROT_LERP: f32 = 0.2;
const JUMP_MOVING_EPSILON: f64 = 1.0E-5_f32 as f64;

/// Vanilla `findNearestMapStructure` radius, in chunks.
const TREASURE_SEARCH_RADIUS: i32 = 50;
const TREASURE_MIN_AIR: i32 = 100;
const TREASURE_REACHED_DISTANCE: f64 = 4.0;
const TREASURE_SPEED_MODIFIER: f64 = 1.3;
const TREASURE_PARTICLE_INTERVAL: i32 = 80;

const SWIM_WITH_PLAYER_RANGE: f64 = 10.0;
const SWIM_WITH_PLAYER_MAX_DISTANCE_SQR: f64 = 256.0;
const SWIM_WITH_PLAYER_STOP_DISTANCE_SQR: f64 = 6.25;
const DOLPHINS_GRACE_TICKS: i32 = 100;
const DOLPHINS_GRACE_REFRESH_BOUND: i32 = 6;

const ITEM_SEARCH_INFLATE: f64 = 8.0;
const ITEM_SPEED_MODIFIER: f64 = 1.2;
const ITEM_DROP_COOLDOWN_BOUND: i32 = 100;
const ITEM_DROP_PICKUP_DELAY: i32 = 40;
const ITEM_DROP_HAND_OFFSET: f64 = 0.3_f32 as f64;
const ITEM_DROP_POWER: f32 = 0.3;
const ITEM_DROP_SPREAD: f32 = 0.02;
const ITEM_DROP_UPWARD_FACTOR: f32 = 1.5;

fn dolphin(mob: &dyn PathfinderMob) -> Option<&DolphinEntity> {
    let dolphin = mob.downcast_ref::<DolphinEntity>();
    if dolphin.is_none() {
        tracing::warn!("dolphin goal assigned to a non-dolphin entity");
    }
    dolphin
}

/// Vanilla `LookControl.setLookAt` with the dolphin's
/// `getMaxHeadYRot() + 20` and `getMaxHeadXRot()` limits.
fn look_at(mob: &dyn PathfinderMob, position: DVec3) {
    mob.mob_base().controls().lock().look_control.set_look_at(
        position,
        mob.max_head_y_rot() + 20.0,
        mob.max_head_x_rot(),
    );
}

/// Vanilla `Mth.rotLerp`.
fn rot_lerp(alpha: f32, from: f32, to: f32) -> f32 {
    from + alpha * wrap_degrees(to - from)
}

/// Vanilla `DolphinJumpGoal`: leaps out of open water now and then.
pub(super) struct DolphinJumpGoal {
    interval: i32,
    breached: bool,
}

impl DolphinJumpGoal {
    pub(super) const fn new(interval: i32) -> Self {
        Self {
            interval: reduced_tick_delay(interval),
            breached: false,
        }
    }

    fn water_is_clear(world: &World, pos: BlockPos, step_x: i32, step_z: i32, step: i32) -> bool {
        let next = pos.offset(step_x * step, 0, step_z * step);
        let state = world.get_block_state(next);
        state.get_fluid_state().fluid_id.has_tag(&FluidTag::WATER)
            && !state.get_block().has_tag(&BlockTag::BLOCKS_DOLPHIN_JUMP)
    }

    fn surface_is_clear(world: &World, pos: BlockPos, step_x: i32, step_z: i32, step: i32) -> bool {
        world
            .get_block_state(pos.offset(step_x * step, 1, step_z * step))
            .is_air()
            && world
                .get_block_state(pos.offset(step_x * step, 2, step_z * step))
                .is_air()
    }
}

impl Goal for DolphinJumpGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::JUMP
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if rand::random_range(0..self.interval) != 0 {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        let (step_x, _, step_z) = Direction::from_yaw(mob.rotation().0).offset();
        let pos = mob.block_position();
        JUMP_STEPS_TO_CHECK.iter().all(|&step| {
            Self::water_is_clear(&world, pos, step_x, step_z, step)
                && Self::surface_is_clear(&world, pos, step_x, step_z, step)
        })
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let yd = mob.velocity().y;
        let x_rot = mob.rotation().1;
        (yd * yd >= JUMP_SETTLE_Y_SPEED_SQR
            || x_rot == 0.0
            || x_rot.abs() >= JUMP_SETTLE_X_ROT
            || !mob.is_in_water())
            && !mob.on_ground()
    }

    fn is_interruptable(&self) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let (step_x, _, step_z) = Direction::from_yaw(mob.rotation().0).offset();
        mob.set_velocity(
            mob.velocity()
                + DVec3::new(
                    f64::from(step_x) * JUMP_HORIZONTAL_PUSH,
                    JUMP_VERTICAL_PUSH,
                    f64::from(step_z) * JUMP_HORIZONTAL_PUSH,
                ),
        );
        mob.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        mob.set_rotation((mob.rotation().0, 0.0));
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let already_breached = self.breached;
        if !already_breached && let Some(world) = mob.level() {
            self.breached = world
                .get_block_state(mob.block_position())
                .get_fluid_state()
                .fluid_id
                .has_tag(&FluidTag::WATER);
        }
        if self.breached && !already_breached {
            mob.play_sound(&sound_events::ENTITY_DOLPHIN_JUMP, 1.0, 1.0);
        }

        let movement = mob.velocity();
        let (y_rot, x_rot) = mob.rotation();
        if movement.y * movement.y < JUMP_SETTLE_Y_SPEED_SQR && x_rot != 0.0 {
            mob.set_rotation((y_rot, rot_lerp(JUMP_X_ROT_LERP, x_rot, 0.0)));
        } else if movement.length() > JUMP_MOVING_EPSILON {
            let horizontal = movement.x.hypot(movement.z);
            let rotation = (-movement.y).atan2(horizontal) * 180.0 / f64::from(PI);
            mob.set_rotation((y_rot, rotation as f32));
        }
    }
}

/// Vanilla `Dolphin.DolphinSwimToTreasureGoal`: after being fed, leads the
/// player toward the nearest ocean ruin or shipwreck.
///
/// Vanilla searches for the structure synchronously in `start`. Steel's
/// search waits for candidate chunks to generate, so the goal keeps running
/// while it is pending and only swims once the search reports back.
pub(super) struct DolphinSwimToTreasureGoal {
    stuck: bool,
    search: Option<StructureSearch>,
}

impl DolphinSwimToTreasureGoal {
    pub(super) const fn new() -> Self {
        Self {
            stuck: false,
            search: None,
        }
    }

    fn reached(dolphin: &DolphinEntity, treasure: BlockPos) -> bool {
        BlockPos::containing(
            f64::from(treasure.x()),
            dolphin.position().y,
            f64::from(treasure.z()),
        )
        .closer_to_center_than(dolphin.position(), TREASURE_REACHED_DISTANCE)
    }

    fn start_search(&mut self, dolphin: &DolphinEntity) {
        let Some(world) = dolphin.level() else {
            self.stuck = true;
            return;
        };
        let structures = REGISTRY
            .structures
            .get_tag(&StructureTag::DOLPHIN_LOCATED)
            .unwrap_or_default()
            .iter()
            .map(|structure| structure.key.clone())
            .collect::<Vec<_>>();
        self.search = StructureSearch::new(
            world,
            &structures,
            dolphin.block_position(),
            TREASURE_SEARCH_RADIUS,
        );
        if self.search.is_none() {
            self.stuck = true;
        }
        self.poll_search(dolphin);
    }

    /// Advances the pending search; returns whether it is still pending.
    fn poll_search(&mut self, dolphin: &DolphinEntity) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        match search.poll() {
            StructureSearchPoll::Pending => return true,
            StructureSearchPoll::Found(found) => {
                dolphin.set_treasure_pos(Some(found.pos));
                dolphin.broadcast_entity_event(EntityStatus::DolphinLookingForTreasure);
            }
            StructureSearchPoll::NotFound | StructureSearchPoll::Cancelled => self.stuck = true,
        }
        self.search = None;
        false
    }

    fn next_pos(dolphin: &DolphinEntity, world: &World, treasure: DVec3) -> Option<DVec3> {
        let mut next = default_random_pos_towards(dolphin, 16, 1, treasure, f64::from(PI / 8.0))
            .or_else(|| default_random_pos_towards(dolphin, 8, 4, treasure, f64::from(PI / 2.0)));
        if let Some(pos) = next {
            let block = BlockPos::containing(pos.x, pos.y, pos.z);
            let state = world.get_block_state(block);
            if !state.get_fluid_state().fluid_id.has_tag(&FluidTag::WATER)
                || !state.is_pathfindable(PathComputationType::Water)
            {
                next = default_random_pos_towards(dolphin, 8, 5, treasure, f64::from(PI / 2.0));
            }
        }
        next
    }
}

impl Goal for DolphinSwimToTreasureGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn is_interruptable(&self) -> bool {
        false
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        dolphin(mob)
            .is_some_and(|dolphin| dolphin.got_fish() && dolphin.air_supply() >= TREASURE_MIN_AIR)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(dolphin) = dolphin(mob) else {
            return false;
        };
        if self.stuck || dolphin.air_supply() < TREASURE_MIN_AIR {
            return false;
        }
        if self.search.is_some() {
            return true;
        }
        dolphin
            .treasure_pos()
            .is_some_and(|treasure| !Self::reached(dolphin, treasure))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(dolphin) = dolphin(mob) else {
            return;
        };
        self.stuck = false;
        mob.mob_base().navigation().lock().stop();
        self.start_search(dolphin);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(mut search) = self.search.take() {
            search.cancel();
        }
        let Some(dolphin) = dolphin(mob) else {
            return;
        };
        if dolphin
            .treasure_pos()
            .is_none_or(|treasure| Self::reached(dolphin, treasure))
            || self.stuck
        {
            dolphin.set_got_fish(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(dolphin) = dolphin(mob) else {
            return;
        };
        if self.poll_search(dolphin) {
            return;
        }
        let Some(treasure) = dolphin.treasure_pos() else {
            return;
        };
        let Some(world) = mob.level() else {
            return;
        };
        if !dolphin.close_to_next_pos() && !mob.mob_base().navigation().lock().is_done() {
            return;
        }

        let (center_x, center_y, center_z) = treasure.get_center();
        let center = DVec3::new(center_x, center_y, center_z);
        let Some(next) = Self::next_pos(dolphin, &world, center) else {
            self.stuck = true;
            return;
        };
        look_at(mob, next);
        mob.move_to_pos(next, TREASURE_SPEED_MODIFIER);
        if rand::random_range(0..reduced_tick_delay(TREASURE_PARTICLE_INTERVAL)) == 0 {
            mob.broadcast_entity_event(EntityStatus::DolphinLookingForTreasure);
        }
    }
}

/// Vanilla `Dolphin.DolphinSwimWithPlayerGoal`: swims alongside a swimming
/// player and gives them Dolphin's Grace.
pub(super) struct DolphinSwimWithPlayerGoal {
    speed_modifier: f64,
    player: Option<Arc<Player>>,
    targeting: TargetingConditions,
}

impl DolphinSwimWithPlayerGoal {
    pub(super) const fn new(speed_modifier: f64) -> Self {
        Self {
            speed_modifier,
            player: None,
            targeting: TargetingConditions::for_non_combat()
                .range(SWIM_WITH_PLAYER_RANGE)
                .ignore_line_of_sight(),
        }
    }

    fn give_grace(mob: &dyn PathfinderMob, player: &Player) {
        player.add_mob_effect_with_source(
            MobEffectInstance::with_duration(
                vanilla_mob_effects::DOLPHINS_GRACE,
                DOLPHINS_GRACE_TICKS,
                0,
            ),
            Some(mob.as_entity_event_source()),
        );
    }
}

impl Goal for DolphinSwimWithPlayerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        self.player = world.nearest_player(mob.position(), -1.0, |player| {
            self.targeting.test(&world, Some(mob), player)
        });
        self.player.as_ref().is_some_and(|player| {
            player.is_swimming() && mob.target().is_none_or(|target| target.id() != player.id())
        })
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.player.as_ref().is_some_and(|player| {
            player.is_swimming()
                && mob.position().distance_squared(player.position())
                    < SWIM_WITH_PLAYER_MAX_DISTANCE_SQR
        })
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(player) = &self.player {
            Self::give_grace(mob, player);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.player = None;
        mob.mob_base().navigation().lock().stop();
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(player) = self.player.clone() else {
            return;
        };
        let position = player.position();
        look_at(mob, DVec3::new(position.x, player.get_eye_y(), position.z));
        if mob.position().distance_squared(position) < SWIM_WITH_PLAYER_STOP_DISTANCE_SQR {
            mob.mob_base().navigation().lock().stop();
        } else {
            mob.move_to_entity(player.as_ref(), self.speed_modifier);
        }

        if player.is_swimming() && rand::random_range(0..DOLPHINS_GRACE_REFRESH_BOUND) == 0 {
            Self::give_grace(mob, &player);
        }
    }
}

/// Vanilla `Dolphin.ALLOWED_ITEMS`: settled items floating in water.
fn allowed_items(mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
    let Some(world) = mob.level() else {
        return Vec::new();
    };
    let area = mob.bounding_box().inflate(ITEM_SEARCH_INFLATE);
    world.get_entities_in_aabb_matching(&area, |entity| {
        entity.downcast_ref::<ItemEntity>().is_some_and(|item| {
            !item.has_pickup_delay() && !item.is_removed() && item.is_in_water()
        })
    })
}

/// Vanilla `Dolphin.ItemGoal.dropItem`: tosses the held item forward.
fn drop_held_item(mob: &dyn PathfinderMob) -> bool {
    let held = mob.equipment_in_slot(EquipmentSlot::MainHand);
    if held.is_empty() {
        return false;
    }
    let Some(world) = mob.level() else {
        return false;
    };

    let (y_rot, x_rot) = mob.rotation();
    let y_rot = y_rot * (PI_F64 / 180.0) as f32;
    let x_rot = x_rot * (PI_F64 / 180.0) as f32;
    let dir = rand::random::<f32>() * (TAU as f32);
    let spread = ITEM_DROP_SPREAD * rand::random::<f32>();
    let velocity = DVec3::new(
        f64::from(ITEM_DROP_POWER * -y_rot.sin() * x_rot.cos() + dir.cos() * spread),
        f64::from(ITEM_DROP_POWER * x_rot.sin() * ITEM_DROP_UPWARD_FACTOR),
        f64::from(ITEM_DROP_POWER * y_rot.cos() * x_rot.cos() + dir.sin() * spread),
    );
    let position = mob.position();
    let hand = DVec3::new(
        position.x,
        mob.get_eye_y() - ITEM_DROP_HAND_OFFSET,
        position.z,
    );
    if let Some(item) = world.spawn_item_with_velocity(hand, held, velocity) {
        item.set_pickup_delay(ITEM_DROP_PICKUP_DELAY);
        item.set_thrower(mob.uuid());
    }
    mob.living_base()
        .equipment()
        .lock()
        .set(EquipmentSlot::MainHand, ItemStack::empty());
    true
}

/// Vanilla `Dolphin.MoveToItemGoal`: swims to items floating nearby.
pub(super) struct MoveToItemGoal {
    cooldown: i32,
}

impl MoveToItemGoal {
    pub(super) const fn new() -> Self {
        Self { cooldown: 0 }
    }
}

impl Goal for MoveToItemGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.cooldown <= mob.tick_count() && !allowed_items(mob).is_empty()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(item) = allowed_items(mob).first() {
            mob.move_to_entity(item.as_ref(), ITEM_SPEED_MODIFIER);
            mob.play_sound(&sound_events::ENTITY_DOLPHIN_PLAY, 1.0, 1.0);
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if drop_held_item(mob) {
            self.cooldown = mob.tick_count() + rand::random_range(0..ITEM_DROP_COOLDOWN_BOUND);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(item) = allowed_items(mob).first() {
            mob.move_to_entity(item.as_ref(), ITEM_SPEED_MODIFIER);
        }
    }
}

/// Vanilla `Dolphin.PlayWithItemsGoal`: keeps tossing the held item.
pub(super) struct PlayWithItemsGoal;

impl Goal for PlayWithItemsGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !mob.equipment_in_slot(EquipmentSlot::MainHand).is_empty()
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        drop_held_item(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        drop_held_item(mob);
    }
}

/// Vanilla `FollowPlayerRiddenEntityGoal` filter for `AbstractBoat.class`.
pub(super) fn is_boat(entity: &dyn Entity) -> bool {
    entity.entity_type().is_abstract_boat
}

/// Vanilla `FollowPlayerRiddenEntityGoal` filter for `AbstractNautilus.class`.
pub(super) fn is_nautilus(entity: &dyn Entity) -> bool {
    entity.entity_type().is_abstract_nautilus
}
