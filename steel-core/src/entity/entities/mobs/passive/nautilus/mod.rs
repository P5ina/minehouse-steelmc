//! Vanilla `AbstractNautilus` and the nautiluses built on it.
//!
//! The nautilus and the zombie nautilus are tamable, rideable water mobs
//! with a brain. They share taming, riding, dashing and their inventory
//! screen through the [`AbstractNautilus`] trait; each entity supplies its
//! sounds and brain.

mod ai;
mod living_nautilus;
#[cfg(test)]
mod tests;
mod zombie_nautilus;

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

use glam::DVec3;
use rand::Rng;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_game_events, vanilla_items, vanilla_mob_effects,
};
use steel_utils::BlockPos;
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::{IntoShared as _, Shared, SyncMutex};
use steel_utils::types::InteractionHand;

use crate::behavior::{ITEM_BEHAVIORS, InteractionResult};
use crate::entity::ai::brain::memory_types::{ANGRY_AT, ATTACK_TARGET};
use crate::entity::ai::smooth_swimming::SmoothSwimmingMoveControl;
use crate::entity::entities::mobs::passive::equine::menu::nautilus_inventory_menu;
use crate::entity::{
    AgeableMob, Entity, EntitySpawnReason, LivingEntity, MobEffectInstance, PathfinderMob,
    SharedEntity, TamableAnimal,
};
use crate::inventory::container::SimpleContainer;
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::ServerLevelAccessor;
use steel_registry::fluid::FluidStateExt as _;

pub use living_nautilus::NautilusEntity;
pub use zombie_nautilus::ZombieNautilusEntity;

/// Vanilla `AbstractNautilus.SMALL_RESTRICTION_RADIUS`.
const SMALL_RESTRICTION_RADIUS: i32 = 16;
/// Vanilla `AbstractNautilus.LARGE_RESTRICTION_RADIUS`.
const LARGE_RESTRICTION_RADIUS: i32 = 32;
/// Vanilla `AbstractNautilus.RESTRICTION_RADIUS_BUFFER`.
const RESTRICTION_RADIUS_BUFFER: i32 = 8;
/// Vanilla `AbstractNautilus.EFFECT_DURATION`.
const EFFECT_DURATION: i32 = 60;
/// Vanilla `AbstractNautilus.EFFECT_REFRESH_RATE`.
const EFFECT_REFRESH_RATE: i64 = 40;
/// Vanilla `AbstractNautilus.NAUTILUS_WATER_RESISTANCE`.
const WATER_RESISTANCE: f64 = 0.9;
/// Vanilla `AbstractNautilus.RIDDEN_SPEED_MODIFIER_IN_WATER`.
const RIDDEN_SPEED_MODIFIER_IN_WATER: f32 = 0.0325;
/// Vanilla `AbstractNautilus.RIDDEN_SPEED_MODIFIER_ON_LAND`.
const RIDDEN_SPEED_MODIFIER_ON_LAND: f32 = 0.02;
/// Vanilla `AbstractNautilus.DASH_COOLDOWN_TICKS`.
const DASH_COOLDOWN_TICKS: i32 = 40;
/// Vanilla `AbstractNautilus.DASH_MINIMUM_DURATION_TICKS`: the dash shows
/// for this many ticks after it starts.
const DASH_MINIMUM_DURATION_TICKS: i32 = 5;
/// Vanilla `AbstractNautilus.DASH_MOMENTUM_IN_WATER`.
const DASH_MOMENTUM_IN_WATER: f32 = 1.2;
/// Vanilla `AbstractNautilus.DASH_MOMENTUM_ON_LAND`.
const DASH_MOMENTUM_ON_LAND: f32 = 0.5;
/// Vanilla `AbstractNautilus.tickRidden` turn speed toward the rider's yaw.
const RIDDEN_TURN_SPEED: f32 = 0.5;
/// Vanilla `getRiddenInput` scale when the rider backs up.
const BACKWARDS_INPUT_SCALE: f32 = -0.5;
/// Vanilla `tryToTame`: one feeding in this many tames the nautilus.
const TAME_CHANCE: u32 = 3;
/// Vanilla `mobInteract` healing for food when hurt.
const FEED_HEALING_FACTOR: f32 = 2.0;
const FEED_DEFAULT_HEAL: f32 = 1.0;
/// Vanilla `checkNautilusSpawnRules`: spawns between 25 and 5 blocks below sea level.
const MIN_SPAWN_DEPTH: i32 = 25;
const MAX_SPAWN_DEPTH: i32 = 5;
/// Vanilla `SmoothSwimmingMoveControl(this, 85, 10, 0.011F, 0.0F, true)`.
pub(super) const NAUTILUS_MOVE_CONTROL: SmoothSwimmingMoveControl = SmoothSwimmingMoveControl {
    max_turn_x: 85,
    max_turn_y: 10,
    in_water_speed_modifier: 0.011,
    outside_water_speed_modifier: 0.0,
    apply_gravity: true,
};
/// Vanilla `SmoothSwimmingLookControl(this, 10)`.
pub(super) const NAUTILUS_LOOK_MAX_Y_ROT_FROM_CENTER: i32 = 10;

/// Vanilla `AbstractNautilus.checkNautilusSpawnRules`: deep water with water
/// above.
pub(crate) fn check_nautilus_spawn_rules(
    _entity_type: EntityTypeRef,
    level: &dyn ServerLevelAccessor,
    _spawn_reason: EntitySpawnReason,
    pos: BlockPos,
    _random: &mut dyn Rng,
) -> bool {
    let sea_level = level.sea_level();
    pos.y() >= sea_level - MIN_SPAWN_DEPTH
        && pos.y() <= sea_level - MAX_SPAWN_DEPTH
        && level
            .get_block_state(pos.below())
            .get_fluid_state()
            .is_water()
        && level.get_block_state(pos.above()).get_block() == &vanilla_blocks::WATER
}

/// The mutable state vanilla `AbstractNautilus` keeps in fields.
pub struct AbstractNautilusBase {
    /// Vanilla `dashCooldown`.
    dash_cooldown: AtomicI32,
    /// Vanilla `playerJumpPendingScale`.
    player_jump_pending_scale: SyncMutex<f32>,
    /// Vanilla `inventory`: a nautilus has no storage columns, so it stays
    /// empty, but its menu still checks it.
    inventory: Shared<SimpleContainer>,
}

impl AbstractNautilusBase {
    #[must_use]
    pub fn new() -> Self {
        Self {
            dash_cooldown: AtomicI32::new(0),
            player_jump_pending_scale: SyncMutex::new(0.0),
            inventory: SimpleContainer::new(0).into_shared(),
        }
    }

    /// Vanilla `getJumpCooldown`.
    #[must_use]
    pub fn dash_cooldown(&self) -> i32 {
        self.dash_cooldown.load(Ordering::Relaxed)
    }
}

impl Default for AbstractNautilusBase {
    fn default() -> Self {
        Self::new()
    }
}

/// Vanilla `AbstractNautilus`.
pub trait AbstractNautilus: TamableAnimal + PathfinderMob {
    /// The nautilus' field state.
    fn nautilus_base(&self) -> &AbstractNautilusBase;

    /// Vanilla `isDashing`, the synced `DASH` flag.
    fn is_dashing(&self) -> bool;

    /// Vanilla `setDashing`.
    fn set_dashing(&self, dashing: bool);

    /// Vanilla `getDashSound`.
    fn dash_sound(&self) -> Option<SoundEventRef>;

    /// Vanilla `getDashReadySound`.
    fn dash_ready_sound(&self) -> Option<SoundEventRef>;

    /// Vanilla `isAggravated`: angry at someone or already attacking.
    fn is_aggravated(&self) -> bool {
        self.brain_memories().is_some_and(|memories| {
            let memories = memories.lock();
            memories.has_value(&ANGRY_AT) || memories.has_value(&ATTACK_TARGET)
        })
    }

    /// Vanilla `isMobControlled`: a mob rides it.
    fn is_mob_controlled(&self) -> bool {
        self.first_passenger()
            .is_some_and(|passenger| passenger.is_mob())
    }

    /// Vanilla `isFood`: taming items while wild and grown, food otherwise.
    fn is_nautilus_food(&self, item_stack: &ItemStack) -> bool {
        let tag = if !self.is_tame() && !AgeableMob::is_baby(self) {
            &ItemTag::NAUTILUS_TAMING_ITEMS
        } else {
            &ItemTag::NAUTILUS_FOOD
        };
        REGISTRY.items.is_in_tag(item_stack.item(), tag)
    }

    /// Vanilla `usePlayerItem`: a fish bucket leaves a water bucket behind.
    fn use_nautilus_player_item(&self, player: &Player, hand: InteractionHand) {
        let is_bucket_food = {
            let inventory = player.inventory.lock();
            REGISTRY.items.is_in_tag(
                inventory.get_item_in_hand(hand).item(),
                &ItemTag::NAUTILUS_BUCKET_FOOD,
            )
        };
        if !is_bucket_food {
            player.inventory.lock().shrink_item_in_hand(hand, 1);
            return;
        }
        let overflow = player.inventory.lock().apply_filled_result(
            hand,
            ITEM_BEHAVIORS.default_instance(&vanilla_items::WATER_BUCKET),
            player.has_infinite_materials(),
            true,
        );
        if !overflow.is_empty() {
            let _ = player.drop_item(overflow, false, Prediction::Predicted);
        }
    }

    /// Vanilla `canUseSlot`: the saddle and body slots belong to grown,
    /// tamed, living nautiluses.
    fn can_use_nautilus_slot(&self, slot: EquipmentSlot) -> bool {
        if slot == EquipmentSlot::Saddle || slot == EquipmentSlot::Body {
            Entity::is_alive(self) && !AgeableMob::is_baby(self) && self.is_tame()
        } else {
            true
        }
    }

    /// Vanilla `getControllingPassenger`.
    fn nautilus_controlling_passenger(&self) -> Option<SharedEntity> {
        if self.is_saddled()
            && let Some(passenger) = self.first_passenger()
            && passenger.as_player().is_some()
        {
            return Some(passenger);
        }
        self.controlling_passenger_mob()
    }

    /// Vanilla `getRiddenInput`: the rider steers along their look pitch.
    fn nautilus_ridden_input(&self, controller: &Player) -> DVec3 {
        let input = controller.last_client_input();
        let strafe = input.left_intent();
        let forward_intent = input.forward_intent();
        let mut forward = 0.0;
        let mut up = 0.0;
        if forward_intent != 0.0 {
            let pitch = controller.rotation().1.to_radians();
            let mut forward_look = pitch.cos();
            let mut up_look = -pitch.sin();
            if forward_intent < 0.0 {
                forward_look *= BACKWARDS_INPUT_SCALE;
                up_look *= BACKWARDS_INPUT_SCALE;
            }
            up = up_look;
            forward = forward_look;
        }
        DVec3::new(f64::from(strafe), f64::from(up), f64::from(forward))
    }

    /// Vanilla `tickRidden`: turns toward the rider and dashes when asked.
    fn nautilus_tick_ridden(&self, controller: &Player) {
        let (rider_yaw, rider_pitch) = controller.rotation();
        let mut yaw = self.rotation().0;
        yaw += steel_math::wrap_degrees(rider_yaw - yaw) * RIDDEN_TURN_SPEED;
        self.set_rotation((yaw, rider_pitch * 0.5));
        self.base().set_old_yaw_to_current();
        self.set_y_body_rot(yaw);
        self.set_y_head_rot(yaw);
        if self.can_simulate_movement() {
            let pending = *self.nautilus_base().player_jump_pending_scale.lock();
            if pending > 0.0 && !self.is_jumping() {
                self.execute_riders_jump(pending, controller);
            }
            *self.nautilus_base().player_jump_pending_scale.lock() = 0.0;
        }
    }

    /// Vanilla `getRiddenSpeed`.
    fn nautilus_ridden_speed(&self) -> f32 {
        let speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED) as f32;
        if self.is_in_water() {
            RIDDEN_SPEED_MODIFIER_IN_WATER * speed
        } else {
            RIDDEN_SPEED_MODIFIER_ON_LAND * speed
        }
    }

    /// Vanilla `travelInWater`: swims along the input with steady drag.
    fn nautilus_travel_in_water(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        self.move_relative(self.get_speed(), input);
        let result = Arc::clone(entity).move_entity(MoverType::SelfMovement, self.velocity());
        self.set_velocity(self.velocity() * WATER_RESISTANCE);
        result
    }

    /// Vanilla `executeRidersJump`: a dash along the rider's look.
    fn execute_riders_jump(&self, amount: f32, controller: &Player) {
        let momentum = if self.is_in_water() {
            DASH_MOMENTUM_IN_WATER
        } else {
            DASH_MOMENTUM_ON_LAND
        };
        let speed = self
            .attributes()
            .lock()
            .required_value(vanilla_attributes::MOVEMENT_SPEED);
        let scale = f64::from(momentum * amount) * speed * f64::from(self.block_speed_factor());
        self.set_velocity(self.velocity() + controller.look_angle() * scale);
        self.nautilus_base()
            .dash_cooldown
            .store(DASH_COOLDOWN_TICKS, Ordering::Relaxed);
        self.set_dashing(true);
        self.mark_velocity_sync();
    }

    /// Vanilla `onPlayerJump`.
    fn on_player_jump(&self, jump_amount: i32) {
        if self.is_saddled() && self.nautilus_base().dash_cooldown() <= 0 {
            *self.nautilus_base().player_jump_pending_scale.lock() =
                player_jump_pending_scale(jump_amount);
        }
    }

    /// Vanilla `handleStartJump`.
    fn nautilus_handle_start_jump(&self) {
        self.make_sound(self.dash_sound());
        self.game_event(&vanilla_game_events::ENTITY_ACTION);
        self.set_dashing(true);
    }

    /// Vanilla `doPlayerRide`.
    fn do_player_ride(&self, player: &Player) {
        if let Some(world) = self.level()
            && let Some(vehicle) = world.get_entity_by_id(self.id())
        {
            let _ = player.start_riding(&vehicle);
        }
        if !self.is_vehicle() {
            self.clear_home();
        }
    }

    /// Vanilla `getNautilusRestrictionRadius`.
    fn restriction_radius(&self) -> i32 {
        if !AgeableMob::is_baby(self) && self.equipment_in_slot(EquipmentSlot::Saddle).is_empty() {
            LARGE_RESTRICTION_RADIUS
        } else {
            SMALL_RESTRICTION_RADIUS
        }
    }

    /// Vanilla `checkRestriction`: a tame nautilus stays near where it was
    /// left.
    fn check_restriction(&self) {
        if self.is_leashed() || self.is_vehicle() || !self.is_tame() {
            return;
        }
        let radius = self.restriction_radius();
        let position = self.block_position();
        let near_home = self.has_home()
            && self
                .home_position()
                .closer_than(position, f64::from(radius + RESTRICTION_RADIUS_BUFFER));
        if !near_home || radius != self.home_radius() {
            self.set_home_to(position, radius);
        }
    }

    /// Vanilla `applyEffects`: the rider breathes underwater.
    fn apply_rider_effects(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let Some(passenger) = self.first_passenger() else {
            return;
        };
        let Some(player) = passenger.as_player() else {
            return;
        };
        let has_effect = player.has_mob_effect(vanilla_mob_effects::BREATH_OF_THE_NAUTILUS);
        let should_refresh = world.game_time() % EFFECT_REFRESH_RATE == 0;
        if !has_effect || should_refresh {
            player.add_mob_effect(
                MobEffectInstance::with_duration(
                    vanilla_mob_effects::BREATH_OF_THE_NAUTILUS,
                    EFFECT_DURATION,
                    0,
                )
                .with_ambient(true)
                .with_visible(true),
            );
        }
    }

    /// Vanilla `AbstractNautilus.tick` after `super.tick()`.
    fn tick_nautilus(&self) {
        self.apply_rider_effects();
        let base = self.nautilus_base();
        if self.is_dashing()
            && base.dash_cooldown() < DASH_COOLDOWN_TICKS - DASH_MINIMUM_DURATION_TICKS
        {
            self.set_dashing(false);
        }
        if base.dash_cooldown() > 0 {
            let cooldown = base.dash_cooldown.fetch_sub(1, Ordering::Relaxed) - 1;
            if cooldown == 0 {
                self.make_sound(self.dash_ready_sound());
            }
        }
        // Vanilla `spawnBubbles` adds client-side particles only.
    }

    /// Vanilla `tryToTame`.
    fn try_to_tame(&self, player: &Player) {
        if rand::random_range(0..TAME_CHANCE) == 0 {
            self.tame(player);
            self.mob_base().navigation().lock().stop();
            self.broadcast_entity_event(EntityStatus::TamingSucceeded);
        } else {
            self.broadcast_entity_event(EntityStatus::TamingFailed);
        }
        self.play_eating_sound();
    }

    /// Vanilla `mobInteract`.
    fn nautilus_mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        if AgeableMob::is_baby(self) {
            return self.mob_interact_animal(player, hand);
        }
        if self.is_tame() && player.is_secondary_use_active() {
            self.open_custom_inventory_screen(player);
            return InteractionResult::Success;
        }
        let item_stack = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        let is_food = self.is_food(&item_stack);
        if !item_stack.is_empty() {
            if !self.is_tame() && is_food {
                self.use_nautilus_player_item(player, hand);
                self.try_to_tame(player);
                return InteractionResult::SuccessServer;
            }
            if is_food && self.get_health() < self.get_max_health() {
                self.feed(player, hand, FEED_HEALING_FACTOR, FEED_DEFAULT_HEAL);
                return InteractionResult::Success;
            }
            let result = self.interact_living_entity_with_equippable(player, hand);
            if result.consumes_action() {
                return result;
            }
            if let Some(me) = self.as_living_entity() {
                let result = {
                    let mut inventory = player.inventory.lock();
                    let held = inventory.get_item_in_hand_mut(hand);
                    ITEM_BEHAVIORS
                        .get_behavior(item_stack.item())
                        .interact_living_entity(held, player, me, hand)
                };
                if result.consumes_action() {
                    return result;
                }
            }
        }
        if self.is_tame() && !player.is_secondary_use_active() && !is_food {
            self.do_player_ride(player);
            return InteractionResult::Success;
        }
        self.mob_interact_animal(player, hand)
    }

    /// Vanilla `openCustomInventoryScreen`.
    fn open_custom_inventory_screen(&self, player: &Player) {
        let rides_it = self
            .passengers()
            .iter()
            .any(|passenger| passenger.id() == player.id());
        if (self.is_vehicle() && !rides_it) || !self.is_tame() {
            return;
        }
        let Some(world) = self.level() else {
            return;
        };
        let Some(mount) = world.get_entity_by_id(self.id()) else {
            return;
        };
        let mount_inventory = Arc::clone(&self.nautilus_base().inventory);
        player.open_mount_menu(0, self.id(), move |context| {
            nautilus_inventory_menu(
                &context.player.inventory,
                context.container_id,
                &mount,
                mount_inventory,
            )
        });
    }

    /// Vanilla `getEquipSound` for the saddle, which sounds different underwater.
    fn nautilus_equip_sound(
        &self,
        slot: EquipmentSlot,
        stack: &ItemStack,
    ) -> Option<SoundEventRef> {
        if slot == EquipmentSlot::Saddle {
            return Some(if self.is_under_water() {
                &sound_events::ITEM_NAUTILUS_SADDLE_UNDERWATER_EQUIP
            } else {
                &sound_events::ITEM_NAUTILUS_SADDLE_EQUIP
            });
        }
        let equippable = stack.get_equippable()?;
        (slot == equippable.slot)
            .then(|| equippable.equip_sound.registry_ref())
            .flatten()
    }
}

/// Vanilla `PlayerRideableJumping.getPlayerJumpPendingScale`.
fn player_jump_pending_scale(jump_amount: i32) -> f32 {
    if jump_amount >= 90 {
        1.0
    } else {
        0.4 + 0.4 * jump_amount as f32 / 90.0
    }
}
