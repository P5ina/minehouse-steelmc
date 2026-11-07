//! The panda's own goals, vanilla `Panda`'s inner goal classes.

use glam::DVec3;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_blocks;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_utils::{BlockPos, Downcast as _, Identifier};

use super::{PandaEntity, TOTAL_UNHAPPY_TIME, can_pick_up_and_eat, is_aggressive_panda};
use crate::entity::ai::control::{DEFAULT_LOOK_X_MAX_ROT_ANGLE, DEFAULT_LOOK_Y_MAX_ROT_SPEED};
use crate::entity::ai::goal::{
    AvoidEntityGoal, BreedGoal, Goal, GoalControls, HurtByTargetGoal, MeleeAttackGoal, PanicGoal,
    reduced_tick_delay,
};
use crate::entity::ai::targeting::TargetingConditions;
use crate::entity::entities::ItemEntity;
use crate::entity::{AgeableMob, EquipmentSlot, PathfinderMob, SharedEntity};

/// Vanilla `Panda.BREED_TARGETING` range.
const BREED_TARGETING_RANGE: f64 = 8.0;
/// Ticks before an unhappy panda shakes its head again.
const UNHAPPY_COOLDOWN: i32 = 600;
const BAMBOO_SEARCH_RADIUS: i32 = 8;
const BAMBOO_SEARCH_HEIGHT: i32 = 3;
const LOOK_DISTANCE: f64 = 6.0;
const LOOK_PROBABILITY: f32 = 0.02;
const SIT_ITEM_SEARCH: f64 = 6.0;
const SIT_ITEM_PATH_SEARCH: f64 = 8.0;
const SIT_ITEM_SPEED: f64 = 1.2;

fn panda(mob: &dyn PathfinderMob) -> Option<&PandaEntity> {
    mob.downcast_ref::<PandaEntity>()
}

fn chance(bound: i32) -> bool {
    rand::random_range(0..bound.max(1)) == 1
}

/// Vanilla `PandaPanicGoal`: panics at environmental damage only, and stops
/// once the panda sits down.
pub(super) struct PandaPanicGoal {
    panic: PanicGoal,
}

fn environmental_panic_causes(_mob: &dyn PathfinderMob) -> Identifier {
    DamageTypeTag::PANIC_ENVIRONMENTAL_CAUSES
}

impl PandaPanicGoal {
    pub(super) const fn new() -> Self {
        Self {
            panic: PanicGoal::new(2.0).with_panic_causes(environmental_panic_causes),
        }
    }
}

impl Goal for PandaPanicGoal {
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
        if panda(mob).is_some_and(PandaEntity::is_sitting) {
            mob.mob_base().navigation().lock().stop();
            return false;
        }
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

/// Vanilla `PandaBreedGoal`: pandas only breed with bamboo nearby, and are
/// unhappy otherwise.
pub(super) struct PandaBreedGoal {
    breed: BreedGoal,
    unhappy_cooldown: i32,
}

impl PandaBreedGoal {
    pub(super) const fn new() -> Self {
        Self {
            breed: BreedGoal::new(1.0),
            unhappy_cooldown: 0,
        }
    }

    /// Vanilla `PandaBreedGoal.canFindBamboo`: bamboo up to 2 blocks above
    /// and 7 blocks around the panda.
    fn can_find_bamboo(mob: &dyn PathfinderMob) -> bool {
        let Some(world) = mob.level() else {
            return false;
        };
        let origin = mob.block_position();
        for y in 0..BAMBOO_SEARCH_HEIGHT {
            for r in 0..BAMBOO_SEARCH_RADIUS {
                let mut x = 0;
                while x <= r {
                    let mut z = if x < r && x > -r { r } else { 0 };
                    while z <= r {
                        let pos = BlockPos::new(origin.x() + x, origin.y() + y, origin.z() + z);
                        if world.get_block_state(pos).get_block() == &vanilla_blocks::BAMBOO {
                            return true;
                        }
                        z = if z > 0 { -z } else { 1 - z };
                    }
                    x = if x > 0 { -x } else { 1 - x };
                }
            }
        }
        false
    }
}

impl Goal for PandaBreedGoal {
    fn controls(&self) -> GoalControls {
        self.breed.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(panda) = panda(mob) else {
            return false;
        };
        if !self.breed.can_use(mob) || panda.unhappy_counter() != 0 {
            return false;
        }
        if Self::can_find_bamboo(mob) {
            return true;
        }
        if self.unhappy_cooldown <= mob.tick_count() {
            panda.set_unhappy_counter(TOTAL_UNHAPPY_TIME);
            self.unhappy_cooldown = mob.tick_count() + UNHAPPY_COOLDOWN;
            if mob.is_effective_ai()
                && let Some(world) = mob.level()
            {
                let targeting = TargetingConditions::for_non_combat().range(BREED_TARGETING_RANGE);
                let player = world.nearest_player(mob.position(), -1.0, |player| {
                    targeting.test(world.as_ref(), Some(mob), player)
                });
                panda.set_look_at(player.map(|player| -> SharedEntity { player }));
            }
        }
        false
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.breed.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.breed.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.breed.stop(mob);
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.breed.tick(mob, entity);
    }
}

/// Vanilla `PandaAttackGoal`: melee, only while the panda is free to act.
pub(super) struct PandaAttackGoal {
    attack: MeleeAttackGoal,
}

impl PandaAttackGoal {
    pub(super) const fn new() -> Self {
        Self {
            attack: MeleeAttackGoal::new(1.2, true),
        }
    }
}

impl Goal for PandaAttackGoal {
    fn controls(&self) -> GoalControls {
        self.attack.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        panda(mob).is_some_and(PandaEntity::can_perform_action) && self.attack.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.attack.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.attack.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.attack.stop(mob);
    }

    fn requires_update_every_tick(&self) -> bool {
        self.attack.requires_update_every_tick()
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, entity: &SharedEntity) {
        self.attack.tick(mob, entity);
    }
}

/// Vanilla `PandaAvoidGoal`: worried pandas run from players and monsters.
pub(super) struct PandaAvoidGoal {
    avoid: AvoidEntityGoal,
}

impl PandaAvoidGoal {
    pub(super) fn players() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(8.0, 2.0, 2.0, |target, _| {
                target.as_player().is_some() && !target.is_spectator()
            }),
        }
    }

    pub(super) fn monsters() -> Self {
        Self {
            avoid: AvoidEntityGoal::with_selector(4.0, 2.0, 2.0, |target, _| {
                target.is_monster() && !target.is_spectator()
            }),
        }
    }
}

impl Goal for PandaAvoidGoal {
    fn controls(&self) -> GoalControls {
        self.avoid.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        panda(mob).is_some_and(|panda| panda.is_worried() && panda.can_perform_action())
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

/// Vanilla `PandaHurtByTargetGoal`: only aggressive pandas join in, and the
/// panda calms down once it got bamboo or bit back.
pub(super) struct PandaHurtByTargetGoal {
    hurt_by: HurtByTargetGoal,
}

impl PandaHurtByTargetGoal {
    pub(super) fn new() -> Self {
        Self {
            hurt_by: HurtByTargetGoal::new().set_alert_others_matching(is_aggressive_panda, []),
        }
    }
}

impl Goal for PandaHurtByTargetGoal {
    fn controls(&self) -> GoalControls {
        self.hurt_by.controls()
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.hurt_by.can_use(mob)
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if panda(mob).is_some_and(PandaEntity::got_bamboo_or_bit) {
            let _ = mob.set_target(None);
            return false;
        }
        self.hurt_by.can_continue_to_use(mob)
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.start(mob);
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        self.hurt_by.stop(mob);
    }
}

/// Vanilla `PandaLieOnBackGoal`: lazy pandas lie on their back now and then.
pub(super) struct PandaLieOnBackGoal {
    cooldown: i32,
}

impl PandaLieOnBackGoal {
    pub(super) const fn new() -> Self {
        Self { cooldown: 0 }
    }
}

impl Goal for PandaLieOnBackGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        self.cooldown < mob.tick_count()
            && panda(mob).is_some_and(|panda| panda.is_lazy() && panda.can_perform_action())
            && chance(reduced_tick_delay(400))
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let lazy = panda(mob).is_some_and(PandaEntity::is_lazy);
        !mob.is_in_water()
            && (lazy || !chance(reduced_tick_delay(600)))
            && !chance(reduced_tick_delay(2000))
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(panda) = panda(mob) {
            panda.set_on_back(true);
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(panda) = panda(mob) {
            panda.set_on_back(false);
        }
        self.cooldown = mob.tick_count() + 200;
    }
}

/// Vanilla `PandaLookAtPlayerGoal`: a look-at-player goal whose target the
/// breed goal can set, and which only runs while the panda is free to act.
pub(super) struct PandaLookAtPlayerGoal {
    look_time: i32,
    look_at_context: TargetingConditions,
}

impl PandaLookAtPlayerGoal {
    pub(super) const fn new() -> Self {
        Self {
            look_time: 0,
            look_at_context: TargetingConditions::for_non_combat().range(LOOK_DISTANCE),
        }
    }
}

impl Goal for PandaLookAtPlayerGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::LOOK
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        if rand::random::<f32>() >= LOOK_PROBABILITY {
            return false;
        }
        let Some(panda) = panda(mob) else {
            return false;
        };
        if panda.look_at_target().is_none()
            && let Some(world) = mob.level()
        {
            let position = mob.position();
            let origin = DVec3::new(position.x, mob.get_eye_y(), position.z);
            let player = world.nearest_player(origin, -1.0, |player| {
                !mob.has_indirect_passenger(player)
                    && self.look_at_context.test(world.as_ref(), Some(mob), player)
            });
            panda.set_look_at(player.map(|player| -> SharedEntity { player }));
        }
        panda.can_perform_action() && panda.look_at_target().is_some()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(look_at) = panda(mob).and_then(PandaEntity::look_at_target) else {
            return false;
        };
        look_at.is_alive()
            && mob.position().distance_squared(look_at.position()) <= LOOK_DISTANCE * LOOK_DISTANCE
            && self.look_time > 0
    }

    fn start(&mut self, _mob: &dyn PathfinderMob) {
        self.look_time = reduced_tick_delay(40 + rand::random_range(0..40));
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        if let Some(panda) = panda(mob) {
            panda.set_look_at(None);
        }
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        let Some(look_at) = panda(mob).and_then(PandaEntity::look_at_target) else {
            return;
        };
        if look_at.is_alive() {
            let position = look_at.position();
            mob.mob_base().controls().lock().look_control.set_look_at(
                DVec3::new(position.x, look_at.get_eye_y(), position.z),
                DEFAULT_LOOK_Y_MAX_ROT_SPEED,
                DEFAULT_LOOK_X_MAX_ROT_ANGLE,
            );
        }
        self.look_time -= 1;
    }
}

/// Vanilla `PandaRollGoal`: cubs and playful pandas roll forward, always when
/// facing a drop.
pub(super) struct PandaRollGoal;

impl Goal for PandaRollGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE | GoalControls::LOOK | GoalControls::JUMP
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(panda) = panda(mob) else {
            return false;
        };
        if !(AgeableMob::is_baby(panda) || panda.is_playful()) || !mob.on_ground() {
            return false;
        }
        if !panda.can_perform_action() {
            return false;
        }
        let angle = mob.rotation().0.to_radians();
        let x_dir = -angle.sin();
        let z_dir = angle.cos();
        let x_step = if x_dir.abs() > 0.5 {
            x_dir.signum() as i32
        } else {
            0
        };
        let z_step = if z_dir.abs() > 0.5 {
            z_dir.signum() as i32
        } else {
            0
        };
        let below_ahead = mob.block_position().offset(x_step, -1, z_step);
        if mob
            .level()
            .is_some_and(|world| world.get_block_state(below_ahead).is_air())
        {
            return true;
        }
        if panda.is_playful() && chance(reduced_tick_delay(60)) {
            return true;
        }
        chance(reduced_tick_delay(500))
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(panda) = panda(mob) {
            panda.roll(true);
        }
    }

    fn is_interruptable(&self) -> bool {
        false
    }
}

/// Vanilla `PandaSitGoal`: sits down to eat what it holds, or walks to food
/// on the ground first.
pub(super) struct PandaSitGoal {
    cooldown: i32,
}

impl PandaSitGoal {
    pub(super) const fn new() -> Self {
        Self { cooldown: 0 }
    }

    fn ground_food(mob: &dyn PathfinderMob, range: f64) -> Vec<SharedEntity> {
        let Some(world) = mob.level() else {
            return Vec::new();
        };
        let area = mob.bounding_box().inflate(range);
        world.get_entities_in_aabb_matching(&area, |entity| {
            entity
                .downcast_ref::<ItemEntity>()
                .is_some_and(can_pick_up_and_eat)
        })
    }
}

fn holds_item(mob: &dyn PathfinderMob) -> bool {
    !mob.equipment_in_slot(EquipmentSlot::MainHand).is_empty()
}

impl Goal for PandaSitGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::MOVE
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(panda) = panda(mob) else {
            return false;
        };
        if self.cooldown > mob.tick_count()
            || AgeableMob::is_baby(panda)
            || mob.is_in_water()
            || !panda.can_perform_action()
            || panda.unhappy_counter() > 0
        {
            return false;
        }
        holds_item(mob) || !Self::ground_food(mob, SIT_ITEM_SEARCH).is_empty()
    }

    fn can_continue_to_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let lazy = panda(mob).is_some_and(PandaEntity::is_lazy);
        !mob.is_in_water()
            && (lazy || !chance(reduced_tick_delay(600)))
            && !chance(reduced_tick_delay(2000))
    }

    fn tick(&mut self, mob: &dyn PathfinderMob, _entity: &SharedEntity) {
        if let Some(panda) = panda(mob)
            && !panda.is_sitting()
            && holds_item(mob)
        {
            panda.try_to_sit();
        }
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if holds_item(mob) {
            if let Some(panda) = panda(mob) {
                panda.try_to_sit();
            }
        } else if let Some(item) = Self::ground_food(mob, SIT_ITEM_PATH_SEARCH).first() {
            mob.move_to_pos(item.position(), SIT_ITEM_SPEED);
        }
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &dyn PathfinderMob) {
        let Some(panda) = panda(mob) else {
            return;
        };
        let item_stack = mob.equipment_in_slot(EquipmentSlot::MainHand);
        if !item_stack.is_empty() {
            mob.spawn_at_location(item_stack, 0.0);
            panda.set_main_hand(ItemStack::empty());
            let wait_seconds = if panda.is_lazy() {
                rand::random_range(0..50) + 10
            } else {
                rand::random_range(0..150) + 10
            };
            self.cooldown = mob.tick_count() + wait_seconds * 20;
        }
        panda.sit(false);
    }
}

/// Vanilla `PandaSneezeGoal`: cubs sneeze now and then, weak cubs more often.
pub(super) struct PandaSneezeGoal;

impl Goal for PandaSneezeGoal {
    fn controls(&self) -> GoalControls {
        GoalControls::EMPTY
    }

    fn can_use(&mut self, mob: &dyn PathfinderMob) -> bool {
        let Some(panda) = panda(mob) else {
            return false;
        };
        if !AgeableMob::is_baby(panda) || !panda.can_perform_action() {
            return false;
        }
        (panda.is_weak() && chance(reduced_tick_delay(500))) || chance(reduced_tick_delay(6000))
    }

    fn can_continue_to_use(&mut self, _mob: &dyn PathfinderMob) -> bool {
        false
    }

    fn start(&mut self, mob: &dyn PathfinderMob) {
        if let Some(panda) = panda(mob) {
            panda.sneeze(true);
        }
    }
}
