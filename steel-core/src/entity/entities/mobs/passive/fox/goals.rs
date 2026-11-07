//! The fox's own goals, vanilla `Fox`'s inner goal classes.

use std::f64::consts::{PI, TAU};

use glam::DVec3;
use steel_math::wrap_degrees;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{
    sound_events, vanilla_blocks, vanilla_entities, vanilla_game_events, vanilla_items,
};
use steel_utils::types::{GameType, UpdateFlags};
use steel_utils::{BlockPos, BlockStateId, Downcast as _};

use super::{FoxEntity, is_allowed_item, is_path_clear, is_stalkable_prey};
use crate::behavior::blocks::CaveVinesBlock;
use crate::entity::ai::goal::{
    AvoidEntityGoal, BreedGoal, FleeSunGoal, FloatGoal, FollowParentGoal, Goal, GoalControls,
    LookAtPlayerGoal, MeleeAttackGoal, MoveToBlockGoal, NearestAttackableTargetGoal, PanicGoal,
    StrollThroughVillageGoal, reduced_tick_delay,
};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::ItemEntity;
use crate::entity::entity_selector;
use crate::entity::{Entity, LivingEntity, MobBase, PathfinderMob, SharedEntity, TamableAnimal};
use crate::world::World;
use crate::world::game_event::GameEventContext;

const ALERTABLE_RANGE: f64 = 12.0;
const ALERTABLE_HEIGHT: f64 = 6.0;
const ITEM_SEARCH_RANGE: f64 = 8.0;
const ITEM_SPEED: f64 = 1.2;
const STALK_DISTANCE_SQR: f64 = 36.0;
const STALK_SPEED: f64 = 1.5;
const POUNCE_HIT_DISTANCE: f64 = 2.0;
const FACEPLANT_TICKS: i32 = 40;
const BERRY_WAIT_TICKS: i32 = 40;
const SHELTER_INTERVAL: i32 = 100;
const TRUSTED_HURT_MEMORY: i32 = 600;
const FOX_FLOAT_DEPTH: f64 = 0.25;

fn fox(mob: &dyn PathfinderMob) -> Option<&FoxEntity> {
    mob.downcast_ref::<FoxEntity>()
}

fn fox_of(entity: &dyn LivingEntity) -> Option<&FoxEntity> {
    entity.as_entity_event_source().downcast_ref::<FoxEntity>()
}

fn chance(bound: i32) -> bool {
    rand::random_range(0..bound.max(1)) == 0
}

fn is_creative(entity: &dyn Entity) -> bool {
    entity
        .as_player()
        .is_some_and(|player| player.game_mode() == GameType::Creative)
}

/// Vanilla `Fox.FoxAlertableEntitiesSelector`.
fn is_alertable(fox: &FoxEntity, target: &dyn LivingEntity) -> bool {
    let entity = target.as_entity_event_source();
    if entity.downcast_ref::<FoxEntity>().is_some() {
        return false;
    }
    if is_stalkable_prey(entity) || target.is_monster() {
        return true;
    }
    if let Some(tamable) = target.as_tamable_animal() {
        return !tamable.is_tame();
    }
    if entity.as_player().is_some() && (target.is_spectator() || is_creative(entity)) {
        return false;
    }
    !fox.trusts(entity) && !target.is_sleeping() && !target.is_discrete()
}

/// Vanilla `FoxBehaviorGoal.hasShelter`.
fn has_shelter(fox: &FoxEntity) -> bool {
    let Some(world) = fox.level() else {
        return false;
    };
    let position = fox.position();
    let pos = BlockPos::containing(position.x, fox.bounding_box().max_y(), position.z);
    !world.can_see_sky(pos) && fox.get_walk_target_value(pos) >= 0.0
}

/// Vanilla `FoxBehaviorGoal.alertable`.
fn alertable(fox: &FoxEntity) -> bool {
    let Some(world) = fox.level() else {
        return false;
    };
    let conditions = TargetingConditions::for_combat()
        .range(ALERTABLE_RANGE)
        .ignore_line_of_sight()
        .targeter_selector(|target, targeter, _| {
            fox_of(targeter).is_some_and(|fox| is_alertable(fox, target))
        });
    let area = fox
        .bounding_box()
        .inflate_xyz(ALERTABLE_RANGE, ALERTABLE_HEIGHT, ALERTABLE_RANGE);
    world.has_entity_in_aabb_matching(&area, |entity| {
        entity.uuid() != fox.uuid()
            && entity
                .as_living_entity()
                .is_some_and(|target| conditions.test(world.as_ref(), Some(fox), target))
    })
}

/// Vanilla `Fox.FoxFloatGoal`.
pub(super) struct FoxFloatGoal {
    float: FloatGoal,
}

impl FoxFloatGoal {
    pub(super) fn new(mob_base: &MobBase) -> Self {
        Self {
            float: FloatGoal::new(mob_base),
        }
    }
}

impl Goal for FoxFloatGoal {
    fn controls(&self) -> GoalControls {
        self.float.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        mob.is_in_water() && mob.fluid_contact().water_height() > FOX_FLOAT_DEPTH
            || mob.is_in_lava()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.float.start(mob);
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
    }

    fn requires_update_every_tick(&self) -> bool {
        self.float.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.float.tick(mob, entity);
    }
}

/// Vanilla `Fox.FaceplantGoal`: lies still for two seconds after a pounce into
/// the snow.
pub(super) struct FaceplantGoal {
    countdown: i32,
}

impl FaceplantGoal {
    pub(super) const fn new() -> Self {
        Self { countdown: 0 }
    }
}

impl Goal for FaceplantGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::LOOK | GoalControls::JUMP | GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        fox(mob).is_some_and(FoxEntity::is_faceplanted)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.can_use(mob) && self.countdown > 0
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.countdown = reduced_tick_delay(FACEPLANT_TICKS);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.set_faceplanted(false);
        }
    }

    fn tick(&mut self, _mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.countdown -= 1;
    }
}

/// Vanilla `Fox.FoxPanicGoal`: a defending fox does not panic.
pub(super) struct FoxPanicGoal {
    panic: PanicGoal,
}

impl FoxPanicGoal {
    pub(super) const fn new() -> Self {
        Self {
            panic: PanicGoal::new(2.2),
        }
    }
}

impl Goal for FoxPanicGoal {
    fn controls(&self) -> GoalControls {
        self.panic.controls()
    }

    fn is_panic_goal(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !fox(mob).is_some_and(FoxEntity::is_defending) && self.panic.can_use(mob)
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
    }
}

/// Vanilla `Fox.FoxBreedGoal`: both parents stop what they do. The cub's
/// trust is set by [`FoxEntity::on_bred_offspring`].
pub(super) struct FoxBreedGoal {
    breed: BreedGoal,
}

impl FoxBreedGoal {
    pub(super) const fn new() -> Self {
        Self {
            breed: BreedGoal::new(1.0),
        }
    }
}

impl Goal for FoxBreedGoal {
    fn controls(&self) -> GoalControls {
        self.breed.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.breed.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.breed.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
        if let Some(partner) = self
            .breed
            .partner()
            .and_then(|partner| partner.downcast_ref::<FoxEntity>())
        {
            partner.clear_states();
        }
        self.breed.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.breed.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.breed.tick(mob, entity);
    }
}

/// Vanilla fox `AvoidEntityGoal`s: players it does not trust, wild wolves and
/// polar bears, unless it is defending.
pub(super) struct FoxAvoidGoal {
    avoid: AvoidEntityGoal,
}

impl FoxAvoidGoal {
    fn not_defending(_target: &dyn LivingEntity, fox: &dyn LivingEntity, _world: &World) -> bool {
        fox_of(fox).is_some_and(|fox| !fox.is_defending())
    }

    pub(super) fn players() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(16.0, 1.6, 1.4, |target, _| {
                let entity = target.as_entity_event_source();
                entity.as_player().is_some()
                    && !entity.is_discrete()
                    && entity_selector::no_creative_or_spectator(entity)
            })
            .with_targeter_selector(|target, fox, world| {
                Self::not_defending(target, fox, world)
                    && fox_of(fox).is_some_and(|fox| !fox.trusts(target.as_entity_event_source()))
            }),
        }
    }

    pub(super) fn wolves() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(8.0, 1.6, 1.4, |target, _| {
                target.entity_type() == &vanilla_entities::WOLF
                    && !target
                        .as_tamable_animal()
                        .is_some_and(TamableAnimal::is_tame)
            })
            .with_targeter_selector(Self::not_defending),
        }
    }

    pub(super) fn polar_bears() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(8.0, 1.6, 1.4, |target, _| {
                target.entity_type() == &vanilla_entities::POLAR_BEAR
            })
            .with_targeter_selector(Self::not_defending),
        }
    }
}

impl Goal for FoxAvoidGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.avoid.can_use(mob)
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

/// Vanilla `Fox.StalkPreyGoal`: sneaks up on chickens and rabbits until it is
/// close enough to crouch.
pub(super) struct StalkPreyGoal;

impl Goal for StalkPreyGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        if fox.is_sleeping() {
            return false;
        }
        let Some(target) = mob.target() else {
            return false;
        };
        target.is_alive()
            && is_stalkable_prey(target.as_ref())
            && mob.position().distance_squared(target.position()) > STALK_DISTANCE_SQR
            && !fox.is_crouching()
            && !fox.is_interested()
            && !mob.living_base().is_jumping()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.set_sitting(false);
            fox.set_faceplanted(false);
        }
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(fox) = fox(mob) else {
            return;
        };
        match mob.target() {
            Some(target) if is_path_clear(fox, target.as_ref()) => {
                fox.set_is_interested(true);
                fox.set_is_crouching(true);
                mob.mob_base().navigation().lock().stop();
                look_at_entity(
                    mob,
                    target.as_ref(),
                    mob.max_head_y_rot(),
                    mob.max_head_x_rot(),
                );
            }
            _ => {
                fox.set_is_interested(false);
                fox.set_is_crouching(false);
            }
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let (Some(fox), Some(target)) = (fox(mob), mob.target()) else {
            return;
        };
        look_at_entity(
            mob,
            target.as_ref(),
            mob.max_head_y_rot(),
            mob.max_head_x_rot(),
        );
        if mob.position().distance_squared(target.position()) <= STALK_DISTANCE_SQR {
            fox.set_is_interested(true);
            fox.set_is_crouching(true);
            mob.mob_base().navigation().lock().stop();
        } else {
            mob.move_to_entity(target.as_ref(), STALK_SPEED);
        }
    }
}

fn look_at_entity(mob: &dyn PathfinderMob, target: &dyn Entity, y_max: f32, x_max: f32) {
    let position = target.position();
    mob.mob_base().controls().lock().look_control.set_look_at(
        DVec3::new(position.x, target.get_eye_y(), position.z),
        y_max,
        x_max,
    );
}

/// Vanilla `Mth.rotLerp`.
fn rot_lerp(alpha: f32, from: f32, to: f32) -> f32 {
    from + alpha * wrap_degrees(to - from)
}

/// Vanilla `Fox.FoxPounceGoal`: the leap from a full crouch onto the prey.
pub(super) struct FoxPounceGoal;

impl Goal for FoxPounceGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::JUMP
    }

    fn is_interruptable(&self) -> bool {
        false
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        if !fox.is_fully_crouched() {
            return false;
        }
        let Some(target) = mob.target() else {
            return false;
        };
        if !target.is_alive() {
            return false;
        }
        let clear = is_path_clear(fox, target.as_ref());
        if !clear {
            let _ = mob.create_path_to(target.block_position(), 0);
            fox.set_is_crouching(false);
            fox.set_is_interested(false);
        }
        clear
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        if !mob.target().is_some_and(|target| target.is_alive()) {
            return false;
        }
        let yd = mob.velocity().y;
        (yd * yd >= 0.05 || mob.rotation().1.abs() >= 15.0 || !mob.on_ground())
            && !fox.is_faceplanted()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(fox) = fox(mob) else {
            return;
        };
        mob.set_jumping(true);
        fox.set_is_pouncing(true);
        fox.set_is_interested(false);
        if let Some(target) = mob.target() {
            look_at_entity(mob, target.as_ref(), 60.0, 30.0);
            let direction = (target.position() - mob.position()).normalize_or_zero();
            mob.set_velocity(
                mob.velocity() + DVec3::new(direction.x * 0.8, 0.9, direction.z * 0.8),
            );
        }
        mob.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.set_is_crouching(false);
            fox.reset_crouch_amount();
            fox.set_is_interested(false);
            fox.set_is_pouncing(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(fox) = fox(mob) else {
            return;
        };
        let target = mob.target();
        if let Some(target) = &target {
            look_at_entity(mob, target.as_ref(), 60.0, 30.0);
        }

        if !fox.is_faceplanted() {
            let movement = mob.velocity();
            let (y_rot, x_rot) = mob.rotation();
            if movement.y * movement.y < 0.03 && x_rot != 0.0 {
                mob.set_rotation((y_rot, rot_lerp(0.2, x_rot, 0.0)));
            } else {
                let horizontal = movement.x.hypot(movement.z);
                let upwards_bias = if mob.living_base().is_jumping() && movement.y > 0.0 {
                    6.5
                } else {
                    1.0
                };
                let biased_y = movement.y * upwards_bias;
                let len = horizontal.hypot(biased_y);
                if len > 1.0e-5 {
                    let rotation = (-biased_y).signum() * (horizontal / len).acos() * 180.0 / PI;
                    mob.set_rotation((y_rot, rotation as f32));
                }
            }
        }

        if let Some(target) = &target
            && mob.position().distance(target.position()) <= POUNCE_HIT_DISTANCE
        {
            if let Some(world) = mob.level() {
                let _ = mob.do_hurt_target(entity, &world, target);
            }
        } else if mob.rotation().1 > 0.0
            && mob.on_ground()
            && mob.velocity().y as f32 != 0.0
            && mob.level().is_some_and(|world| {
                world.get_block_state(mob.block_position()).get_block() == &vanilla_blocks::SNOW
            })
        {
            let (y_rot, _) = mob.rotation();
            mob.set_rotation((y_rot, 60.0));
            let _ = mob.set_target(None);
            fox.set_faceplanted(true);
        }
    }
}

/// Vanilla `Fox.SeekShelterGoal`: hides from storms and from the midday sun
/// outside villages.
pub(super) struct SeekShelterGoal {
    flee: FleeSunGoal,
    interval: i32,
}

impl SeekShelterGoal {
    pub(super) const fn new() -> Self {
        Self {
            flee: FleeSunGoal::new(1.25),
            interval: reduced_tick_delay(SHELTER_INTERVAL),
        }
    }
}

impl Goal for SeekShelterGoal {
    fn controls(&self) -> GoalControls {
        self.flee.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        if fox.is_sleeping() || mob.target().is_some() {
            return false;
        }
        let Some(world) = mob.level() else {
            return false;
        };
        let pos = mob.block_position();
        if world.is_thundering() && world.can_see_sky(pos) {
            return self.flee.set_wanted_pos(mob, &world);
        }
        if self.interval > 0 {
            self.interval -= 1;
            return false;
        }
        self.interval = SHELTER_INTERVAL;
        world.is_bright_outside()
            && world.can_see_sky(pos)
            && !world.is_village(pos)
            && self.flee.set_wanted_pos(mob, &world)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.flee.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
        self.flee.start(mob);
    }
}

/// Vanilla `Fox.FoxMeleeAttackGoal`: bites without swinging.
pub(super) struct FoxMeleeAttackGoal {
    attack: MeleeAttackGoal,
}

impl FoxMeleeAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            attack: MeleeAttackGoal::new(1.2, true),
        }
    }
}

impl Goal for FoxMeleeAttackGoal {
    fn controls(&self) -> GoalControls {
        self.attack.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        !fox.is_sitting()
            && !fox.is_sleeping()
            && !fox.is_crouching()
            && !fox.is_faceplanted()
            && self.attack.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.set_is_interested(false);
        }
        self.attack.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.attack.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.attack.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        let Some(target) = mob.target() else {
            return;
        };
        self.attack.tick_pursuit(mob, &target);
        let Some(target_living) = target.as_living_entity() else {
            return;
        };
        if self.attack.can_perform_attack(mob, target_living) {
            self.attack.reset_attack_cooldown();
            if let Some(world) = mob.level() {
                let _ = mob.do_hurt_target(entity, &world, &target);
            }
            mob.play_sound(&sound_events::ENTITY_FOX_BITE, 1.0, 1.0);
        }
    }
}

/// Vanilla `Fox.SleepGoal`: naps in the shade through the day when nothing
/// worrying is around.
pub(super) struct SleepGoal {
    countdown: i32,
}

const fn wait_time_before_sleep() -> i32 {
    reduced_tick_delay(140)
}

impl SleepGoal {
    pub(super) fn new() -> Self {
        Self {
            countdown: rand::random_range(0..wait_time_before_sleep()),
        }
    }

    fn can_sleep(&mut self, fox: &FoxEntity) -> bool {
        if self.countdown > 0 {
            self.countdown -= 1;
            return false;
        }
        fox.level().is_some_and(|world| world.is_bright_outside())
            && has_shelter(fox)
            && !alertable(fox)
            && !fox.base().is_in_powder_snow()
    }
}

impl Goal for SleepGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK | GoalControls::JUMP
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        let input = mob.travel_input();
        if input.sideways() == 0.0 && input.vertical() == 0.0 && input.forward() == 0.0 {
            self.can_sleep(fox) || fox.is_sleeping()
        } else {
            false
        }
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        fox(mob).is_some_and(|fox| self.can_sleep(fox))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let Some(fox) = fox(mob) else {
            return;
        };
        fox.set_sitting(false);
        fox.set_is_crouching(false);
        fox.set_is_interested(false);
        mob.set_jumping(false);
        fox.set_sleeping_flag(true);
        mob.mob_base().navigation().lock().stop();
        mob.mob_base()
            .controls()
            .lock()
            .move_control
            .set_wanted_position(mob.position(), 0.0);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.countdown = rand::random_range(0..wait_time_before_sleep());
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
    }
}

/// Vanilla `Fox.FoxFollowParentGoal`: cubs stop following while defending.
pub(super) struct FoxFollowParentGoal {
    follow: FollowParentGoal,
}

impl FoxFollowParentGoal {
    pub(super) const fn new() -> Self {
        Self {
            follow: FollowParentGoal::new(1.25),
        }
    }
}

impl Goal for FoxFollowParentGoal {
    fn controls(&self) -> GoalControls {
        self.follow.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !fox(mob).is_some_and(FoxEntity::is_defending) && self.follow.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !fox(mob).is_some_and(FoxEntity::is_defending) && self.follow.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
        self.follow.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.follow.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.follow.tick(mob, entity);
    }
}

/// Vanilla `Fox.FoxStrollThroughVillageGoal`.
pub(super) struct FoxStrollThroughVillageGoal {
    stroll: StrollThroughVillageGoal,
}

impl FoxStrollThroughVillageGoal {
    pub(super) const fn new() -> Self {
        Self {
            stroll: StrollThroughVillageGoal::new(200),
        }
    }

    fn can_fox_move(mob: &dyn PathfinderMob) -> bool {
        fox(mob).is_some_and(|fox| {
            !fox.is_sleeping() && !fox.is_sitting() && !fox.is_defending() && mob.target().is_none()
        })
    }
}

impl Goal for FoxStrollThroughVillageGoal {
    fn controls(&self) -> GoalControls {
        self.stroll.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.stroll.can_use(mob) && Self::can_fox_move(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.stroll.can_continue_to_use(mob) && Self::can_fox_move(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.clear_states();
        }
        self.stroll.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.stroll.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.stroll.tick(mob, entity);
    }
}

/// Vanilla `CaveVines.hasGlowBerries`.
fn has_glow_berries(state: BlockStateId) -> bool {
    let block = state.get_block();
    (block == &vanilla_blocks::CAVE_VINES || block == &vanilla_blocks::CAVE_VINES_PLANT)
        && state.get_value(&BlockStateProperties::BERRIES)
}

fn is_ripe_sweet_berry_bush(state: BlockStateId) -> bool {
    state.get_block() == &vanilla_blocks::SWEET_BERRY_BUSH
        && state.get_value(&BlockStateProperties::AGE_3) >= 2
}

/// Vanilla `Fox.FoxEatBerriesGoal`: walks to ripe sweet berries or glow
/// berries and picks them.
pub(super) struct FoxEatBerriesGoal {
    move_to: MoveToBlockGoal,
    ticks_waited: i32,
}

impl FoxEatBerriesGoal {
    pub(super) fn new() -> Self {
        Self {
            move_to: MoveToBlockGoal::with_vertical_search_range(1.2, 12, 1, |world, pos| {
                let state = world.get_block_state(pos);
                is_ripe_sweet_berry_bush(state) || has_glow_berries(state)
            })
            .with_accepted_distance(2.0)
            .with_recalculate_path_interval(100),
            ticks_waited: 0,
        }
    }

    fn on_reached_target(&self, mob: &dyn PathfinderMob, fox: &FoxEntity) {
        let Some(world) = mob.level() else {
            return;
        };
        if !world.get_game_rule(&MOB_GRIEFING) {
            return;
        }
        let pos = self.move_to.block_pos();
        let state = world.get_block_state(pos);
        if state.get_block() == &vanilla_blocks::SWEET_BERRY_BUSH {
            let age = state.get_value(&BlockStateProperties::AGE_3);
            let mut count = 1 + rand::random_range(0..2) + i32::from(age == 3);
            if fox.main_hand().is_empty() {
                fox.set_main_hand(ItemStack::new(&vanilla_items::SWEET_BERRIES));
                count -= 1;
            }
            if count > 0 {
                world.pop_resource(
                    pos,
                    ItemStack::with_count(&vanilla_items::SWEET_BERRIES, count),
                );
            }
            mob.play_sound(&sound_events::BLOCK_SWEET_BERRY_BUSH_PICK_BERRIES, 1.0, 1.0);
            world.set_block(
                pos,
                state.set_value(&BlockStateProperties::AGE_3, 1),
                UpdateFlags::UPDATE_CLIENTS,
            );
            world.game_event(
                &vanilla_game_events::BLOCK_CHANGE,
                pos,
                &GameEventContext::new(Some(mob.as_entity_event_source()), None),
            );
        } else if has_glow_berries(state) {
            let _ = CaveVinesBlock::use_block(mob.as_entity_event_source(), state, &world, pos);
        }
    }
}

impl Goal for FoxEatBerriesGoal {
    fn controls(&self) -> GoalControls {
        self.move_to.controls()
    }

    fn requires_update_every_tick(&self) -> bool {
        self.move_to.requires_update_every_tick()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        !fox(mob).is_some_and(FoxEntity::is_sleeping) && self.move_to.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.move_to.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.ticks_waited = 0;
        if let Some(fox) = fox(mob) {
            fox.set_sitting(false);
        }
        self.move_to.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.move_to.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        if self.move_to.is_reached_target() {
            if self.ticks_waited >= BERRY_WAIT_TICKS {
                if let Some(fox) = fox(mob) {
                    self.on_reached_target(mob, fox);
                }
            } else {
                self.ticks_waited += 1;
            }
        } else if rand::random::<f32>() < 0.05 {
            mob.play_sound(&sound_events::ENTITY_FOX_SNIFF, 1.0, 1.0);
        }
        self.move_to.tick(mob, entity);
    }
}

fn nearby_items(mob: &dyn PathfinderMob) -> Vec<SharedEntity> {
    let Some(world) = mob.level() else {
        return Vec::new();
    };
    let area = mob.bounding_box().inflate(ITEM_SEARCH_RANGE);
    world.get_entities_in_aabb_matching(&area, |entity| {
        entity
            .downcast_ref::<ItemEntity>()
            .is_some_and(is_allowed_item)
    })
}

/// Vanilla `Fox.FoxSearchForItemsGoal`: an empty-mouthed fox walks to items
/// lying around.
pub(super) struct FoxSearchForItemsGoal;

impl Goal for FoxSearchForItemsGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        if !fox.main_hand().is_empty()
            || mob.target().is_some()
            || mob.last_hurt_by_mob().is_some()
            || !fox.can_move()
            || !chance(reduced_tick_delay(10))
        {
            return false;
        }
        !nearby_items(mob).is_empty()
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(item) = nearby_items(mob).first() {
            mob.move_to_entity(item.as_ref(), ITEM_SPEED);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let items = nearby_items(mob);
        if fox(mob).is_some_and(|fox| fox.main_hand().is_empty())
            && let Some(item) = items.first()
        {
            mob.move_to_entity(item.as_ref(), ITEM_SPEED);
        }
    }
}

/// Vanilla `Fox.FoxLookAtPlayerGoal`.
pub(super) struct FoxLookAtPlayerGoal {
    look: LookAtPlayerGoal,
}

impl FoxLookAtPlayerGoal {
    pub(super) fn new() -> Self {
        Self {
            look: LookAtPlayerGoal::new(24.0),
        }
    }

    fn free(mob: &dyn PathfinderMob) -> bool {
        fox(mob).is_some_and(|fox| !fox.is_faceplanted() && !fox.is_interested())
    }
}

impl Goal for FoxLookAtPlayerGoal {
    fn controls(&self) -> GoalControls {
        self.look.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.look.can_use(mob) && Self::free(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.look.can_continue_to_use(mob) && Self::free(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.look.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.look.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.look.tick(mob, entity);
    }
}

/// Vanilla `Fox.PerchAndSearchGoal`: sits down and looks around now and then.
pub(super) struct PerchAndSearchGoal {
    rel_x: f64,
    rel_z: f64,
    look_time: i32,
    looks_remaining: i32,
}

impl PerchAndSearchGoal {
    pub(super) const fn new() -> Self {
        Self {
            rel_x: 0.0,
            rel_z: 0.0,
            look_time: 0,
            looks_remaining: 0,
        }
    }

    fn reset_look(&mut self) {
        let angle = TAU * rand::random::<f64>();
        self.rel_x = angle.cos();
        self.rel_z = angle.sin();
        self.look_time = reduced_tick_delay(80 + rand::random_range(0..20));
    }
}

impl Goal for PerchAndSearchGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(fox) = fox(mob) else {
            return false;
        };
        mob.last_hurt_by_mob().is_none()
            && rand::random::<f32>() < 0.02
            && !fox.is_sleeping()
            && mob.target().is_none()
            && mob.mob_base().navigation().lock().is_done()
            && !alertable(fox)
            && !fox.is_pouncing()
            && !fox.is_crouching()
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        self.looks_remaining > 0
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.reset_look();
        self.looks_remaining = 2 + rand::random_range(0..3);
        if let Some(fox) = fox(mob) {
            fox.set_sitting(true);
        }
        mob.mob_base().navigation().lock().stop();
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(fox) = fox(mob) {
            fox.set_sitting(false);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        self.look_time -= 1;
        if self.look_time <= 0 {
            self.looks_remaining -= 1;
            self.reset_look();
        }
        let position = mob.position();
        mob.mob_base().controls().lock().look_control.set_look_at(
            DVec3::new(
                position.x + self.rel_x,
                mob.get_eye_y(),
                position.z + self.rel_z,
            ),
            mob.max_head_y_rot(),
            mob.max_head_x_rot(),
        );
    }
}

/// Vanilla `TRUSTED_TARGET_SELECTOR`: whoever hurt the trusted player in the
/// last 30 seconds, and is not trusted itself.
fn is_trusted_target(target: &dyn LivingEntity, fox: &dyn LivingEntity, _world: &World) -> bool {
    target.last_hurt_mob().is_some()
        && target.last_hurt_mob_timestamp() < target.tick_count() + TRUSTED_HURT_MEMORY
        && fox_of(fox).is_some_and(|fox| !fox.trusts(target.as_entity_event_source()))
}

/// Vanilla `Fox.DefendTrustedTargetGoal`: attacks whoever hurt a player the
/// fox trusts.
pub(super) struct DefendTrustedTargetGoal {
    target: NearestAttackableTargetGoal,
    random_interval: i32,
    trusted_last_hurt_by: Option<SharedEntity>,
    trusted_last_hurt: Option<SharedEntity>,
    timestamp: i32,
}

impl DefendTrustedTargetGoal {
    pub(super) fn new() -> Self {
        Self {
            target: NearestAttackableTargetGoal::new_with_interval(10, false, false, |_, _| true)
                .with_targeter_selector(is_trusted_target),
            random_interval: reduced_tick_delay(10),
            trusted_last_hurt_by: None,
            trusted_last_hurt: None,
            timestamp: 0,
        }
    }
}

impl Goal for DefendTrustedTargetGoal {
    fn controls(&self) -> GoalControls {
        self.target.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if self.random_interval > 0 && !chance(self.random_interval) {
            return false;
        }
        let (Some(fox), Some(world)) = (fox(mob), mob.level()) else {
            return false;
        };
        for uuid in fox.trusted() {
            let Some(trusted) = world.get_entity_by_uuid(&uuid) else {
                continue;
            };
            let Some(trusted_living) = trusted.as_living_entity() else {
                continue;
            };
            let last_hurt_by = trusted_living.last_hurt_by_mob();
            let timestamp = trusted_living.last_hurt_by_mob_timestamp();
            let can_attack = self.target.can_attack_target(
                mob,
                last_hurt_by.as_ref().and_then(|e| e.as_living_entity()),
            );
            self.trusted_last_hurt_by = last_hurt_by;
            self.trusted_last_hurt = Some(trusted);
            return timestamp != self.timestamp && can_attack;
        }
        false
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.target.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        let _ = mob.set_target(self.trusted_last_hurt_by.as_ref());
        self.target.set_target(self.trusted_last_hurt_by.clone());
        if let Some(trusted) = self
            .trusted_last_hurt
            .as_ref()
            .and_then(|e| e.as_living_entity())
        {
            self.timestamp = trusted.last_hurt_by_mob_timestamp();
        }
        mob.play_sound(&sound_events::ENTITY_FOX_AGGRO, 1.0, 1.0);
        if let Some(fox) = fox(mob) {
            fox.set_defending(true);
            fox.wake_up();
        }
        self.target.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.target.stop(mob);
    }
}
