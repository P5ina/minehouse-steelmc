//! The TNT minecart (vanilla `MinecartTNT`).

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::RngExt as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::fluid::FluidState;
use steel_registry::items::ItemRef;
use steel_registry::sound_events;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::{AbstractMinecartEntityData, TntMinecartEntityData};
use steel_registry::vanilla_game_rules::TNT_EXPLODES;
use steel_registry::vanilla_items;
use steel_utils::axis::Axis;
use steel_utils::block_util::FoundRectangle;
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, BlockStateId, Direction, DowncastType, DowncastTypeKey};

use super::abstract_minecart::{AbstractMinecart, MinecartCore, tick_minecart};
use crate::entity::damage::DamageSource;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityMovementEmission, EntitySyncedData, MoveResult,
    RemovalReason, reset_forward_direction_of_relative_portal_position,
};
use crate::physics::MoverType;
use crate::portal::portal_shape::PortalShape;
use crate::world::explosion::explosion_damage_source;
use crate::world::{Explosion, ExplosionInteraction, ExplosionOptions, World};
use steel_protocol::packets::game::SoundSource;

/// Vanilla `MinecartTNT.DEFAULT_EXPLOSION_POWER_BASE`.
const DEFAULT_EXPLOSION_POWER_BASE: f32 = 4.0;
/// Vanilla `MinecartTNT.DEFAULT_EXPLOSION_SPEED_FACTOR`.
const DEFAULT_EXPLOSION_SPEED_FACTOR: f32 = 1.0;
/// The fuse of a cart that is not primed.
const NO_FUSE: i32 = -1;
/// Ticks between priming and the explosion.
const PRIMED_FUSE: i32 = 80;
/// Vanilla compares squared speeds against the float `0.01F`, widened to a double.
const IGNITING_SPEED_SQR: f64 = 0.01_f32 as f64;

struct TntState {
    ignition_source: Option<DamageSource>,
    fuse: i32,
    explosion_power_base: f32,
    explosion_speed_factor: f32,
}

impl TntState {
    const fn new() -> Self {
        Self {
            ignition_source: None,
            fuse: NO_FUSE,
            explosion_power_base: DEFAULT_EXPLOSION_POWER_BASE,
            explosion_speed_factor: DEFAULT_EXPLOSION_SPEED_FACTOR,
        }
    }
}

/// A minecart carrying TNT that explodes when primed, burnt or crashed.
#[entity_behavior(class = "MinecartTNT")]
pub struct TntMinecartEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<TntMinecartEntityData>,
    minecart: MinecartCore,
    tnt: SyncMutex<TntState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `TntMinecartEntity`.
unsafe impl DowncastType for TntMinecartEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/tnt_minecart");
}

impl TntMinecartEntity {
    /// Creates a new TNT minecart.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a TNT minecart from saved data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(TntMinecartEntityData::new()),
            minecart: MinecartCore::new(),
            tnt: SyncMutex::new(TntState::new()),
        }
    }

    /// Vanilla `getFuse`: ticks until the explosion, or -1 when not primed.
    #[must_use]
    pub fn fuse(&self) -> i32 {
        self.tnt.lock().fuse
    }

    /// Vanilla `isPrimed`.
    #[must_use]
    pub fn is_primed(&self) -> bool {
        self.fuse() > NO_FUSE
    }

    /// Vanilla `MinecartTNT.primeFuse`: lights the fuse, unless TNT cannot
    /// explode in this world.
    pub fn prime_fuse(&self, source: Option<&DamageSource>) {
        let Some(world) = self.level() else {
            return;
        };
        if !world.get_game_rule(&TNT_EXPLODES) {
            return;
        }
        {
            let mut tnt = self.tnt.lock();
            tnt.fuse = PRIMED_FUSE;
            if let Some(source) = source
                && tnt.ignition_source.is_none()
            {
                tnt.ignition_source = Some(explosion_damage_source(
                    world.get_entity_by_id(self.id()),
                    source.causing_entity().cloned(),
                ));
            }
        }
        self.broadcast_entity_event(EntityStatus::TntPrime);
        if !self.is_silent() {
            world.play_sound_at(
                &sound_events::ENTITY_TNT_PRIMED,
                SoundSource::Blocks,
                self.position(),
                1.0,
                1.0,
                None,
            );
        }
    }

    /// Vanilla `MinecartTNT.explode`: a blast that grows with the cart's speed.
    fn explode(&self, damage_source: Option<DamageSource>, speed_sqr: f64) {
        let Some(world) = self.level() else {
            return;
        };
        if world.get_game_rule(&TNT_EXPLODES) {
            let speed = speed_sqr.sqrt().min(5.0);
            let (power_base, speed_factor) = {
                let tnt = self.tnt.lock();
                (tnt.explosion_power_base, tnt.explosion_speed_factor)
            };
            #[expect(
                clippy::cast_possible_truncation,
                reason = "vanilla narrows the explosion power to a float"
            )]
            let power = (f64::from(power_base)
                + f64::from(speed_factor) * rand::rng().random::<f64>() * 1.5 * speed)
                as f32;
            let mut options =
                ExplosionOptions::new(self.position(), power, ExplosionInteraction::Tnt);
            options.source = world.get_entity_by_id(self.id());
            options.damage_source = damage_source;
            world.explode(options);
            self.set_removed(RemovalReason::Discarded);
        } else if self.is_primed() {
            self.set_removed(RemovalReason::Discarded);
        }
    }

    fn ignition_source(&self) -> Option<DamageSource> {
        self.tnt.lock().ignition_source.clone()
    }

    /// Vanilla `MinecartTNT.damageSourceIgnitesTnt`.
    fn damage_source_ignites_tnt(source: &DamageSource) -> bool {
        match source.direct_entity() {
            Some(direct) if direct.as_projectile().is_some() => direct.is_on_fire(),
            _ => source.is(&DamageTypeTag::IS_FIRE) || source.is(&DamageTypeTag::IS_EXPLOSION),
        }
    }

    /// Whether a primed cart spares this block: vanilla keeps rails, and the
    /// blocks under them, from its own blast.
    fn spares_block(&self, world: &World, pos: BlockPos, state: BlockStateId) -> bool {
        self.is_primed()
            && (state.get_block().has_tag(&BlockTag::RAILS)
                || world
                    .get_block_state(pos.above())
                    .get_block()
                    .has_tag(&BlockTag::RAILS))
    }
}

impl AbstractMinecart for TntMinecartEntity {
    fn minecart_core(&self) -> &MinecartCore {
        &self.minecart
    }

    fn with_minecart_data(&self, f: &mut dyn FnMut(&mut AbstractMinecartEntityData)) {
        f(&mut self.entity_data.lock().abstract_minecart);
    }

    fn drop_item(&self) -> ItemRef {
        &vanilla_items::TNT_MINECART
    }

    /// Vanilla `MinecartTNT.activateMinecart`: a powered activator rail primes it.
    fn activate_minecart(&self, _world: &Arc<World>, _pos: BlockPos, powered: bool) {
        if powered && self.fuse() < 0 {
            self.prime_fuse(None);
        }
    }

    /// Vanilla `MinecartTNT.destroy`: a slow cart broken by hand drops its
    /// item; fire, explosions or speed light the fuse instead.
    fn destroy(&self, world: &World, source: &DamageSource) {
        let speed_sqr = horizontal_distance_sqr(self.velocity());
        if !Self::damage_source_ignites_tnt(source) && speed_sqr < IGNITING_SPEED_SQR {
            self.default_destroy(world);
        } else if self.fuse() < 0 {
            self.prime_fuse(Some(source));
            let fuse = {
                let mut rng = rand::rng();
                rng.random_range(0..20) + rng.random_range(0..20)
            };
            self.tnt.lock().fuse = fuse;
        }
    }

    fn should_source_destroy(&self, source: &DamageSource) -> bool {
        Self::damage_source_ignites_tnt(source)
    }
}

fn horizontal_distance_sqr(velocity: DVec3) -> f64 {
    velocity.x * velocity.x + velocity.z * velocity.z
}

impl Entity for TntMinecartEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `MinecartTNT.tick`: burns the fuse, and a hard crash sets the cart off.
    fn tick(self: Arc<Self>) {
        tick_minecart(&self);
        let fuse = {
            let mut tnt = self.tnt.lock();
            if tnt.fuse > 0 {
                tnt.fuse -= 1;
                None
            } else {
                Some(tnt.fuse)
            }
        };
        // The smoke over a burning fuse is drawn by the client from the prime event.
        if fuse == Some(0) {
            self.explode(
                self.ignition_source(),
                horizontal_distance_sqr(self.velocity()),
            );
        }

        if self.horizontal_collision() {
            let speed_sqr = horizontal_distance_sqr(self.velocity());
            if speed_sqr >= IGNITING_SPEED_SQR {
                self.explode(self.ignition_source(), speed_sqr);
            }
        }
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        let result = Arc::clone(&self).default_move_entity(mover_type, delta);
        self.apply_effects_from_blocks();
        result
    }

    fn apply_effects_from_blocks(&self) {
        let position = self.position();
        self.apply_effects_from_blocks_between(position, position);
        self.base().clear_movement_this_tick();
    }

    /// Vanilla `MinecartTNT.hurtServer`: a burning arrow sets the cart off at once.
    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if let Some(arrow) = source.direct_entity()
            && arrow.is_abstract_arrow()
            && arrow.is_on_fire()
        {
            let damage_source = explosion_damage_source(
                world.get_entity_by_id(self.id()),
                source.causing_entity().cloned(),
            );
            self.explode(Some(damage_source), arrow.velocity().length_squared());
        }
        self.hurt_vehicle(world, source, amount)
    }

    /// Vanilla `MinecartTNT.causeFallDamage`: a fall of three blocks or more
    /// sets the cart off.
    fn cause_fall_damage(
        self: Arc<Self>,
        fall_distance: f64,
        damage_modifier: f32,
        source: &DamageSource,
    ) -> bool {
        if fall_distance >= 3.0 {
            let power = fall_distance / 10.0;
            self.explode(self.ignition_source(), power * power);
        }
        if self.is_fall_damage_immune() {
            return false;
        }
        self.propagate_fall_to_passengers(fall_distance, damage_modifier, source);
        false
    }

    fn block_explosion_resistance(
        &self,
        _explosion: &dyn Explosion,
        world: &World,
        pos: BlockPos,
        state: BlockStateId,
        _fluid: FluidState,
        resistance: f32,
    ) -> f32 {
        if self.spares_block(world, pos, state) {
            0.0
        } else {
            resistance
        }
    }

    fn should_block_explode(
        &self,
        _explosion: &dyn Explosion,
        world: &World,
        pos: BlockPos,
        state: BlockStateId,
        _power: f32,
    ) -> bool {
        !self.spares_block(world, pos, state)
    }

    fn is_on_rails(&self) -> bool {
        self.minecart_on_rails()
    }

    fn movement_emission(&self) -> EntityMovementEmission {
        EntityMovementEmission::Events
    }

    fn is_pickable(&self) -> bool {
        !self.is_removed()
    }

    fn is_pushable(&self) -> bool {
        true
    }

    fn blocks_building(&self) -> bool {
        true
    }

    fn can_collide_with(&self, other: &dyn Entity) -> bool {
        (other.can_be_collided_with(Some(self)) || other.is_pushable())
            && !self.is_passenger_of_same_vehicle(other)
    }

    fn push_entity(&self, entity: &dyn Entity) {
        self.push_minecart_entity(entity);
    }

    fn passenger_attachment_point(&self, passenger: &dyn Entity) -> DVec3 {
        self.minecart_passenger_attachment_point(passenger)
            .unwrap_or_else(|| self.default_passenger_attachment_point(passenger))
    }

    fn dismount_location_for_passenger(&self, passenger: &dyn Entity) -> DVec3 {
        self.minecart_dismount_location(passenger)
    }

    fn motion_direction(&self) -> Direction {
        self.minecart_motion_direction()
    }

    fn known_movement(&self) -> DVec3 {
        let movement = self.velocity();
        if movement.is_nan() {
            return DVec3::ZERO;
        }
        DVec3::new(
            movement.x.clamp(-0.4, 0.4),
            movement.y,
            movement.z.clamp(-0.4, 0.4),
        )
    }

    fn block_speed_factor(&self) -> f32 {
        self.minecart_block_speed_factor()
    }

    fn get_default_gravity(&self) -> f64 {
        self.minecart_gravity()
    }

    fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
        self.vehicle_ignores_explosion(explosion)
    }

    fn dimension_changing_delay(&self) -> i32 {
        10
    }

    fn get_relative_portal_position(&self, axis: Axis, portal_area: FoundRectangle) -> DVec3 {
        reset_forward_direction_of_relative_portal_position(PortalShape::get_relative_position(
            portal_area,
            axis,
            self.position(),
            self.dimensions_for_pose(self.pose()),
        ))
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    /// Vanilla `MinecartTNT.addAdditionalSaveData`.
    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_minecart(nbt);
        let tnt = self.tnt.lock();
        nbt.insert("fuse", tnt.fuse);
        if tnt.explosion_power_base != DEFAULT_EXPLOSION_POWER_BASE {
            nbt.insert("explosion_power", tnt.explosion_power_base);
        }
        if tnt.explosion_speed_factor != DEFAULT_EXPLOSION_SPEED_FACTOR {
            nbt.insert("explosion_speed_factor", tnt.explosion_speed_factor);
        }
    }

    /// Vanilla `MinecartTNT.readAdditionalSaveData`.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_minecart(nbt);
        let mut tnt = self.tnt.lock();
        tnt.fuse = nbt.int("fuse").unwrap_or(NO_FUSE);
        tnt.explosion_power_base = nbt
            .float("explosion_power")
            .unwrap_or(DEFAULT_EXPLOSION_POWER_BASE)
            .clamp(0.0, 128.0);
        tnt.explosion_speed_factor = nbt
            .float("explosion_speed_factor")
            .unwrap_or(DEFAULT_EXPLOSION_SPEED_FACTOR)
            .clamp(0.0, 128.0);
    }
}
