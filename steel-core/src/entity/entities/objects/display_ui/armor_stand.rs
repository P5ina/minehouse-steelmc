//! Vanilla `ArmorStand`.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::{NbtCompound as BorrowedNbtCompoundView, NbtList as BorrowedNbtList};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_macros::entity_behavior;
use steel_registry::blocks::behavior::PushReaction;
use steel_registry::data_components::vanilla_components::CUSTOM_NAME;
use steel_registry::enchantment_effect::EnchantmentEffectComponent;
use steel_registry::entity_data::Rotations;
use steel_registry::entity_type::{EntityDimensions, EntityTypeRef};
use steel_registry::equipment::{EquipmentSlot, EquipmentSlotType};
use steel_registry::item_stack::ItemStack;
use steel_registry::particle_type::{BlockParticleOption, ParticleData};
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::ArmorStandEntityData;
use steel_registry::vanilla_game_rules::MOB_GRIEFING;
use steel_registry::{
    sound_events, vanilla_blocks, vanilla_game_events, vanilla_items, vanilla_particle_types,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{DowncastType, DowncastTypeKey};

use crate::behavior::InteractionResult;
use crate::entity::damage::DamageSource;
use crate::entity::entities::LightningBoltEntity;
use crate::entity::entities::objects::vehicles::AbstractMinecart;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityPose, EntitySyncedData, LivingEntity,
    LivingEntityBase, RemovalReason, SharedEntity, sync_dirty_mob_effects,
};
use crate::physics::MoveResult;
use crate::player::Player;
use crate::world::{Explosion, World};

/// Vanilla `ArmorStand.WOBBLE_TIME`: ticks after a hit in which another hit breaks the stand.
const WOBBLE_TIME: i64 = 5;
/// Vanilla `ArmorStand.DISABLE_TAKING_OFFSET`.
const DISABLE_TAKING_OFFSET: i32 = 8;
/// Vanilla `ArmorStand.DISABLE_PUTTING_OFFSET`.
const DISABLE_PUTTING_OFFSET: i32 = 16;
/// Vanilla `ArmorStand.CLIENT_FLAG_SMALL`.
const CLIENT_FLAG_SMALL: i8 = 1;
/// Vanilla `ArmorStand.CLIENT_FLAG_SHOW_ARMS`.
const CLIENT_FLAG_SHOW_ARMS: i8 = 4;
/// Vanilla `ArmorStand.CLIENT_FLAG_NO_BASEPLATE`.
const CLIENT_FLAG_NO_BASEPLATE: i8 = 8;
/// Vanilla `ArmorStand.CLIENT_FLAG_MARKER`.
const CLIENT_FLAG_MARKER: i8 = 16;
/// Vanilla `ArmorStand.FEET_OFFSET`.
const FEET_OFFSET: f64 = 0.1;
/// Vanilla `ArmorStand.CHEST_OFFSET`.
const CHEST_OFFSET: f64 = 0.9;
/// Vanilla `ArmorStand.LEGS_OFFSET`.
const LEGS_OFFSET: f64 = 0.4;
/// Vanilla `ArmorStand.HEAD_OFFSET`.
const HEAD_OFFSET: f64 = 1.6;
/// Vanilla `ArmorStand.BABY_DIMENSIONS` eye height.
const SMALL_EYE_HEIGHT: f32 = 0.9875;
/// Squared distance within which a stand nudges a rideable minecart it overlaps.
const MINECART_PUSH_DISTANCE_SQR: f64 = 0.2;

/// Vanilla `ArmorStand.ArmorStandPose`: the six limb rotations, in degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ArmorStandPose {
    /// Head rotation.
    pub head: Rotations,
    /// Body rotation.
    pub body: Rotations,
    /// Left arm rotation.
    pub left_arm: Rotations,
    /// Right arm rotation.
    pub right_arm: Rotations,
    /// Left leg rotation.
    pub left_leg: Rotations,
    /// Right leg rotation.
    pub right_leg: Rotations,
}

impl ArmorStandPose {
    /// Vanilla `ArmorStandPose.DEFAULT`.
    pub const DEFAULT: Self = Self {
        head: Rotations::new(0.0, 0.0, 0.0),
        body: Rotations::new(0.0, 0.0, 0.0),
        left_arm: Rotations::new(-10.0, 0.0, -10.0),
        right_arm: Rotations::new(-15.0, 0.0, 10.0),
        left_leg: Rotations::new(-1.0, 0.0, -1.0),
        right_leg: Rotations::new(1.0, 0.0, 1.0),
    };

    /// The limbs paired with their vanilla `ArmorStandPose.CODEC` field names and defaults.
    const fn fields(&self) -> [(&'static str, Rotations, Rotations); 6] {
        [
            ("Head", self.head, Self::DEFAULT.head),
            ("Body", self.body, Self::DEFAULT.body),
            ("LeftArm", self.left_arm, Self::DEFAULT.left_arm),
            ("RightArm", self.right_arm, Self::DEFAULT.right_arm),
            ("LeftLeg", self.left_leg, Self::DEFAULT.left_leg),
            ("RightLeg", self.right_leg, Self::DEFAULT.right_leg),
        ]
    }

    /// Vanilla `ArmorStandPose.CODEC` encode: limbs left at their default are omitted.
    fn save(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        for (name, rotations, default) in self.fields() {
            if !same_rotations(rotations, default) {
                nbt.insert(
                    name,
                    NbtList::Float(vec![rotations.x, rotations.y, rotations.z]),
                );
            }
        }
        nbt
    }

    /// Vanilla `ArmorStandPose.CODEC` decode: missing limbs take their default, and a
    /// limb that is not a list of three numbers rejects the whole pose.
    fn load(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let limb = |name: &str, default: Rotations| match nbt.get(name) {
            None => Some(default),
            Some(tag) => read_rotations(&tag.list()?),
        };
        Some(Self {
            head: limb("Head", Self::DEFAULT.head)?,
            body: limb("Body", Self::DEFAULT.body)?,
            left_arm: limb("LeftArm", Self::DEFAULT.left_arm)?,
            right_arm: limb("RightArm", Self::DEFAULT.right_arm)?,
            left_leg: limb("LeftLeg", Self::DEFAULT.left_leg)?,
            right_leg: limb("RightLeg", Self::DEFAULT.right_leg)?,
        })
    }
}

/// Java record equality for `Rotations`: `Float.compare` on every angle.
const fn same_rotations(a: Rotations, b: Rotations) -> bool {
    a.x.to_bits() == b.x.to_bits()
        && a.y.to_bits() == b.y.to_bits()
        && a.z.to_bits() == b.z.to_bits()
}

/// Vanilla `Rotations.CODEC`: exactly three numbers.
fn read_rotations(list: &BorrowedNbtList<'_, '_>) -> Option<Rotations> {
    let values: Vec<f32> = if let Some(floats) = list.floats() {
        floats
    } else if let Some(doubles) = list.doubles() {
        doubles.into_iter().map(|value| value as f32).collect()
    } else {
        list.ints()?.into_iter().map(|value| value as f32).collect()
    };
    let [x, y, z] = values.as_slice() else {
        return None;
    };
    Some(Rotations::new(*x, *y, *z))
}

/// Vanilla `ArmorStand`.
#[entity_behavior(class = "ArmorStand")]
pub struct ArmorStandEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    entity_data: SyncMutex<ArmorStandEntityData>,
    /// Vanilla `ArmorStand.invisible`: kept apart from the shared flag so mob
    /// effects cannot change it.
    invisible: AtomicBool,
    /// Vanilla `ArmorStand.disabledSlots`.
    disabled_slots: AtomicI32,
    /// Vanilla `ArmorStand.lastHit`: game time of the last wobble.
    last_hit: AtomicI64,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ArmorStandEntity`.
unsafe impl DowncastType for ArmorStandEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/armor_stand");
}

impl ArmorStandEntity {
    /// Creates a new armor stand.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Loads a saved armor stand.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mut entity_data = ArmorStandEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        Self {
            base,
            entity_type,
            living_base,
            entity_data: SyncMutex::new(entity_data),
            invisible: AtomicBool::new(false),
            disabled_slots: AtomicI32::new(0),
            last_hit: AtomicI64::new(0),
        }
    }

    fn client_flags(&self) -> i8 {
        *self.entity_data.lock().client_flags.get()
    }

    /// Vanilla `setSmall`/`setShowArms`/`setNoBasePlate`/`setMarker` and the
    /// `onSyncedDataUpdated` refresh that follows a flag change.
    fn set_client_flag(&self, flag: i8, value: bool) {
        let flags = self.client_flags();
        let updated = if value { flags | flag } else { flags & !flag };
        if updated == flags {
            return;
        }
        self.entity_data.lock().client_flags.set(updated);
        self.refresh_dimensions();
    }

    /// Vanilla `isSmall`.
    #[must_use]
    pub fn is_small(&self) -> bool {
        self.client_flags() & CLIENT_FLAG_SMALL != 0
    }

    /// Vanilla `setSmall`.
    pub fn set_small(&self, small: bool) {
        self.set_client_flag(CLIENT_FLAG_SMALL, small);
    }

    /// Vanilla `showArms`.
    #[must_use]
    pub fn show_arms(&self) -> bool {
        self.client_flags() & CLIENT_FLAG_SHOW_ARMS != 0
    }

    /// Vanilla `setShowArms`.
    pub fn set_show_arms(&self, show_arms: bool) {
        self.set_client_flag(CLIENT_FLAG_SHOW_ARMS, show_arms);
    }

    /// Vanilla `showBasePlate`.
    #[must_use]
    pub fn show_base_plate(&self) -> bool {
        self.client_flags() & CLIENT_FLAG_NO_BASEPLATE == 0
    }

    /// Vanilla `setNoBasePlate`.
    pub fn set_no_base_plate(&self, no_base_plate: bool) {
        self.set_client_flag(CLIENT_FLAG_NO_BASEPLATE, no_base_plate);
    }

    /// Vanilla `isMarker`: a marker has no size, physics or hitbox.
    #[must_use]
    pub fn is_marker(&self) -> bool {
        self.client_flags() & CLIENT_FLAG_MARKER != 0
    }

    /// Vanilla `setMarker`.
    pub fn set_marker(&self, marker: bool) {
        self.set_client_flag(CLIENT_FLAG_MARKER, marker);
    }

    /// Vanilla `ArmorStand.setInvisible`.
    pub fn set_invisible(&self, invisible: bool) {
        self.invisible.store(invisible, Ordering::Relaxed);
        self.entity_data.set_base_invisible_flag(invisible);
    }

    /// Vanilla `getArmorStandPose`.
    #[must_use]
    pub fn pose(&self) -> ArmorStandPose {
        let data = self.entity_data.lock();
        ArmorStandPose {
            head: *data.head_pose.get(),
            body: *data.body_pose.get(),
            left_arm: *data.left_arm_pose.get(),
            right_arm: *data.right_arm_pose.get(),
            left_leg: *data.left_leg_pose.get(),
            right_leg: *data.right_leg_pose.get(),
        }
    }

    /// Vanilla `setArmorStandPose`.
    pub fn set_pose(&self, pose: ArmorStandPose) {
        let mut data = self.entity_data.lock();
        data.head_pose.set(pose.head);
        data.body_pose.set(pose.body);
        data.left_arm_pose.set(pose.left_arm);
        data.right_arm_pose.set(pose.right_arm);
        data.left_leg_pose.set(pose.left_leg);
        data.right_leg_pose.set(pose.right_leg);
    }

    /// Vanilla `hasPhysics`.
    fn has_physics(&self) -> bool {
        !self.is_marker() && !self.base.no_gravity()
    }

    /// Vanilla `isDisabled`: the slot is locked against both taking and putting, or
    /// it is a hand of a stand without arms.
    fn is_disabled(&self, slot: EquipmentSlot) -> bool {
        self.disabled_slots.load(Ordering::Relaxed) & 1 << slot.filter_bit(0) != 0
            || slot.slot_type() == EquipmentSlotType::Hand && !self.show_arms()
    }

    /// Vanilla `getClickedSlot`: which worn item a click at `location` (relative to
    /// the stand's feet) aims at.
    fn clicked_slot(&self, location: DVec3) -> EquipmentSlot {
        let small = self.is_small();
        let y = location.y / f64::from(self.get_scale() * self.get_age_scale());
        if y >= FEET_OFFSET
            && y < FEET_OFFSET + if small { 0.8 } else { 0.45 }
            && self.has_item_in_slot(EquipmentSlot::Feet)
        {
            EquipmentSlot::Feet
        } else if y >= CHEST_OFFSET + if small { 0.3 } else { 0.0 }
            && y < CHEST_OFFSET + if small { 1.0 } else { 0.7 }
            && self.has_item_in_slot(EquipmentSlot::Chest)
        {
            EquipmentSlot::Chest
        } else if y >= LEGS_OFFSET
            && y < LEGS_OFFSET + if small { 1.0 } else { 0.8 }
            && self.has_item_in_slot(EquipmentSlot::Legs)
        {
            EquipmentSlot::Legs
        } else if y >= HEAD_OFFSET && self.has_item_in_slot(EquipmentSlot::Head) {
            EquipmentSlot::Head
        } else if !self.has_item_in_slot(EquipmentSlot::MainHand)
            && self.has_item_in_slot(EquipmentSlot::OffHand)
        {
            EquipmentSlot::OffHand
        } else {
            EquipmentSlot::MainHand
        }
    }

    /// Vanilla `swapItem`: trades the player's held stack with the stand's `slot`.
    fn swap_item(&self, player: &Player, slot: EquipmentSlot, hand: InteractionHand) -> bool {
        let item = self.equipment_in_slot(slot);
        let disabled_slots = self.disabled_slots.load(Ordering::Relaxed);
        if !item.is_empty() && disabled_slots & 1 << slot.filter_bit(DISABLE_TAKING_OFFSET) != 0 {
            return false;
        }
        if item.is_empty() && disabled_slots & 1 << slot.filter_bit(DISABLE_PUTTING_OFFSET) != 0 {
            return false;
        }

        let infinite_materials = player.has_infinite_materials();
        let mut inventory = player.inventory.lock();
        let held = inventory.get_item_in_hand(hand).clone();
        if infinite_materials && item.is_empty() && !held.is_empty() {
            drop(inventory);
            self.set_item_slot(slot, held.copy_with_count(1));
            return true;
        }
        if held.is_empty() || held.count() <= 1 {
            inventory.set_item_in_hand(hand, item);
            drop(inventory);
            self.set_item_slot(slot, held);
            return true;
        }
        if !item.is_empty() {
            return false;
        }
        let one = inventory.split_item_in_hand(hand, 1);
        drop(inventory);
        self.set_item_slot(slot, one);
        true
    }

    /// Vanilla `kill(level, attributedTo)`: removed outright, without a death.
    fn kill_attributed(&self, attributed_to: Option<&SharedEntity>) {
        self.set_removed(RemovalReason::Killed);
        let source: &dyn Entity = attributed_to.map_or(self as &dyn Entity, AsRef::as_ref);
        self.game_event_with_source_entity(&vanilla_game_events::ENTITY_DIE, Some(source));
    }

    /// Vanilla `causeDamage`: burning wears the stand down until it breaks.
    fn cause_damage(&self, source: &DamageSource, damage: f32) {
        let health = self.get_health() - damage;
        if health <= 0.5 {
            self.broken_by_anything(source);
            self.kill_attributed(source.causing_entity());
        } else {
            self.set_health(health);
            self.game_event_with_source_entity(
                &vanilla_game_events::ENTITY_DAMAGE,
                source.causing_entity().map(AsRef::as_ref),
            );
        }
    }

    /// Vanilla `brokenByPlayer`: the stand drops itself, keeping its name.
    fn broken_by_player(&self, world: &Arc<World>, source: &DamageSource) {
        let mut item = ItemStack::new(&vanilla_items::ARMOR_STAND);
        if let Some(name) = self.custom_name() {
            item.set(CUSTOM_NAME, name);
        }
        world.pop_resource(self.block_position(), item);
        self.broken_by_anything(source);
    }

    /// Vanilla `brokenByAnything`: the break sound, death loot, and every worn item
    /// except those cursed to vanish.
    fn broken_by_anything(&self, source: &DamageSource) {
        self.play_broken_sound();
        self.drop_all_death_loot(source);
        let Some(world) = self.level() else {
            return;
        };
        let drop_pos = self.block_position().above();
        for slot in EquipmentSlot::ALL {
            let item = self.living_base.equipment().lock().take(slot);
            if !item.is_empty()
                && !item.has_enchantment_effect(EnchantmentEffectComponent::PreventEquipmentDrop)
            {
                world.pop_resource(drop_pos, item);
            }
        }
    }

    /// Vanilla `playBrokenSound`.
    fn play_broken_sound(&self) {
        if let Some(world) = self.level() {
            world.play_sound_at(
                &sound_events::ENTITY_ARMOR_STAND_BREAK,
                self.sound_source(),
                self.position(),
                1.0,
                1.0,
                None,
            );
        }
    }

    /// Vanilla `showBreakingParticles`: oak plank splinters.
    fn show_breaking_particles(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let dimensions = self.base.dimensions();
        let position = self.position();
        let width = f64::from(dimensions.width / 4.0);
        world.send_particles(
            ParticleData::new(
                &vanilla_particle_types::BLOCK,
                BlockParticleOption::new(vanilla_blocks::OAK_PLANKS.default_state()),
            ),
            DVec3::new(
                position.x,
                position.y + f64::from(dimensions.height) * (2.0 / 3.0),
                position.z,
            ),
            10,
            DVec3::new(width, f64::from(dimensions.height / 4.0), width),
            0.05,
        );
    }

    /// Vanilla `getDefaultDimensions`.
    fn default_dimensions(&self) -> EntityDimensions {
        if self.is_marker() {
            EntityDimensions::with_default_eye_height(0.0, 0.0)
        } else if self.is_small() {
            let mut dimensions = self.entity_type.dimensions.scale(0.5);
            dimensions.eye_height = SMALL_EYE_HEIGHT;
            dimensions
        } else {
            self.entity_type.dimensions
        }
    }
}

impl Entity for ArmorStandEntity {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn base_tick(&self) {
        self.base_tick_living_entity();
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    /// Vanilla `updateInvisibilityStatus`: effects never decide the stand's visibility.
    fn update_data_before_sync(&self) {
        if sync_dirty_mob_effects(&self.living_base, &self.entity_data).is_some() {
            self.entity_data
                .set_base_invisible_flag(self.invisible.load(Ordering::Relaxed));
        }
    }

    fn dimensions_for_pose(&self, _pose: EntityPose) -> EntityDimensions {
        self.default_dimensions().scale(self.get_scale())
    }

    /// Vanilla `ArmorStand.refreshDimensions`: the stand keeps its place.
    fn refresh_dimensions(&self) {
        let position = self.position();
        self.default_refresh_dimensions();
        if self.position() != position
            && let Err(error) = self.try_set_position(position)
        {
            log::warn!(
                "failed to keep armor stand {} in place after a size change: {error}",
                self.id()
            );
        }
    }

    fn is_effective_ai(&self) -> bool {
        self.is_server_driven_movement() && self.has_physics()
    }

    fn is_pushable(&self) -> bool {
        false
    }

    fn is_pickable(&self) -> bool {
        !self.is_removed() && !self.is_marker()
    }

    fn blocks_building(&self) -> bool {
        !self.is_marker()
    }

    fn is_ignoring_block_triggers(&self) -> bool {
        self.is_marker()
    }

    fn is_marker_armor_stand(&self) -> bool {
        self.is_marker()
    }

    fn piston_push_reaction(&self) -> PushReaction {
        if self.is_marker() {
            PushReaction::IgnoreEntity
        } else {
            self.entity_type.flags.piston_push_reaction
        }
    }

    fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
        !explosion.should_affect_blocklike_entities() || self.is_invisible()
    }

    fn skip_attack_interaction(&self, source: &SharedEntity) -> bool {
        let (Some(player), Some(world)) = (source.as_player(), self.level()) else {
            return false;
        };
        !world.may_interact(player, self.block_position())
    }

    fn fall_sounds(&self) -> (SoundEventRef, SoundEventRef) {
        (
            &sound_events::ENTITY_ARMOR_STAND_FALL,
            &sound_events::ENTITY_ARMOR_STAND_FALL,
        )
    }

    fn thunder_hit(&self, _world: &World, _lightning: &LightningBoltEntity) {}

    fn kill(&self, _world: &World) {
        self.kill_attributed(None);
    }

    fn interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        location: DVec3,
    ) -> InteractionResult {
        let held = player.inventory.lock().get_item_in_hand(hand).clone();
        if self.is_marker() || held.is(&vanilla_items::NAME_TAG) {
            return self.interact_entity(player, hand, location);
        }
        if player.is_spectator() {
            return InteractionResult::Success;
        }

        let held_slot = self.get_equipment_slot_for_item(&held);
        if held.is_empty() {
            let clicked_slot = self.clicked_slot(location);
            let target_slot = if self.is_disabled(clicked_slot) {
                held_slot
            } else {
                clicked_slot
            };
            if self.has_item_in_slot(target_slot) && self.swap_item(player, target_slot, hand) {
                return InteractionResult::SuccessServer;
            }
        } else {
            if self.is_disabled(held_slot)
                || held_slot.slot_type() == EquipmentSlotType::Hand && !self.show_arms()
            {
                return InteractionResult::Fail;
            }
            if self.swap_item(player, held_slot, hand) {
                return InteractionResult::SuccessServer;
            }
        }
        self.interact_entity(player, hand, location)
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_equipment(nbt);
        nbt.insert(
            "Invisible",
            i8::from(self.invisible.load(Ordering::Relaxed)),
        );
        nbt.insert("Small", i8::from(self.is_small()));
        nbt.insert("ShowArms", i8::from(self.show_arms()));
        nbt.insert("DisabledSlots", self.disabled_slots.load(Ordering::Relaxed));
        nbt.insert("NoBasePlate", i8::from(!self.show_base_plate()));
        if self.is_marker() {
            nbt.insert("Marker", 1_i8);
        }
        nbt.insert("Pose", NbtTag::Compound(self.pose().save()));
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_equipment(nbt);
        let flag = |name: &str| nbt.byte(name).is_some_and(|value| value != 0);
        self.set_invisible(flag("Invisible"));
        self.set_small(flag("Small"));
        self.set_show_arms(flag("ShowArms"));
        self.disabled_slots
            .store(nbt.int("DisabledSlots").unwrap_or(0), Ordering::Relaxed);
        self.set_no_base_plate(flag("NoBasePlate"));
        self.set_marker(flag("Marker"));
        self.base.set_no_physics(!self.has_physics());
        if let Some(pose) = nbt.compound("Pose").and_then(ArmorStandPose::load) {
            self.set_pose(pose);
        }
    }
}

impl LivingEntity for ArmorStandEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(health.clamp(0.0, max_health));
    }

    fn can_use_slot(&self, slot: EquipmentSlot) -> bool {
        slot != EquipmentSlot::Body && slot != EquipmentSlot::Saddle && !self.is_disabled(slot)
    }

    /// Vanilla `ArmorStand.hurtServer`: stands do not take damage like mobs. Two
    /// quick punches (or one from a creative player) break them, fire and lava burn
    /// them down, and explosions blow them apart.
    fn hurt_server(&self, world: &World, source: &DamageSource, _amount: f32) -> bool {
        if self.is_removed() {
            return false;
        }
        if !world.get_game_rule(&MOB_GRIEFING)
            && source
                .causing_entity()
                .is_some_and(|entity| entity.as_mob().is_some())
        {
            return false;
        }
        if source.is(&DamageTypeTag::BYPASSES_INVULNERABILITY) {
            self.kill_attributed(source.causing_entity());
            return false;
        }
        if self.is_invulnerable_to(world, source)
            || self.invisible.load(Ordering::Relaxed)
            || self.is_marker()
        {
            return false;
        }
        if source.is(&DamageTypeTag::IS_EXPLOSION) {
            self.broken_by_anything(source);
            self.kill_attributed(source.causing_entity());
            return false;
        }
        if source.is(&DamageTypeTag::IGNITES_ARMOR_STANDS) {
            if self.is_on_fire() {
                self.cause_damage(source, 0.15);
            } else {
                self.ignite_for_ticks(5 * 20);
            }
            return false;
        }
        if source.is(&DamageTypeTag::BURNS_ARMOR_STANDS) && self.get_health() > 0.5 {
            self.cause_damage(source, 4.0);
            return false;
        }

        let allow_incremental_breaking = source.is(&DamageTypeTag::CAN_BREAK_ARMOR_STAND);
        let should_kill = source.is(&DamageTypeTag::ALWAYS_KILLS_ARMOR_STANDS);
        if !allow_incremental_breaking && !should_kill {
            return false;
        }
        if source
            .causing_entity()
            .and_then(|entity| entity.as_player())
            .is_some_and(|player| !player.abilities.lock().may_build)
        {
            return false;
        }
        if source.is_creative_player() {
            self.play_broken_sound();
            self.show_breaking_particles();
            self.kill_attributed(source.causing_entity());
            return true;
        }

        let time = world.game_time();
        if time - self.last_hit.load(Ordering::Relaxed) > WOBBLE_TIME && !should_kill {
            self.broadcast_entity_event(EntityStatus::ArmorstandWobble);
            self.game_event_with_source_entity(
                &vanilla_game_events::ENTITY_DAMAGE,
                source.causing_entity().map(AsRef::as_ref),
            );
            self.last_hit.store(time, Ordering::Relaxed);
        } else if let Some(world) = self.level() {
            self.broken_by_player(&world, source);
            self.show_breaking_particles();
            self.kill_attributed(source.causing_entity());
        }
        true
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ARMOR_STAND_HIT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ARMOR_STAND_BREAK)
    }

    fn is_baby(&self) -> bool {
        self.is_small()
    }

    fn is_affected_by_potions(&self) -> bool {
        false
    }

    fn can_be_seen_by_anyone(&self) -> bool {
        !self.is_invisible() && !self.is_marker()
    }

    /// Vanilla `ArmorStand.tickHeadTurn`: the body always faces where the stand does.
    fn tick_head_turn(&self) {
        let (y_rot_o, _) = self.base.old_rotation();
        self.living_base
            .set_y_body_rot_and_old(self.rotation().0, y_rot_o);
    }

    fn set_y_body_rot(&self, y_body_rot: f32) {
        self.living_base.snap_body_and_head_rot(y_body_rot);
        let (_, x_rot_o) = self.base.old_rotation();
        self.base.set_old_rotation((y_body_rot, x_rot_o));
    }

    fn set_y_head_rot(&self, y_head_rot: f32) {
        self.set_y_body_rot(y_head_rot);
    }

    fn travel(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        if self.has_physics() {
            self.default_travel(entity, input)
        } else {
            None
        }
    }

    /// Vanilla `ArmorStand.pushEntities`: only rideable minecarts right on top of
    /// the stand get nudged.
    fn push_entities(&self) {
        let Some(world) = self.level() else {
            return;
        };
        let id = self.id();
        for entity in world.get_entities_in_aabb(&self.bounding_box()) {
            if entity.id() != id
                && entity
                    .as_abstract_minecart()
                    .is_some_and(AbstractMinecart::is_rideable)
                && self.distance_to_sqr(entity.position()) <= MINECART_PUSH_DISTANCE_SQR
            {
                entity.push_entity(self);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::items::item::BlockHitResult;
    use steel_registry::{init_vanilla_registry, vanilla_damage_types, vanilla_entities};
    use steel_utils::types::{GameType, UpdateFlags};
    use steel_utils::{BlockPos, ChunkPos, Direction, Downcast as _, Identifier, WorldAabb};
    use text_components::TextComponent;

    use super::*;
    use crate::behavior::items::ArmorStandItem;
    use crate::behavior::{ItemBehavior, UseOnContext, init_behaviors};
    use crate::entity::entities::ItemEntity;
    use crate::entity::{add_spawned_entity, init_entities, next_entity_id};
    use crate::player::ResetReason;
    use crate::test_support::{
        TestPlayerBuilder, TestWorld, dropped_items, fresh_test_world, insert_ready_full_chunk,
    };

    struct Scene {
        _fixture: TestWorld,
        world: Arc<World>,
        player: Arc<Player>,
    }

    fn scene(name: &'static str) -> Scene {
        init_vanilla_registry();
        init_behaviors();
        init_entities();
        let fixture = fresh_test_world(name);
        let world = Arc::clone(&fixture.world);
        insert_ready_full_chunk(&world, ChunkPos::new(0, 0));
        let player =
            TestPlayerBuilder::new(Arc::clone(&world), "Stander", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        Scene {
            _fixture: fixture,
            world,
            player,
        }
    }

    fn hold(player: &Player, item: ItemStack) {
        player
            .inventory
            .lock()
            .set_item_in_hand(InteractionHand::MainHand, item);
    }

    fn held(player: &Player) -> ItemStack {
        player
            .inventory
            .lock()
            .get_item_in_hand(InteractionHand::MainHand)
            .clone()
    }

    /// Places a stand from the player's hand on top of `floor`.
    fn place(scene: &Scene, floor: BlockPos) -> InteractionResult {
        let mut context = UseOnContext::new(
            &scene.player,
            InteractionHand::MainHand,
            BlockHitResult {
                location: floor.0.as_dvec3() + DVec3::new(0.5, 1.0, 0.5),
                direction: Direction::Up,
                block_pos: floor,
                miss: false,
                inside: false,
                world_border_hit: false,
            },
            &scene.world,
            Arc::clone(&scene.player.inventory),
        );
        ArmorStandItem.use_on(&mut context)
    }

    fn stands_near(world: &World, pos: BlockPos) -> Vec<SharedEntity> {
        world.get_entities_in_aabb_matching(
            &WorldAabb::of_size(pos.0.as_dvec3(), 4.0, 4.0, 4.0),
            |entity| entity.downcast_ref::<ArmorStandEntity>().is_some(),
        )
    }

    /// Items dropped around a stand standing at `pos`; its gear pops a block higher.
    fn drops_at(world: &Arc<World>, pos: DVec3) -> Vec<ItemStack> {
        dropped_items(world, pos + DVec3::Y)
            .iter()
            .filter_map(|entity| {
                entity
                    .downcast_ref::<ItemEntity>()
                    .map(ItemEntity::get_item)
            })
            .collect()
    }

    #[test]
    fn the_item_stands_a_stand_facing_the_player_in_eighth_turns() {
        let scene = scene("armor_stand_place");
        let floor = BlockPos::new(3, 63, 3);
        assert!(scene.world.set_block(
            floor,
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        hold(
            &scene.player,
            ItemStack::with_count(&vanilla_items::ARMOR_STAND, 2),
        );
        scene.player.set_rotation((100.0, 0.0));

        assert_eq!(place(&scene, floor), InteractionResult::Success);
        let stands = stands_near(&scene.world, floor);
        assert_eq!(stands.len(), 1);
        let stand = &stands[0];
        assert_eq!(stand.position(), DVec3::new(3.5, 64.0, 3.5));
        // Facing the player: 100 - 180 = -80 rounds to the nearest eighth turn, -90.
        assert_eq!(stand.rotation(), (-90.0, 0.0));
        assert_eq!(held(&scene.player).count(), 1);

        // The spot is taken now.
        assert_eq!(place(&scene, floor), InteractionResult::Fail);
        assert_eq!(held(&scene.player).count(), 1);
        assert_eq!(stands_near(&scene.world, floor).len(), 1);
    }

    #[test]
    fn players_dress_and_undress_a_stand_unless_its_slots_are_locked() {
        let scene = scene("armor_stand_swap");
        let stand = Arc::new(ArmorStandEntity::new(
            &vanilla_entities::ARMOR_STAND,
            next_entity_id(),
            DVec3::new(2.5, 64.0, 2.5),
            Arc::downgrade(&scene.world),
        ));
        let player = &scene.player;
        let head_click = DVec3::new(0.0, 1.8, 0.0);

        // A stand without arms holds nothing.
        hold(player, ItemStack::new(&vanilla_items::IRON_SWORD));
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::Fail
        );

        // Three pumpkins in hand: one goes on the stand's head.
        hold(
            player,
            ItemStack::with_count(&vanilla_items::CARVED_PUMPKIN, 3),
        );
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::SuccessServer
        );
        assert!(
            stand
                .equipment_in_slot(EquipmentSlot::Head)
                .is(&vanilla_items::CARVED_PUMPKIN)
        );
        assert_eq!(held(player).count(), 2);
        // The slot is full and the hand holds more than one: no swap.
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::Pass
        );

        // An empty hand at head height takes the pumpkin back.
        hold(player, ItemStack::empty());
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::SuccessServer
        );
        assert!(held(player).is(&vanilla_items::CARVED_PUMPKIN));
        assert!(!stand.has_item_in_slot(EquipmentSlot::Head));

        // Locked against taking (bit 8 + slot id), the pumpkin stays on.
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::SuccessServer
        );
        stand.disabled_slots.store(
            1 << EquipmentSlot::Head.filter_bit(DISABLE_TAKING_OFFSET),
            Ordering::Relaxed,
        );
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, head_click),
            InteractionResult::Pass
        );
        assert!(stand.has_item_in_slot(EquipmentSlot::Head));

        // With arms, a sword goes into the hand; a click low on the legs finds
        // nothing there and falls back to the main hand.
        stand.disabled_slots.store(0, Ordering::Relaxed);
        stand.set_show_arms(true);
        hold(player, ItemStack::new(&vanilla_items::IRON_SWORD));
        assert_eq!(
            stand.interact(player, InteractionHand::MainHand, DVec3::new(0.0, 0.5, 0.0)),
            InteractionResult::SuccessServer
        );
        assert!(
            stand
                .equipment_in_slot(EquipmentSlot::MainHand)
                .is(&vanilla_items::IRON_SWORD)
        );
    }

    #[test]
    fn two_quick_punches_break_a_stand_into_its_item_and_its_gear() {
        let scene = scene("armor_stand_break");
        let world = &scene.world;
        let player = &scene.player;
        let source = DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK)
            .with_causing_entity(Arc::clone(player) as SharedEntity);
        let spawn = |position: DVec3| {
            let stand = Arc::new(ArmorStandEntity::new(
                &vanilla_entities::ARMOR_STAND,
                next_entity_id(),
                position,
                Arc::downgrade(world),
            ));
            stand.set_custom_name(Some(TextComponent::plain("Bob")));
            stand.set_item_slot(
                EquipmentSlot::Chest,
                ItemStack::new(&vanilla_items::IRON_CHESTPLATE),
            );
            let mut cursed = ItemStack::new(&vanilla_items::IRON_HELMET);
            cursed.set_enchantments(&[(Identifier::vanilla_static("vanishing_curse"), 1)], false);
            stand.set_item_slot(EquipmentSlot::Head, cursed);
            assert!(add_spawned_entity(world, Arc::clone(&stand) as SharedEntity).is_ok());
            // The last wobble was long ago.
            stand.last_hit.store(-100, Ordering::Relaxed);
            stand
        };

        let stand = spawn(DVec3::new(2.5, 64.0, 2.5));
        assert!(stand.hurt(world, &source, 1.0));
        assert!(!stand.is_removed());
        assert!(stand.hurt(world, &source, 1.0));
        assert!(stand.is_removed());
        let drops = drops_at(world, stand.position());
        let named_stand = drops
            .iter()
            .find(|item| item.is(&vanilla_items::ARMOR_STAND))
            .expect("the stand drops itself");
        assert_eq!(
            named_stand.get(CUSTOM_NAME),
            Some(&TextComponent::plain("Bob"))
        );
        assert!(
            drops
                .iter()
                .any(|item| item.is(&vanilla_items::IRON_CHESTPLATE))
        );
        assert!(
            !drops
                .iter()
                .any(|item| item.is(&vanilla_items::IRON_HELMET))
        );

        // Explosions scatter the gear but destroy the stand itself.
        let stand = spawn(DVec3::new(9.5, 64.0, 9.5));
        let explosion = DamageSource::environment(&vanilla_damage_types::EXPLOSION);
        assert!(!stand.hurt(world, &explosion, 4.0));
        assert!(stand.is_removed());
        let drops = drops_at(world, stand.position());
        assert!(
            drops
                .iter()
                .any(|item| item.is(&vanilla_items::IRON_CHESTPLATE))
        );
        assert!(
            !drops
                .iter()
                .any(|item| item.is(&vanilla_items::ARMOR_STAND))
        );

        // A creative player breaks it in one punch and nothing drops.
        player.restore_game_modes(GameType::Creative, None);
        player
            .abilities
            .lock()
            .update_for_game_mode(GameType::Creative);
        let stand = spawn(DVec3::new(13.5, 64.0, 2.5));
        assert!(stand.hurt(world, &source, 1.0));
        assert!(stand.is_removed());
        assert_eq!(drops_at(world, stand.position()), Vec::<ItemStack>::new());
    }

    #[test]
    fn flags_locks_and_pose_round_trip_through_nbt() {
        let scene = scene("armor_stand_nbt");
        let stand = ArmorStandEntity::new(
            &vanilla_entities::ARMOR_STAND,
            next_entity_id(),
            DVec3::new(2.5, 64.0, 2.5),
            Arc::downgrade(&scene.world),
        );
        stand.set_small(true);
        stand.set_show_arms(true);
        stand.set_no_base_plate(true);
        stand.disabled_slots.store(4_144_959, Ordering::Relaxed);
        let mut pose = ArmorStandPose::DEFAULT;
        pose.head = Rotations::new(370.0, f32::NAN, -45.0);
        stand.set_pose(pose);
        assert_eq!(stand.base.dimensions().height, 1.975 * 0.5);
        assert_eq!(stand.base.dimensions().eye_height, SMALL_EYE_HEIGHT);

        let mut nbt = NbtCompound::new();
        stand.save_additional(&mut nbt);
        assert_eq!(nbt.byte("Small"), Some(1));
        assert_eq!(nbt.byte("Marker"), None);
        // Only the limb that left its default pose is written, wrapped into range.
        let saved_pose = nbt.compound("Pose").expect("pose");
        assert_eq!(saved_pose.keys().count(), 1);
        assert_eq!(
            saved_pose.list("Head").and_then(NbtList::floats),
            Some(vec![10.0, 0.0, -45.0])
        );

        nbt.insert("Marker", 1_i8);
        let mut bytes = Vec::new();
        nbt.write(&mut bytes);
        let borrowed = read_compound(&mut Cursor::new(&bytes)).expect("nbt");
        let loaded = ArmorStandEntity::new(
            &vanilla_entities::ARMOR_STAND,
            next_entity_id(),
            stand.position(),
            Arc::downgrade(&scene.world),
        );
        loaded.load_additional((&borrowed).into());
        assert!(loaded.is_small() && loaded.show_arms() && !loaded.show_base_plate());
        assert_eq!(loaded.disabled_slots.load(Ordering::Relaxed), 4_144_959);
        assert_eq!(loaded.pose(), stand.pose());
        // A marker has no size and no physics.
        assert!(loaded.is_marker());
        assert!(loaded.no_physics());
        assert!(!loaded.is_pickable());
        assert_eq!(loaded.base.dimensions().width, 0.0);
    }
}
