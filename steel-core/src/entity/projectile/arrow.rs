//! Vanilla `AbstractArrow`: the stuck-in-ground arrow physics, piercing,
//! critical hits, pickup and despawn shared by arrows and spectral arrows.
//!
//! Concrete arrows embed an [`ArrowBase`] and implement [`AbstractArrow`];
//! their `Entity::tick` calls [`AbstractArrow::abstract_arrow_tick`] (vanilla
//! `super.tick()`).

use rustc_hash::FxHashSet;
use std::str::FromStr as _;
use std::sync::Arc;

use glam::DVec3;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_protocol::packets::game::{CGameEvent, CTakeItemEntity, GameEventType};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::INTANGIBLE_PROJECTILE;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::{REGISTRY, RegistryExt as _, sound_events, vanilla_damage_types};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockStateId, ChunkPos, Identifier, WorldAabb};

use crate::behavior::{BLOCK_BEHAVIORS, BlockCollisionContext};
use crate::block_entity::block_state_nbt;
use crate::enchantment_helper::{self, EnchantmentDamageContext, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::{Entity, LivingEntity, RemovalReason, SharedEntity};
use crate::inventory::container::Container as _;
use crate::physics::{CollisionWorld as _, MoverType, WorldCollisionProvider};
use crate::player::Player;
use crate::world::{ClipBlockShape, ClipFluid, ClipHitResult, World};

use super::{
    EntityHitResult, Projectile, ProjectileDeflection, ProjectileHit, clip_segment, compute_margin,
    lerp_rotation, triangle_random,
};

/// Vanilla `AbstractArrow.ARROW_BASE_DAMAGE`.
pub const ARROW_BASE_DAMAGE: f64 = 2.0;
/// Vanilla `AbstractArrow.SHAKE_TIME`.
const SHAKE_TIME: i32 = 7;
/// Vanilla `AbstractArrow.WATER_INERTIA`.
const WATER_INERTIA: f32 = 0.6;
/// Vanilla `AbstractArrow.INERTIA` (`getAirDrag`).
const INERTIA: f32 = 0.99;
/// Vanilla `AbstractArrow.getDefaultGravity`.
pub const ARROW_GRAVITY: f64 = 0.05;
/// Ticks an arrow stays in the ground before despawning (`tickDespawn`).
const DESPAWN_LIFE: i32 = 1200;
/// Vanilla `AbstractArrow.FLAG_CRIT`.
const FLAG_CRIT: i8 = 1;
/// Vanilla `AbstractArrow.FLAG_NOPHYSICS`.
const FLAG_NO_PHYSICS: i8 = 2;
/// Vanilla `onHitEntity` fire: `igniteForSeconds(5.0F)`.
const BURNING_ARROW_IGNITE_TICKS: i32 = 100;
/// Vanilla `doKnockback` horizontal scale.
const KNOCKBACK_SCALE: f64 = 0.6;
/// Vanilla `doKnockback` vertical push.
const KNOCKBACK_UP: f64 = 0.1;
/// Vanilla `onHitBlock` pull-back so the arrow sits just outside the face.
const HIT_BLOCK_PULL_BACK: f64 = 0.05;
/// Vanilla `shouldFall` probe radius.
const SHOULD_FALL_INFLATE: f64 = 0.06;
/// Vanilla `onHitEntity` deflect power when the target shrugs the arrow off.
const BOUNCE_DEFLECT_POWER: f64 = 0.2;
/// Vanilla mob arrow damage spread (`setBaseDamageFromMob`).
const MOB_DAMAGE_DEVIATION: f64 = 0.574_25;
/// Vanilla mob arrow damage per difficulty id.
const MOB_DAMAGE_PER_DIFFICULTY: f64 = 0.11;

/// Vanilla `AbstractArrow.Pickup`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArrowPickup {
    /// Nobody can pick the arrow up.
    #[default]
    Disallowed,
    /// Any player can pick the arrow up into their inventory.
    Allowed,
    /// Only players with infinite materials (creative) can pick it up.
    CreativeOnly,
}

impl ArrowPickup {
    /// Vanilla `Pickup.byOrdinal` (`LEGACY_CODEC`): out-of-range values fall
    /// back to `DISALLOWED`.
    #[must_use]
    pub const fn by_ordinal(ordinal: i8) -> Self {
        match ordinal {
            1 => Self::Allowed,
            2 => Self::CreativeOnly,
            _ => Self::Disallowed,
        }
    }

    /// The ordinal written by vanilla's `LEGACY_CODEC`.
    #[must_use]
    pub const fn ordinal(self) -> i8 {
        match self {
            Self::Disallowed => 0,
            Self::Allowed => 1,
            Self::CreativeOnly => 2,
        }
    }
}

struct ArrowState {
    last_state: Option<BlockStateId>,
    in_ground_time: i32,
    pickup: ArrowPickup,
    shake_time: i32,
    life: i32,
    base_damage: f64,
    sound_event: SoundEventRef,
    piercing_ignore_entity_ids: Option<FxHashSet<i32>>,
    pickup_item_stack: ItemStack,
    fired_from_weapon: Option<ItemStack>,
}

/// Runtime fields of vanilla `AbstractArrow` that are not synced data.
pub struct ArrowBase {
    state: SyncMutex<ArrowState>,
}

impl ArrowBase {
    /// Creates arrow state picking up `default_pickup_item` (vanilla
    /// `getDefaultPickupItem`).
    #[must_use]
    pub fn new(default_pickup_item: ItemStack) -> Self {
        Self {
            state: SyncMutex::new(ArrowState {
                last_state: None,
                in_ground_time: 0,
                pickup: ArrowPickup::Disallowed,
                shake_time: 0,
                life: 0,
                base_damage: ARROW_BASE_DAMAGE,
                sound_event: &sound_events::ENTITY_ARROW_HIT,
                piercing_ignore_entity_ids: None,
                pickup_item_stack: default_pickup_item,
                fired_from_weapon: None,
            }),
        }
    }
}

/// Vanilla-shaped behavior of entities that extend `AbstractArrow`.
pub trait AbstractArrow: Projectile {
    /// Returns the embedded non-synced arrow state.
    fn arrow_base(&self) -> &ArrowBase;

    /// Returns the synced `ID_FLAGS` byte (crit / no-physics).
    fn arrow_flags(&self) -> i8;

    /// Sets the synced `ID_FLAGS` byte.
    fn set_arrow_flags(&self, flags: i8);

    /// Returns the synced `PIERCE_LEVEL`.
    fn pierce_level(&self) -> i8;

    /// Sets the synced `PIERCE_LEVEL` (vanilla `setPierceLevel`).
    fn set_pierce_level(&self, pierce_level: i8);

    /// Returns the synced `IN_GROUND` flag (vanilla `isInGround`).
    fn is_in_ground(&self) -> bool;

    /// Sets the synced `IN_GROUND` flag (vanilla `setInGround`).
    fn set_in_ground(&self, in_ground: bool);

    /// Vanilla `getDefaultPickupItem`.
    fn get_default_pickup_item(&self) -> ItemStack;

    /// Vanilla `doPostHurtEffects`: applied to a living target after damage.
    fn do_post_hurt_effects(&self, _mob: &dyn LivingEntity) {}

    /// Vanilla `getWaterInertia`.
    fn get_water_inertia(&self) -> f32 {
        WATER_INERTIA
    }

    /// Vanilla `getAirDrag`.
    fn get_arrow_air_drag(&self) -> f32 {
        INERTIA
    }

    /// Vanilla `setPickupItemStack`: an empty stack resets to the default.
    /// Overrides (tipped arrows recolor) call
    /// [`AbstractArrow::default_set_pickup_item_stack`] first.
    fn set_pickup_item_stack(&self, item_stack: ItemStack) {
        self.default_set_pickup_item_stack(item_stack);
    }

    /// The base `AbstractArrow.setPickupItemStack`.
    fn default_set_pickup_item_stack(&self, item_stack: ItemStack) {
        let item_stack = if item_stack.is_empty() {
            self.get_default_pickup_item()
        } else {
            item_stack
        };
        self.arrow_base().state.lock().pickup_item_stack = item_stack;
    }

    /// Vanilla `getPickupItemStackOrigin` (a copy; mutate through
    /// [`AbstractArrow::set_pickup_item_stack`]).
    fn pickup_item_stack(&self) -> ItemStack {
        self.arrow_base().state.lock().pickup_item_stack.clone()
    }

    /// Vanilla `getPickupItem`.
    fn get_pickup_item(&self) -> ItemStack {
        self.pickup_item_stack()
    }

    /// Vanilla `getWeaponItem`.
    fn get_weapon_item(&self) -> Option<ItemStack> {
        self.arrow_base().state.lock().fired_from_weapon.clone()
    }

    /// Vanilla `onItemBreak`: forget the weapon once an enchantment broke it.
    fn on_item_break(&self) {
        self.arrow_base().state.lock().fired_from_weapon = None;
    }

    /// Vanilla `pickup`.
    fn pickup(&self) -> ArrowPickup {
        self.arrow_base().state.lock().pickup
    }

    /// Sets vanilla `pickup`.
    fn set_pickup(&self, pickup: ArrowPickup) {
        self.arrow_base().state.lock().pickup = pickup;
    }

    /// Vanilla `shakeTime`.
    fn shake_time(&self) -> i32 {
        self.arrow_base().state.lock().shake_time
    }

    /// Vanilla `inGroundTime`.
    fn in_ground_time(&self) -> i32 {
        self.arrow_base().state.lock().in_ground_time
    }

    /// Vanilla `baseDamage`.
    fn base_damage(&self) -> f64 {
        self.arrow_base().state.lock().base_damage
    }

    /// Vanilla `setBaseDamage`.
    fn set_base_damage(&self, base_damage: f64) {
        self.arrow_base().state.lock().base_damage = base_damage;
    }

    /// Vanilla `setBaseDamageFromMob`.
    fn set_base_damage_from_mob(&self, power: f32) {
        let difficulty = self
            .level()
            .map_or(0, |world| i32::from(u8::from(world.difficulty())));
        self.set_base_damage(
            f64::from(power * 2.0)
                + triangle_random(
                    f64::from(difficulty) * MOB_DAMAGE_PER_DIFFICULTY,
                    MOB_DAMAGE_DEVIATION,
                ),
        );
    }

    /// Vanilla `setSoundEvent`.
    fn set_sound_event(&self, sound_event: SoundEventRef) {
        self.arrow_base().state.lock().sound_event = sound_event;
    }

    /// Vanilla `isCritArrow`.
    fn is_crit_arrow(&self) -> bool {
        self.arrow_flags() & FLAG_CRIT != 0
    }

    /// Vanilla `setCritArrow`.
    fn set_crit_arrow(&self, crit: bool) {
        set_arrow_flag(self, FLAG_CRIT, crit);
    }

    /// Vanilla `AbstractArrow.setNoPhysics`: also mirrors the flag to clients.
    fn set_arrow_no_physics(&self, no_physics: bool) {
        self.set_no_physics(no_physics);
        set_arrow_flag(self, FLAG_NO_PHYSICS, no_physics);
    }

    /// Vanilla constructor tail (`AbstractArrow(type, x, y, z, level, pickup,
    /// weapon)`): copies the ammo, marks intangible ammo creative-only, and
    /// takes the piercing level from the weapon.
    fn init_from_items(
        &self,
        pickup_item_stack: &ItemStack,
        fired_from_weapon: Option<&ItemStack>,
    ) {
        // VANILLA: the constructor stores a copy, then removes
        // `intangible_projectile` from the argument only, so the stored ammo
        // keeps the component.
        self.set_pickup_item_stack(pickup_item_stack.clone());
        if pickup_item_stack.has(INTANGIBLE_PROJECTILE) {
            self.set_pickup(ArrowPickup::CreativeOnly);
        }

        if let Some(weapon) = fired_from_weapon
            && !weapon.is_empty()
        {
            self.arrow_base().state.lock().fired_from_weapon = Some(weapon.clone());
            let pierce_level = enchantment_helper::get_piercing_count(weapon, pickup_item_stack);
            if pierce_level > 0 {
                self.set_pierce_level(i8::try_from(pierce_level).unwrap_or(i8::MAX));
            }
        }
    }

    /// Vanilla `AbstractArrow.setOwner` after `super.setOwner`: a player
    /// owner makes a disallowed arrow pickable.
    fn arrow_owner_changed(&self, owner: Option<&SharedEntity>) {
        if owner.is_some_and(|owner| owner.as_player().is_some())
            && self.pickup() == ArrowPickup::Disallowed
        {
            self.set_pickup(ArrowPickup::Allowed);
        }
    }

    /// Vanilla `AbstractArrow.shoot`: shooting resets the despawn timer.
    fn arrow_shoot(&self, direction: DVec3, power: f32, uncertainty: f32) {
        self.default_shoot(direction, power, uncertainty);
        self.arrow_base().state.lock().life = 0;
    }

    /// Vanilla `AbstractArrow.move`: an external push (piston, shulker)
    /// knocks a stuck arrow loose.
    fn arrow_after_move(&self, mover_type: MoverType) {
        if mover_type != MoverType::SelfMovement && self.should_fall() {
            self.start_falling();
        }
    }

    /// Vanilla `AbstractArrow.tick`, reached from the concrete arrow's tick.
    fn abstract_arrow_tick(self: Arc<Self>) {
        let Some(world) = self.level() else {
            return;
        };
        // Vanilla `Entity.setOldPosAndRot()` runs in the level before ticking.
        self.set_old_position_to_current();
        self.base().set_old_rotation_to_current();

        let physics_enabled = !self.no_physics();
        let movement = self.velocity();
        let block_pos = self.block_position();
        let block_state = world.get_block_state(block_pos);
        if !block_state.is_air() && physics_enabled {
            let shape = BLOCK_BEHAVIORS
                .get_behavior(block_state.get_block())
                .get_collision_shape(
                    block_state,
                    world.as_ref(),
                    block_pos,
                    BlockCollisionContext::empty(),
                );
            let position = self.position();
            if shape
                .boxes()
                .iter()
                .any(|aabb| aabb.at_block(block_pos).contains(position))
            {
                self.set_velocity(DVec3::ZERO);
                self.set_in_ground(true);
            }
        }

        {
            let mut state = self.arrow_base().state.lock();
            if state.shake_time > 0 {
                state.shake_time -= 1;
            }
        }

        if self.is_in_water_or_rain() {
            self.clear_fire();
        }

        if self.is_in_ground() && physics_enabled {
            let last_state = self.arrow_base().state.lock().last_state;
            if last_state != Some(block_state) && self.should_fall() {
                self.start_falling();
            } else {
                self.tick_despawn();
            }

            self.arrow_base().state.lock().in_ground_time += 1;
            if self.is_alive() {
                self.apply_effects_from_blocks();
            }

            if let Some(synced) = self.synced_data() {
                synced.set_base_on_fire_flag(self.remaining_fire_ticks() > 0);
            }
            return;
        }

        self.arrow_base().state.lock().in_ground_time = 0;
        let original_position = self.position();
        if self.is_in_water() {
            self.apply_arrow_inertia(self.get_water_inertia());
            // VANILLA CLIENT-LOCAL: `addBubbleParticles` goes through
            // `Level.addParticle`, which the server level ignores.
        }
        // VANILLA CLIENT-LOCAL: the crit particle trail is `Level.addParticle`.

        let y_rot = if physics_enabled {
            movement.x.atan2(movement.z).to_degrees() as f32
        } else {
            (-movement.x).atan2(-movement.z).to_degrees() as f32
        };
        let horizontal = movement.x.hypot(movement.z);
        let x_rot = movement.y.atan2(horizontal).to_degrees() as f32;
        let (yaw, pitch) = self.rotation();
        self.set_rotation((lerp_rotation(yaw, y_rot), lerp_rotation(pitch, x_rot)));
        self.check_left_owner();

        if physics_enabled {
            let block_hit = world.clip_including_border(
                original_position,
                original_position + movement,
                ClipBlockShape::Collider,
                ClipFluid::None,
            );
            Arc::clone(&self).step_move_and_hit(&block_hit);
        } else {
            if let Err(error) = self.try_set_position(original_position + movement) {
                log::debug!("failed to advance arrow {}: {error}", self.id());
                self.set_removed(RemovalReason::Discarded);
                return;
            }
            self.apply_effects_from_blocks();
        }

        if !self.is_in_water() {
            self.apply_arrow_inertia(self.get_arrow_air_drag());
        }

        if physics_enabled && !self.is_in_ground() {
            self.apply_gravity();
        }

        self.projectile_base_tick();
    }

    /// Vanilla `stepMoveAndHit`: walks the move vector, hitting every entity
    /// on the way (piercing arrows continue), then the block.
    fn step_move_and_hit(self: Arc<Self>, block_hit: &ClipHitResult) {
        while self.is_alive() {
            let initial_position = self.position();
            let mut entities_hit = self.find_hit_entities(initial_position, block_hit.location);
            entities_hit.sort_by(|a, b| {
                initial_position
                    .distance_squared(a.entity.position())
                    .total_cmp(&initial_position.distance_squared(b.entity.position()))
            });
            let next_location = entities_hit
                .first()
                .map_or(block_hit.location, |hit| hit.location);
            if let Err(error) = self.try_set_position(next_location) {
                log::debug!("failed to advance arrow {}: {error}", self.id());
                self.set_removed(RemovalReason::Discarded);
                return;
            }
            self.apply_effects_from_blocks_between(initial_position, next_location);
            if self.is_world_change_pending() {
                // Vanilla `handlePortal` runs here; Steel defers the
                // dimension change to the world, so stop stepping.
                return;
            }

            if entities_hit.is_empty() {
                if self.is_alive() && !block_hit.is_miss() {
                    Arc::clone(&self).hit_target_or_deflect_self(&ProjectileHit::Block {
                        location: block_hit.location,
                        hit: *block_hit,
                    });
                    self.mark_velocity_sync();
                }
                break;
            }
            if !self.is_alive() || self.no_physics() {
                break;
            }
            let deflection = Arc::clone(&self).hit_targets_or_deflect_self(entities_hit);
            self.mark_velocity_sync();
            if self.pierce_level() > 0 && deflection == ProjectileDeflection::None {
                continue;
            }
            break;
        }
    }

    /// Vanilla `hitTargetsOrDeflectSelf`.
    fn hit_targets_or_deflect_self(
        self: Arc<Self>,
        entity_hits: Vec<EntityHitResult>,
    ) -> ProjectileDeflection {
        for hit in entity_hits {
            let deflection =
                Arc::clone(&self).hit_target_or_deflect_self(&ProjectileHit::Entity(hit));
            if !self.is_alive() || deflection != ProjectileDeflection::None {
                return deflection;
            }
        }
        ProjectileDeflection::None
    }

    /// Vanilla `findHitEntities` (`ProjectileUtil.getManyEntityHitResult`).
    fn find_hit_entities(&self, from: DVec3, to: DVec3) -> Vec<EntityHitResult> {
        let Some(world) = self.level() else {
            return Vec::new();
        };
        let search_box = self
            .bounding_box()
            .expand_towards(self.velocity())
            .inflate(1.0);
        let self_id = self.id();
        get_many_entity_hit_result(
            &world,
            from,
            to,
            search_box,
            compute_margin(self.tick_count()),
            |entity| entity.id() != self_id && self.arrow_can_hit_entity(entity),
        )
    }

    /// Vanilla `AbstractArrow.canHitEntity`: also skips already pierced
    /// entities.
    // TODO: vanilla `Player.canHarmPlayer` rejects friendly fire between team
    // members; Steel has no scoreboard teams yet, so every player is harmable.
    fn arrow_can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.base_can_hit_entity(entity)
            && self
                .arrow_base()
                .state
                .lock()
                .piercing_ignore_entity_ids
                .as_ref()
                .is_none_or(|ids| !ids.contains(&entity.id()))
    }

    /// Vanilla `applyInertia`.
    fn apply_arrow_inertia(&self, inertia: f32) {
        self.set_velocity(self.velocity() * f64::from(inertia));
    }

    /// Vanilla `shouldFall`: nothing is left to hold the arrow.
    fn should_fall(&self) -> bool {
        let Some(world) = self.level() else {
            return false;
        };
        if !self.is_in_ground() {
            return false;
        }
        let position = self.position();
        let probe = WorldAabb::new(
            position.x, position.y, position.z, position.x, position.y, position.z,
        )
        .inflate(SHOULD_FALL_INFLATE);
        let collision_world = WorldCollisionProvider::new(&world);
        !collision_world.has_block_collision_with_context(&probe, BlockCollisionContext::empty())
            && !collision_world.has_entity_collision(&probe)
    }

    /// Vanilla `startFalling`.
    fn start_falling(&self) {
        self.set_in_ground(false);
        let movement = self.velocity();
        self.set_velocity(DVec3::new(
            movement.x * f64::from(rand::random::<f32>() * 0.2),
            movement.y * f64::from(rand::random::<f32>() * 0.2),
            movement.z * f64::from(rand::random::<f32>() * 0.2),
        ));
        self.arrow_base().state.lock().life = 0;
    }

    /// Vanilla `tickDespawn`.
    fn tick_despawn(&self) {
        let life = {
            let mut state = self.arrow_base().state.lock();
            state.life += 1;
            state.life
        };
        if life >= DESPAWN_LIFE {
            self.set_removed(RemovalReason::Discarded);
        }
    }

    /// Vanilla `AbstractArrow.onHitEntity`.
    fn arrow_on_hit_entity(self: Arc<Self>, entity: &SharedEntity)
    where
        Self: Sized + 'static,
    {
        let Some(world) = self.level() else {
            return;
        };
        let pow = self.velocity().length() as f32;
        let mut arrow_damage = self.base_damage();
        let current_owner = self.get_owner();
        let this: SharedEntity = Arc::<Self>::clone(&self);
        let damage_source = DamageSource::environment(&vanilla_damage_types::ARROW)
            .with_direct_entity(Arc::clone(&this))
            .with_causing_entity(current_owner.clone().unwrap_or_else(|| Arc::clone(&this)));
        let weapon = self.get_weapon_item();
        if let Some(weapon) = &weapon {
            arrow_damage = f64::from(enchantment_helper::modify_damage(
                weapon,
                &EnchantmentDamageContext::from_damage_source(entity.entity_type(), &damage_source),
                arrow_damage as f32,
            ));
        }

        let mut damage = (f64::from(pow) * arrow_damage)
            .clamp(0.0, 2.147_483_647e9)
            .ceil() as i32;
        let pierce_level = self.pierce_level();
        if pierce_level > 0 {
            let mut state = self.arrow_base().state.lock();
            let ids = state.piercing_ignore_entity_ids.get_or_insert_with(|| {
                FxHashSet::with_capacity_and_hasher(5, rustc_hash::FxBuildHasher)
            });
            if ids.len() > usize::from(pierce_level.unsigned_abs()) {
                drop(state);
                self.set_removed(RemovalReason::Discarded);
                return;
            }
            ids.insert(entity.id());
        }

        if self.is_crit_arrow() {
            let damage_increase = i64::from(rand::random_range(0..damage / 2 + 2));
            damage = (damage_increase + i64::from(damage)).min(i64::from(i32::MAX)) as i32;
        }

        if let Some(living_owner) = current_owner
            .as_ref()
            .and_then(|owner| owner.as_living_entity())
        {
            living_owner.set_last_hurt_mob(Some(entity));
        }

        let remaining_fire_ticks = entity.remaining_fire_ticks();
        if self.is_on_fire() {
            entity.ignite_for_ticks(BURNING_ARROW_IGNITE_TICKS);
        }

        if entity.hurt(&world, &damage_source, damage as f32) {
            if let Some(mob) = entity.as_living_entity() {
                if pierce_level <= 0 {
                    mob.set_arrow_count(mob.arrow_count() + 1);
                }

                self.do_knockback(mob, &damage_source);
                enchantment_helper::do_post_attack_effects_with_item_source(
                    world.as_ref(),
                    entity.as_ref(),
                    &weapon.unwrap_or_else(ItemStack::empty),
                    &EnchantmentPostAttackContext::new(entity, &damage_source),
                );

                self.do_post_hurt_effects(mob);
                if mob.as_player().is_some()
                    && let Some(owner) = &current_owner
                    && let Some(owner_player) = owner.as_player()
                    && !self.is_silent()
                    && owner.id() != entity.id()
                {
                    owner_player.send_packet(CGameEvent {
                        event: GameEventType::PlayArrowHitSound,
                        data: 0.0,
                    });
                }
                // TODO: vanilla triggers `CriteriaTriggers.KILLED_BY_ARROW`
                // here; Steel has no advancement criteria yet.
            }

            let sound = self.arrow_base().state.lock().sound_event;
            self.play_sound(sound, 1.0, 1.2 / (rand::random::<f32>() * 0.2 + 0.9));
            if pierce_level <= 0 {
                self.set_removed(RemovalReason::Discarded);
            }
        } else if entity.projectile_receives_side_effects_on_hit(false) {
            entity.set_remaining_fire_ticks(remaining_fire_ticks);
            let owner = self.owner_reference();
            self.deflect_with_power(
                ProjectileDeflection::Reverse,
                Some(entity.as_ref()),
                owner,
                false,
                BOUNCE_DEFLECT_POWER,
            );
            if self.velocity().length_squared() < 1.0e-7 {
                if self.pickup() == ArrowPickup::Allowed {
                    let _ = self.spawn_at_location(self.get_pickup_item(), 0.1);
                }
                self.set_removed(RemovalReason::Discarded);
            }
        }
    }

    /// Vanilla `doKnockback`: the weapon's knockback enchantments (Punch).
    fn do_knockback(&self, mob: &dyn LivingEntity, damage_source: &DamageSource) {
        let Some(weapon) = self.get_weapon_item() else {
            return;
        };
        let knockback = f64::from(enchantment_helper::modify_knockback(
            &weapon,
            &EnchantmentDamageContext::from_damage_source(mob.entity_type(), damage_source),
            0.0,
        ));
        if knockback > 0.0 {
            let knockback_resistance = (1.0 - mob.knockback_resistance()).max(0.0);
            let movement = self.velocity();
            let movement = DVec3::new(movement.x, 0.0, movement.z).normalize_or_zero()
                * (knockback * KNOCKBACK_SCALE * knockback_resistance);
            if movement.length_squared() > 0.0 {
                mob.push_impulse(DVec3::new(movement.x, KNOCKBACK_UP, movement.z));
            }
        }
    }

    /// Vanilla `AbstractArrow.onHitBlock`.
    fn arrow_on_hit_block(&self, hit: &ClipHitResult) {
        let Some(world) = self.level() else {
            return;
        };
        self.arrow_base().state.lock().last_state = Some(world.get_block_state(hit.block_pos));
        self.projectile_on_hit_block(hit);
        // VANILLA: `hitBlockEnchantmentEffects` runs the weapon's
        // `minecraft:hit_block` effects; only the trident's Channeling uses
        // that component, and it is ported with the trident.

        let movement = self.velocity();
        let offset = DVec3::new(
            movement.x.signum(),
            movement.y.signum(),
            movement.z.signum(),
        ) * f64::from(HIT_BLOCK_PULL_BACK as f32);
        if let Err(error) = self.try_set_position(self.position() - offset) {
            log::debug!("failed to settle arrow {}: {error}", self.id());
        }
        self.set_velocity(DVec3::ZERO);
        let sound = self.arrow_base().state.lock().sound_event;
        self.play_sound(sound, 1.0, 1.2 / (rand::random::<f32>() * 0.2 + 0.9));
        self.set_in_ground(true);
        let mut state = self.arrow_base().state.lock();
        state.shake_time = SHAKE_TIME;
        drop(state);
        self.set_crit_arrow(false);
        self.set_pierce_level(0);
        let mut state = self.arrow_base().state.lock();
        state.sound_event = &sound_events::ENTITY_ARROW_HIT;
        state.piercing_ignore_entity_ids = None;
    }

    /// Vanilla `AbstractArrow.playerTouch`.
    fn arrow_player_touch(&self, player: &Arc<Player>) {
        if (self.is_in_ground() || self.no_physics())
            && self.shake_time() <= 0
            && self.try_pickup(player)
        {
            // Vanilla `Player.take`.
            if let Some(world) = self.level() {
                world.broadcast_to_nearby(
                    ChunkPos::from_entity_pos(self.position()),
                    CTakeItemEntity::new(self.id(), player.id(), 1),
                    None,
                );
            }
            self.set_removed(RemovalReason::Discarded);
        }
    }

    /// Vanilla `tryPickup`.
    fn try_pickup(&self, player: &Arc<Player>) -> bool {
        match self.pickup() {
            ArrowPickup::Disallowed => false,
            ArrowPickup::Allowed => {
                let mut item = self.get_pickup_item();
                let count = item.count();
                player.inventory.lock().add(&mut item);
                item.count() < count
            }
            ArrowPickup::CreativeOnly => player.has_infinite_materials(),
        }
    }

    /// Saves vanilla `AbstractArrow` fields.
    fn save_arrow(&self, nbt: &mut NbtCompound) {
        self.save_projectile(nbt);
        let state = self.arrow_base().state.lock();
        nbt.insert("life", state.life as i16);
        if let Some(last_state) = state.last_state {
            nbt.insert(
                "inBlockState",
                NbtTag::Compound(block_state_nbt::save(last_state)),
            );
        }
        nbt.insert("shake", state.shake_time as i8);
        nbt.insert("inGround", i8::from(self.is_in_ground()));
        nbt.insert("pickup", state.pickup.ordinal());
        nbt.insert("damage", state.base_damage);
        nbt.insert("crit", i8::from(self.is_crit_arrow()));
        nbt.insert("PierceLevel", self.pierce_level());
        nbt.insert("SoundEvent", state.sound_event.key.to_string());
        nbt.insert("item", state.pickup_item_stack.clone().to_nbt_tag());
        if let Some(weapon) = &state.fired_from_weapon {
            nbt.insert("weapon", weapon.clone().to_nbt_tag());
        }
    }

    /// Loads vanilla `AbstractArrow` fields.
    fn load_arrow(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_projectile(nbt);
        let sound_event = nbt
            .string("SoundEvent")
            .and_then(|value| Identifier::from_str(&value.to_str()).ok())
            .and_then(|id| REGISTRY.sound_events.by_key(&id))
            .unwrap_or(&sound_events::ENTITY_ARROW_HIT);
        {
            let mut state = self.arrow_base().state.lock();
            state.life = i32::from(nbt.short("life").unwrap_or(0));
            state.last_state = nbt.compound("inBlockState").and_then(block_state_nbt::load);
            state.shake_time = i32::from(nbt.byte("shake").unwrap_or(0) as u8);
            state.base_damage = nbt.double("damage").unwrap_or(ARROW_BASE_DAMAGE);
            state.pickup = ArrowPickup::by_ordinal(nbt.byte("pickup").unwrap_or(0));
            state.sound_event = sound_event;
            state.fired_from_weapon = nbt
                .compound("weapon")
                .and_then(|tag| ItemStack::from_borrowed_compound(&tag));
        }
        self.set_in_ground(nbt.byte("inGround").is_some_and(|value| value != 0));
        self.set_crit_arrow(nbt.byte("crit").is_some_and(|value| value != 0));
        self.set_pierce_level(nbt.byte("PierceLevel").unwrap_or(0));
        let item = nbt
            .compound("item")
            .and_then(|tag| ItemStack::from_borrowed_compound(&tag))
            .unwrap_or_else(|| self.get_default_pickup_item());
        self.set_pickup_item_stack(item);
    }
}

fn set_arrow_flag<A: AbstractArrow + ?Sized>(arrow: &A, flag: i8, value: bool) {
    let flags = arrow.arrow_flags();
    arrow.set_arrow_flags(if value { flags | flag } else { flags & !flag });
}

/// Vanilla `ProjectileUtil.getManyEntityHitResult` with `includeFromEntity =
/// false` and `projectSurfaceHitLocation = false` (the arrow overload): every
/// entity the segment enters, with misses inside the margin snapped onto
/// the margin box when an unobstructed line reaches the entity.
pub(super) fn get_many_entity_hit_result(
    world: &Arc<World>,
    from: DVec3,
    to: DVec3,
    search_box: WorldAabb,
    margin: f64,
    matching: impl Fn(&dyn Entity) -> bool,
) -> Vec<EntityHitResult> {
    let mut collector = Vec::new();
    for entity in world.get_entities_in_aabb(&search_box) {
        if !matching(entity.as_ref()) {
            continue;
        }
        let entity_box = entity.bounding_box();
        if let Some(location) = clip_segment(entity_box, from, to) {
            collector.push(EntityHitResult { entity, location });
            continue;
        }
        if margin <= 0.0 {
            continue;
        }
        let Some(outside_hit) = clip_segment(entity_box.inflate(margin), from, to) else {
            continue;
        };
        let mut towards_target = entity_box.center();
        let block_hit = world.clip_including_border(
            outside_hit,
            towards_target,
            ClipBlockShape::Collider,
            ClipFluid::None,
        );
        if !block_hit.is_miss() {
            towards_target = block_hit.location;
        }
        if clip_segment(entity.bounding_box(), outside_hit, towards_target).is_some() {
            collector.push(EntityHitResult {
                entity,
                location: outside_hit,
            });
        }
    }
    collector
}
