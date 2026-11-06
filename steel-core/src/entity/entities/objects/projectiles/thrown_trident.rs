//! Thrown trident entity (vanilla `ThrownTrident`).
//!
//! Extends vanilla `AbstractArrow`, ported in
//! [`crate::entity::projectile::AbstractArrow`]. A trident hits at most one
//! entity, bounces back off it, and with Loyalty flies home to its owner.

use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_registry::data_components::vanilla_components::ENCHANTMENT_GLINT_OVERRIDE;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_entity_data::TridentEntityData;
use steel_registry::{sound_events, vanilla_damage_types, vanilla_entities, vanilla_items};
use steel_utils::locks::SyncMutex;
use steel_utils::{BlockPos, DowncastType, DowncastTypeKey};

use crate::enchantment_helper::{self, EnchantmentDamageContext, EnchantmentPostAttackContext};
use crate::entity::damage::DamageSource;
use crate::entity::projectile::{ProjectileDeflection, compute_margin, get_entity_hit_result};
use crate::entity::{
    ARROW_GRAVITY, AbstractArrow, ArrowBase, ArrowPickup, Entity, EntityBase, EntityBaseLoad,
    EntityHitResult, EntitySyncedData, Projectile, ProjectileBase, RemovalReason, SharedEntity,
    next_entity_id,
};
use crate::inventory::container::Container as _;
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::world::{ClipHitResult, World};

/// Vanilla `ThrownTrident.onHitEntity` base damage.
const TRIDENT_DAMAGE: f32 = 8.0;
/// Vanilla `ThrownTrident.WATER_INERTIA`.
const TRIDENT_WATER_INERTIA: f32 = 0.99;
/// Vanilla `ThrownTrident.onHitEntity` bounce-back power.
const HIT_ENTITY_DEFLECT_POWER: DVec3 = DVec3::new(0.02, 0.2, 0.02);

/// A thrown trident.
#[entity_behavior(class = "ThrownTrident")]
pub struct ThrownTridentEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<TridentEntityData>,
    projectile_base: ProjectileBase,
    arrow_base: ArrowBase,
    state: SyncMutex<TridentState>,
}

#[derive(Debug, Default)]
struct TridentState {
    /// Vanilla `dealtDamage`.
    dealt_damage: bool,
    /// Vanilla `clientSideReturnTridentTickCount`, which the server also
    /// counts to play the return sound once.
    return_tick_count: i32,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ThrownTridentEntity`.
unsafe impl DowncastType for ThrownTridentEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/trident");
}

impl ThrownTridentEntity {
    /// Creates a trident with no owner carrying a plain trident item.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Creates a trident from saved base data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let trident = Self {
            base,
            entity_type,
            entity_data: SyncMutex::new(TridentEntityData::new()),
            projectile_base: ProjectileBase::new(),
            arrow_base: ArrowBase::new(ItemStack::new(&vanilla_items::TRIDENT)),
            state: SyncMutex::new(TridentState::default()),
        };
        trident.set_sound_event(&sound_events::ITEM_TRIDENT_HIT_GROUND);
        trident
    }

    /// Vanilla `ThrownTrident(level, owner, tridentItem)`: thrown from the
    /// owner's eyes, without a separate weapon (the trident is its own).
    #[must_use]
    pub fn thrown_by(world: &Arc<World>, owner: &SharedEntity, trident_item: &ItemStack) -> Self {
        let position = owner.position();
        let position = DVec3::new(position.x, owner.get_eye_y() - 0.1, position.z);
        let trident = Self::new(
            &vanilla_entities::TRIDENT,
            next_entity_id(),
            position,
            Arc::downgrade(world),
        );
        trident.init_from_items(trident_item, None);
        trident.set_owner_entity(Some(owner));
        trident.set_loyalty(loyalty_from_item(trident_item, &trident));
        trident
            .entity_data
            .lock()
            .thrown_trident
            .id_foil
            .set(trident_has_foil(trident_item));
        trident
    }

    /// Vanilla `ID_LOYALTY`.
    #[must_use]
    pub fn loyalty(&self) -> i8 {
        *self.entity_data.lock().thrown_trident.id_loyalty.get()
    }

    fn set_loyalty(&self, loyalty: i8) {
        self.entity_data
            .lock()
            .thrown_trident
            .id_loyalty
            .set(loyalty);
    }

    /// Vanilla `isFoil`.
    #[must_use]
    pub fn is_foil(&self) -> bool {
        *self.entity_data.lock().thrown_trident.id_foil.get()
    }

    /// Vanilla `dealtDamage`.
    #[must_use]
    pub fn dealt_damage(&self) -> bool {
        self.state.lock().dealt_damage
    }

    /// Vanilla `ThrownTrident` return-owner check: a living owner that is not a spectator.
    fn is_acceptable_return_owner(owner: &SharedEntity) -> bool {
        owner.is_alive()
            && owner
                .as_player()
                .is_none_or(|player| !player.is_spectator())
    }

    /// The Loyalty part of vanilla `ThrownTrident.tick`, before the arrow
    /// tick. Returns `false` when the trident was discarded.
    fn tick_loyalty(&self) -> bool {
        if self.in_ground_time() > 4 {
            self.state.lock().dealt_damage = true;
        }

        let loyalty = self.loyalty();
        let Some(owner) = self.get_owner() else {
            return true;
        };
        if loyalty <= 0 || !(self.dealt_damage() || self.no_physics()) {
            return true;
        }

        if !Self::is_acceptable_return_owner(&owner) {
            if self.pickup() == ArrowPickup::Allowed {
                let _ = self.spawn_at_location(self.get_pickup_item(), 0.1);
            }
            self.set_removed(RemovalReason::Discarded);
            return false;
        }

        let position = self.position();
        let owner_position = owner.position();
        let owner_eye = DVec3::new(owner_position.x, owner.get_eye_y(), owner_position.z);
        if owner.as_player().is_none()
            && position.distance(owner_eye) < owner.bounding_box().width() + 1.0
        {
            self.set_removed(RemovalReason::Discarded);
            return false;
        }

        self.set_arrow_no_physics(true);
        let to_owner = owner_eye - position;
        let loyalty = f64::from(loyalty);
        if let Err(error) = self.try_set_position(DVec3::new(
            position.x,
            position.y + to_owner.y * 0.015 * loyalty,
            position.z,
        )) {
            log::debug!("failed to pull trident {} home: {error}", self.id());
        }
        let acceleration = 0.05 * loyalty;
        self.set_velocity(self.velocity() * 0.95 + to_owner.normalize_or_zero() * acceleration);
        let mut state = self.state.lock();
        if state.return_tick_count == 0 {
            drop(state);
            self.play_sound(&sound_events::ITEM_TRIDENT_RETURN, 10.0, 1.0);
            state = self.state.lock();
        }
        state.return_tick_count += 1;
        true
    }

    /// Vanilla `ThrownTrident.onHitEntity`.
    fn trident_on_hit_entity(self: Arc<Self>, entity: &SharedEntity) {
        let Some(world) = self.level() else {
            return;
        };
        let owner = self.get_owner();
        let this: SharedEntity = Arc::<Self>::clone(&self);
        let damage_source = DamageSource::environment(&vanilla_damage_types::TRIDENT)
            .with_direct_entity(Arc::clone(&this))
            .with_causing_entity(owner.clone().unwrap_or_else(|| Arc::clone(&this)));
        let weapon = self.pickup_item_stack();
        let damage = enchantment_helper::modify_damage(
            &weapon,
            &EnchantmentDamageContext::from_damage_source(entity.entity_type(), &damage_source),
            TRIDENT_DAMAGE,
        );

        self.state.lock().dealt_damage = true;
        let was_hurt = entity.hurt(&world, &damage_source, damage);
        if was_hurt {
            // TODO: vanilla breaks the trident here (`kill`) when a post
            // attack effect uses up its durability; Steel's helper has no
            // break callback.
            enchantment_helper::do_post_attack_effects_with_item_source(
                world.as_ref(),
                entity.as_ref(),
                &weapon,
                &EnchantmentPostAttackContext::new(entity, &damage_source),
            );
            if let Some(mob) = entity.as_living_entity() {
                self.do_knockback(mob, &damage_source);
                self.do_post_hurt_effects(mob);
            }
        }

        if entity.projectile_receives_side_effects_on_hit(was_hurt) {
            self.play_sound(&sound_events::ITEM_TRIDENT_HIT, 1.0, 1.0);
            let owner = self.owner_reference();
            self.deflect_with_power(
                ProjectileDeflection::Reverse,
                Some(entity.as_ref()),
                owner,
                false,
                HIT_ENTITY_DEFLECT_POWER,
            );
        }
    }
}

/// Vanilla `ItemStack.hasFoil` for a trident: the glint override, else
/// whether it is enchanted (`TridentItem` keeps `Item.isFoil`).
fn trident_has_foil(trident_item: &ItemStack) -> bool {
    trident_item
        .get(ENCHANTMENT_GLINT_OVERRIDE)
        .copied()
        .unwrap_or_else(|| trident_item.is_enchanted())
}

/// Vanilla `ThrownTrident.getLoyaltyFromItem`
/// (`EnchantmentHelper.getTridentReturnToOwnerAcceleration`).
fn loyalty_from_item(trident_item: &ItemStack, trident: &dyn Entity) -> i8 {
    enchantment_helper::trident_return_to_owner_acceleration(trident_item, trident).clamp(0, 127)
        as i8
}

impl Entity for ThrownTridentEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn get_default_gravity(&self) -> f64 {
        ARROW_GRAVITY
    }

    fn spawn_data(&self) -> i32 {
        self.get_owner().map_or(0, |owner| owner.id())
    }

    fn restore_owner_reference(&self, owner: &SharedEntity) {
        self.cache_owner_entity(owner);
    }

    fn projectile_owner_uuid(&self) -> Option<uuid::Uuid> {
        self.owner_uuid()
    }

    fn projectile_owner(&self) -> Option<SharedEntity> {
        self.get_owner()
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    /// Vanilla `ThrownTrident.playerTouch`: only the owner (or anyone, for an
    /// ownerless trident) can pick it up.
    fn player_touch(self: Arc<Self>, player: &Arc<Player>) {
        if self.owned_by(player.as_ref()) || self.get_owner().is_none() {
            self.arrow_player_touch(player);
        }
    }

    fn push_impulse(&self, impulse: DVec3) {
        // Vanilla `AbstractArrow.push`: stuck arrows ignore pushes.
        if self.is_in_ground() || !impulse.is_finite() {
            return;
        }
        self.set_velocity(self.velocity() + impulse);
        self.mark_velocity_sync();
    }

    fn on_above_bubble_column(&self, drag_down: bool, pos: BlockPos) {
        if !self.is_in_ground() {
            self.on_above_bubble_column_projectile(drag_down, pos);
        }
    }

    fn on_inside_bubble_column(&self, drag_down: bool) {
        if !self.is_in_ground() {
            self.on_inside_bubble_column_projectile(drag_down);
        }
    }

    fn is_pushed_by_fluid(&self) -> bool {
        !self.is_in_ground()
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        let result = Arc::clone(&self).default_move_entity(mover_type, delta);
        self.arrow_after_move(mover_type);
        result
    }

    fn tick(self: Arc<Self>) {
        if self.tick_loyalty() {
            self.abstract_arrow_tick();
        }
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_arrow(nbt);
        nbt.insert("DealtDamage", i8::from(self.dealt_damage()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_arrow(nbt);
        if nbt.string("SoundEvent").is_none() {
            self.set_sound_event(&sound_events::ITEM_TRIDENT_HIT_GROUND);
        }
        self.state.lock().dealt_damage = nbt.byte("DealtDamage").is_some_and(|value| value != 0);
        let loyalty = loyalty_from_item(&self.pickup_item_stack(), self);
        self.set_loyalty(loyalty);
    }
}

impl Projectile for ThrownTridentEntity {
    fn projectile_base(&self) -> &ProjectileBase {
        &self.projectile_base
    }

    fn shoot(&self, direction: DVec3, power: f32, uncertainty: f32) {
        self.arrow_shoot(direction, power, uncertainty);
    }

    fn set_owner_entity(&self, owner: Option<&SharedEntity>) {
        self.projectile_base_set_owner_entity(owner);
        self.arrow_owner_changed(owner);
    }

    fn can_hit_entity(&self, entity: &dyn Entity) -> bool {
        self.arrow_can_hit_entity(entity)
    }

    fn should_bounce_on_world_border(&self) -> bool {
        true
    }

    fn on_hit_entity(self: Arc<Self>, entity: &SharedEntity, _location: DVec3) {
        self.trident_on_hit_entity(entity);
    }

    fn on_hit_block(self: Arc<Self>, hit: &ClipHitResult) {
        // TODO: vanilla `ThrownTrident.hitBlockEnchantmentEffects` runs the
        // `minecraft:hit_block` effects (Channeling's lightning); Steel has
        // no hit-block enchantment effects yet.
        self.arrow_on_hit_block(hit);
    }
}

impl AbstractArrow for ThrownTridentEntity {
    fn arrow_base(&self) -> &ArrowBase {
        &self.arrow_base
    }

    fn arrow_flags(&self) -> i8 {
        *self
            .entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .id_flags
            .get()
    }

    fn set_arrow_flags(&self, flags: i8) {
        self.entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .id_flags
            .set(flags);
    }

    fn pierce_level(&self) -> i8 {
        *self
            .entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .pierce_level
            .get()
    }

    fn set_pierce_level(&self, pierce_level: i8) {
        self.entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .pierce_level
            .set(pierce_level);
    }

    fn is_in_ground(&self) -> bool {
        *self
            .entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .in_ground
            .get()
    }

    fn set_in_ground(&self, in_ground: bool) {
        self.entity_data
            .lock()
            .thrown_trident
            .abstract_arrow
            .in_ground
            .set(in_ground);
    }

    fn get_default_pickup_item(&self) -> ItemStack {
        ItemStack::new(&vanilla_items::TRIDENT)
    }

    fn get_water_inertia(&self) -> f32 {
        TRIDENT_WATER_INERTIA
    }

    /// Vanilla `ThrownTrident.getWeaponItem`: the trident itself.
    fn get_weapon_item(&self) -> Option<ItemStack> {
        Some(self.pickup_item_stack())
    }

    /// Vanilla `ThrownTrident.findHitEntities`: at most one entity, and none
    /// once the trident dealt damage.
    fn find_hit_entities(&self, from: DVec3, to: DVec3) -> Vec<EntityHitResult> {
        if self.dealt_damage() {
            return Vec::new();
        }
        let Some(world) = self.level() else {
            return Vec::new();
        };
        let search_box = self
            .bounding_box()
            .expand_towards(self.velocity())
            .inflate(1.0);
        let self_id = self.id();
        get_entity_hit_result(
            &world,
            from,
            to,
            search_box,
            compute_margin(self.tick_count()),
            |entity| entity.id() != self_id && self.arrow_can_hit_entity(entity),
        )
        .into_iter()
        .collect()
    }

    /// Vanilla `ThrownTrident.tickDespawn`: a loyal trident a player can pick
    /// up never despawns.
    fn tick_despawn(&self) {
        if self.pickup() != ArrowPickup::Allowed || self.loyalty() <= 0 {
            self.default_tick_despawn();
        }
    }

    /// Vanilla `ThrownTrident.tryPickup`: a returning trident also goes back
    /// into its owner's inventory.
    fn try_pickup(&self, player: &Arc<Player>) -> bool {
        if self.default_try_pickup(player) {
            return true;
        }
        if !self.no_physics() || !self.owned_by(player.as_ref()) {
            return false;
        }
        let mut item = self.get_pickup_item();
        let count = item.count();
        player.inventory.lock().add(&mut item);
        item.count() < count
    }
}

#[cfg(test)]
mod tests;
