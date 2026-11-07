//! Vanilla allay: a flying helper that collects items matching the one it
//! holds and brings them to the player who gave it that item, or to a note
//! block it heard. It dances near a playing jukebox and duplicates when given
//! an amethyst shard while dancing.

mod ai;

use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_protocol::packets::game::SoundSource;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::POTION_CONTENTS;
use steel_registry::enchantment_effect::EnchantmentEffectComponent;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_entity_data::AllayEntityData;
use steel_registry::vanilla_item_tags::ItemTag;
use steel_registry::{
    REGISTRY, TaggedRegistryExt as _, sound_events, vanilla_attributes, vanilla_blocks,
    vanilla_entities, vanilla_game_events, vanilla_game_rules,
};
use steel_utils::entity_events::EntityStatus;
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

pub use ai::hear_noteblock;

use crate::behavior::InteractionResult;
use crate::entity::ai::brain::behaviors::utils::throw_item;
use crate::entity::ai::brain::memory_types::{
    ITEM_PICKUP_COOLDOWN_TICKS, LIKED_NOTEBLOCK_COOLDOWN_TICKS, LIKED_NOTEBLOCK_POSITION,
    LIKED_PLAYER,
};
use crate::entity::ai::brain::{Brain, Memories};
use crate::entity::ai::flying_move::FlyingMoveControl;
use crate::entity::ai::navigation::PathNavigation;
use crate::entity::damage::DamageSource;
use crate::entity::entities::ItemEntity;
use crate::entity::entities::mobs::villagers::{
    load_inventory, pick_up_item_into_inventory, save_inventory,
};
use crate::entity::entity::EntityEventSource as _;
use crate::entity::{
    DefaultEntityPush, Entity, EntityBase, EntityBaseLoad, EntitySyncedData, EquipmentSlot,
    LivingEntity, LivingEntityBase, Mob, MobBase, PathfinderMob, SharedEntity, next_entity_id,
};
use crate::inventory::container::Container as _;
use crate::inventory::container::SimpleContainer;
use crate::physics::MoveResult;
use crate::player::Player;
use crate::player::player_inventory::Prediction;
use crate::world::World;

/// Vanilla `FlyingMoveControl(this, 20, true)`.
const ALLAY_MOVE_CONTROL: FlyingMoveControl = FlyingMoveControl {
    max_turn: 20,
    hovers_in_place: true,
};
/// Vanilla `ITEM_PICKUP_REACH`.
const ITEM_PICKUP_REACH: DVec3 = DVec3::new(1.0, 1.0, 1.0);
/// Vanilla `DUPLICATION_COOLDOWN_TICKS`.
const DUPLICATION_COOLDOWN_TICKS: i64 = 6000;
/// Vanilla `createNavigation`: `setRequiredPathLength(48.0F)`.
const REQUIRED_PATH_LENGTH: f32 = 48.0;
const SOUND_VOLUME: f32 = 0.4;
const INTERACTION_SOUND_VOLUME: f32 = 2.0;
/// Vanilla `aiStep`: an allay heals one point every this many ticks.
const HEAL_INTERVAL: i32 = 10;
const TAG_INVENTORY_SIZE: usize = 1;
const TAG_DUPLICATION_COOLDOWN: &str = "DuplicationCooldown";
/// Vanilla `THROW_SOUND_PITCHES`.
const THROW_SOUND_PITCHES: [f32; 16] = [
    0.5625, 0.625, 0.75, 0.9375, 1.0, 1.0, 1.125, 1.25, 1.5, 1.875, 2.0, 2.25, 2.5, 3.0, 3.75, 4.0,
];

#[derive(Default)]
struct AllayState {
    jukebox_pos: Option<BlockPos>,
    duplication_cooldown: i64,
}

/// Vanilla allay.
#[entity_behavior(class = "Allay")]
pub struct AllayEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    living_base: LivingEntityBase,
    mob_base: MobBase,
    entity_data: SyncMutex<AllayEntityData>,
    brain: Brain<Self>,
    inventory: SyncMutex<SimpleContainer>,
    state: SyncMutex<AllayState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `AllayEntity`.
unsafe impl DowncastType for AllayEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/allay");
}

impl AllayEntity {
    /// Creates a new allay at runtime.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_with_base(
            EntityBase::new(id, position, entity_type.dimensions, world),
            entity_type,
        )
    }

    /// Reconstructs an allay from persisted base entity state.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        Self::new_with_base(
            EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
        )
    }

    fn new_with_base(base: EntityBase, entity_type: EntityTypeRef) -> Self {
        let living_base = LivingEntityBase::new(entity_type);
        let mob_base = MobBase::new();
        let follow_range = living_base
            .attributes()
            .lock()
            .get_value(vanilla_attributes::FOLLOW_RANGE)
            .unwrap_or(vanilla_attributes::FOLLOW_RANGE.default_value);
        {
            let mut navigation = PathNavigation::flying();
            navigation.set_can_open_doors(false);
            navigation.set_can_float(true);
            navigation.set_required_path_length(REQUIRED_PATH_LENGTH, follow_range);
            *mob_base.navigation().lock() = navigation;
        }
        let mut entity_data = AllayEntityData::new();
        living_base.initialize_synced_data(&mut entity_data);
        entity_data.can_duplicate.set(true);

        Self {
            base,
            entity_type,
            living_base,
            mob_base,
            entity_data: SyncMutex::new(entity_data),
            brain: Brain::new(
                &[
                    &LIKED_PLAYER,
                    &LIKED_NOTEBLOCK_POSITION,
                    &LIKED_NOTEBLOCK_COOLDOWN_TICKS,
                ],
                ai::sensors(),
                ai::activities(),
                None,
            ),
            inventory: SyncMutex::new(SimpleContainer::new(TAG_INVENTORY_SIZE)),
            state: SyncMutex::new(AllayState::default()),
        }
    }

    const fn brain(&self) -> &Brain<Self> {
        &self.brain
    }

    fn brain_memories_lock(&self) -> parking_lot::MutexGuard<'_, Memories> {
        self.brain.memories().lock()
    }

    /// Vanilla `isDancing`.
    #[must_use]
    pub fn is_dancing(&self) -> bool {
        *self.entity_data.lock().dancing.get()
    }

    /// Vanilla `setDancing`: a panicking allay never starts dancing.
    pub fn set_dancing(&self, dancing: bool) {
        if !dancing || !self.is_panicking() {
            self.entity_data.lock().dancing.set(dancing);
        }
    }

    /// Vanilla `canDuplicate`.
    #[must_use]
    pub fn can_duplicate(&self) -> bool {
        *self.entity_data.lock().can_duplicate.get()
    }

    fn set_duplication_cooldown(&self, duplication_cooldown: i64) {
        self.state.lock().duplication_cooldown = duplication_cooldown;
        self.entity_data
            .lock()
            .can_duplicate
            .set(duplication_cooldown == 0);
    }

    fn reset_duplication_cooldown(&self) {
        self.set_duplication_cooldown(DUPLICATION_COOLDOWN_TICKS);
    }

    fn update_duplication_cooldown(&self) {
        let cooldown = self.state.lock().duplication_cooldown;
        if cooldown > 0 {
            self.set_duplication_cooldown(cooldown - 1);
        }
    }

    /// Vanilla `setJukeboxPlaying`.
    pub fn set_jukebox_playing(&self, jukebox: BlockPos, is_playing: bool) {
        if is_playing {
            if !self.is_dancing() {
                self.state.lock().jukebox_pos = Some(jukebox);
                self.set_dancing(true);
            }
        } else {
            let matches = {
                let mut state = self.state.lock();
                let matches = state.jukebox_pos.is_none_or(|pos| pos == jukebox);
                if matches {
                    state.jukebox_pos = None;
                }
                matches
            };
            if matches {
                self.set_dancing(false);
            }
        }
    }

    /// Vanilla `shouldStopDancing`.
    fn should_stop_dancing(&self) -> bool {
        let Some(jukebox_pos) = self.state.lock().jukebox_pos else {
            return true;
        };
        let radius = f64::from(vanilla_game_events::JUKEBOX_PLAY.notification_radius);
        let (x, y, z) = jukebox_pos.get_center();
        let center = DVec3::new(x, y, z);
        center.distance_squared(self.position()) >= radius * radius
            || self.level().is_none_or(|world| {
                world.get_block_state(jukebox_pos).get_block() != &vanilla_blocks::JUKEBOX
            })
    }

    /// Vanilla `hasItemInHand`.
    #[must_use]
    pub fn has_item_in_hand(&self) -> bool {
        !self.equipment_in_slot(EquipmentSlot::MainHand).is_empty()
    }

    fn is_on_pickup_cooldown(&self) -> bool {
        self.brain_memories_lock()
            .has_value(&ITEM_PICKUP_COOLDOWN_TICKS)
    }

    fn inventory_is_empty(&self) -> bool {
        self.inventory.lock().is_empty()
    }

    /// Vanilla `getInventory().removeItem(0, 1)`.
    fn remove_one_from_inventory(&self) -> ItemStack {
        self.inventory.lock().remove_item(0, 1)
    }

    /// Vanilla `SimpleContainer.removeAllItems`.
    fn remove_all_inventory_items(&self) -> Vec<ItemStack> {
        let mut inventory = self.inventory.lock();
        let items = inventory
            .items_mut()
            .iter_mut()
            .filter(|stack| !stack.is_empty())
            .map(mem::take)
            .collect();
        inventory.set_changed();
        items
    }

    fn is_liked_player(&self, other: Option<&dyn Entity>) -> bool {
        let Some(player) = other.and_then(Entity::as_player) else {
            return false;
        };
        self.brain_memories_lock()
            .get_ref(&LIKED_PLAYER)
            .is_some_and(|liked| *liked == player.uuid())
    }

    /// Vanilla `allayConsidersItemEqual`: same item and the same potion, if any.
    fn allay_considers_item_equal(item1: &ItemStack, item2: &ItemStack) -> bool {
        ItemStack::is_same_item(item1, item2)
            && item1.get(POTION_CONTENTS) == item2.get(POTION_CONTENTS)
    }

    /// Vanilla `duplicateAllay`.
    fn duplicate_allay(&self, world: &Arc<World>) {
        let allay = Arc::new(Self::new(
            &vanilla_entities::ALLAY,
            next_entity_id(),
            self.position(),
            Arc::downgrade(world),
        ));
        allay.snap_to(self.position(), 0.0, 0.0);
        allay.set_persistence_required();
        allay.reset_duplication_cooldown();
        self.reset_duplication_cooldown();
        if let Err(error) = world.try_add_entity(allay) {
            log::warn!("Failed to spawn duplicated allay: {error}");
        }
    }

    fn play_interaction_sound(&self, world: &World, player: &Player, sound: SoundEventRef) {
        world.play_sound_at(
            sound,
            SoundSource::Neutral,
            self.position(),
            INTERACTION_SOUND_VOLUME,
            1.0,
            Some(player.id()),
        );
    }
}

impl DefaultEntityPush for AllayEntity {}

impl Entity for AllayEntity {
    fn as_default_entity_push(&self) -> Option<&dyn DefaultEntityPush> {
        Some(self)
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    /// Vanilla `Allay.tick`: a panicking allay stops dancing.
    fn tick(self: Arc<Self>) {
        let entity = Arc::clone(&self).into_shared_entity();
        self.tick_living_entity(&entity);
        if self.is_panicking() {
            self.set_dancing(false);
        }
    }

    fn base_tick(&self) {
        Mob::base_tick_mob(self);
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn sound_source(&self) -> SoundSource {
        SoundSource::Neutral
    }

    /// Vanilla `playStepSound`: allays make no step sounds.
    fn play_step_sound(&self, _pos: BlockPos, _block_state: BlockStateId) {}

    fn is_flapping(&self) -> bool {
        !self.on_ground()
    }

    /// Vanilla `checkFallDamage`: allays never take fall damage.
    fn check_fall_damage(
        self: Arc<Self>,
        _vertical_movement: f64,
        _on_ground: bool,
        _on_state: BlockStateId,
        _pos: BlockPos,
        _world: &Arc<World>,
    ) {
    }

    fn considers_entity_as_ally(&self, other: &dyn Entity) -> bool {
        self.is_liked_player(Some(other))
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_mob(nbt);
        save_inventory(&self.inventory, nbt);
        nbt.insert(
            TAG_DUPLICATION_COOLDOWN,
            self.state.lock().duplication_cooldown,
        );
        nbt.insert("Brain", self.brain.save());
    }

    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_mob(nbt);
        load_inventory(&self.inventory, nbt);
        // Vanilla reads the long back with `getIntOr`, which accepts any number.
        let cooldown = nbt
            .long(TAG_DUPLICATION_COOLDOWN)
            .or_else(|| nbt.int(TAG_DUPLICATION_COOLDOWN).map(i64::from))
            .unwrap_or(0);
        self.set_duplication_cooldown(i64::from(cooldown as i32));
        if let Some(brain) = nbt.compound("Brain") {
            self.brain.memories().lock().load(brain);
        }
    }
}

impl LivingEntity for AllayEntity {
    fn living_base(&self) -> &LivingEntityBase {
        &self.living_base
    }

    fn brain_memories(&self) -> Option<&SyncMutex<Memories>> {
        Some(self.brain.memories())
    }

    fn get_health(&self) -> f32 {
        *self.entity_data.lock().living_entity().health.get()
    }

    fn set_health(&self, health: f32) {
        let max_health = self.get_max_health();
        let clamped = health.clamp(0.0, max_health);
        self.entity_data
            .lock()
            .living_entity_mut()
            .health
            .set(clamped);
    }

    fn hurt_sound(&self, _source: &DamageSource) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ALLAY_HURT)
    }

    fn death_sound(&self) -> Option<SoundEventRef> {
        Some(&sound_events::ENTITY_ALLAY_DEATH)
    }

    fn sound_volume(&self) -> f32 {
        SOUND_VOLUME
    }

    fn omnidirectional_air_mover(&self) -> bool {
        true
    }

    /// Vanilla `hurtServer`: the liked player cannot hurt its allay.
    fn hurt_server(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_liked_player(source.causing_entity().map(AsRef::as_ref)) {
            return false;
        }
        self.default_hurt_server(world, source, amount)
    }

    fn travel(&self, entity: &SharedEntity, input: DVec3) -> Option<MoveResult> {
        let speed = self.get_speed();
        self.travel_flying(entity, input, speed, speed, speed)
    }

    fn server_ai_step(&self, entity: &SharedEntity) {
        Mob::mob_server_ai_step(self, entity);
    }

    fn ai_step(&self, entity: &SharedEntity) -> Option<MoveResult> {
        let result = Mob::mob_ai_step(self, entity);
        if LivingEntity::is_alive(self) && self.tick_count() % HEAL_INTERVAL == 0 {
            self.heal(1.0);
        }
        if self.is_dancing() && self.should_stop_dancing() && self.tick_count() % 20 == 0 {
            self.set_dancing(false);
            self.state.lock().jukebox_pos = None;
        }
        self.update_duplication_cooldown();
        result
    }

    /// Vanilla `dropEquipment`: the inventory and the held item drop.
    fn drop_equipment(&self) {
        for stack in self.remove_all_inventory_items() {
            let _ = self.spawn_at_location(stack, 0.0);
        }
        let held = self.equipment_in_slot(EquipmentSlot::MainHand);
        if !held.is_empty()
            && !held.has_enchantment_effect(EnchantmentEffectComponent::PreventEquipmentDrop)
        {
            let _ = self.spawn_at_location(held, 0.0);
            self.set_item_slot(EquipmentSlot::MainHand, ItemStack::empty());
        }
    }
}

impl Mob for AllayEntity {
    fn mob_base(&self) -> &MobBase {
        &self.mob_base
    }

    fn tick_goal_selectors(&self, entity: &SharedEntity) {
        PathfinderMob::tick_pathfinder_goal_selectors(self, entity);
    }

    fn tick_move_control(&self) {
        ALLAY_MOVE_CONTROL.tick(self);
    }

    fn custom_server_ai_step(&self) {
        if let Some(world) = self.level() {
            self.brain.tick(&world, self);
            ai::update_activity(self);
        }
    }

    fn ambient_sound(&self) -> Option<SoundEventRef> {
        Some(if self.has_item_in_slot(EquipmentSlot::MainHand) {
            &sound_events::ENTITY_ALLAY_AMBIENT_WITH_ITEM
        } else {
            &sound_events::ENTITY_ALLAY_AMBIENT_WITHOUT_ITEM
        })
    }

    /// Vanilla `canPickUpLoot`: only while holding an item and off cooldown.
    fn can_pick_up_loot(&self) -> bool {
        !self.is_on_pickup_cooldown() && self.has_item_in_hand()
    }

    fn get_pickup_reach(&self) -> DVec3 {
        ITEM_PICKUP_REACH
    }

    /// Vanilla `wantsToPickUp`: more of what it holds, with room for it.
    fn wants_to_pick_up(&self, item_stack: &ItemStack) -> bool {
        let held = self.equipment_in_slot(EquipmentSlot::MainHand);
        !held.is_empty()
            && self
                .level()
                .is_some_and(|world| world.get_game_rule(&vanilla_game_rules::MOB_GRIEFING))
            && self.inventory.lock().can_add_item(item_stack)
            && Self::allay_considers_item_equal(&held, item_stack)
    }

    fn pick_up_item(&self, world: &Arc<World>, item_entity: &ItemEntity) {
        pick_up_item_into_inventory(self, &self.inventory, world, item_entity);
    }

    fn remove_when_far_away(&self, _dist_sqr: f64) -> bool {
        false
    }

    /// Vanilla `mobInteract`: duplicate while dancing, take an item, or give
    /// everything back.
    fn mob_interact(&self, player: &Player, hand: InteractionHand) -> InteractionResult {
        let Some(world) = self.level() else {
            return InteractionResult::Pass;
        };
        let interaction_item = {
            let inventory = player.inventory.lock();
            let item_stack = inventory.get_item_in_hand(hand);
            item_stack.copy_with_count(item_stack.count())
        };
        let item_in_hand = self.equipment_in_slot(EquipmentSlot::MainHand);
        if self.is_dancing()
            && REGISTRY
                .items
                .is_in_tag(interaction_item.item(), &ItemTag::DUPLICATES_ALLAYS)
            && self.can_duplicate()
        {
            self.duplicate_allay(&world);
            self.broadcast_entity_event(EntityStatus::InLoveHearts);
            self.play_interaction_sound(&world, player, &sound_events::BLOCK_AMETHYST_BLOCK_CHIME);
            Self::consume_interaction_item(player, hand);
            return InteractionResult::Success;
        }
        if item_in_hand.is_empty() && !interaction_item.is_empty() {
            self.set_item_slot(EquipmentSlot::MainHand, interaction_item.copy_with_count(1));
            Self::consume_interaction_item(player, hand);
            self.play_interaction_sound(&world, player, &sound_events::ENTITY_ALLAY_ITEM_GIVEN);
            self.brain_memories_lock()
                .set(&LIKED_PLAYER, Some(player.uuid()));
            return InteractionResult::Success;
        }
        if !item_in_hand.is_empty()
            && hand == InteractionHand::MainHand
            && interaction_item.is_empty()
        {
            self.set_item_slot(EquipmentSlot::MainHand, ItemStack::empty());
            self.play_interaction_sound(&world, player, &sound_events::ENTITY_ALLAY_ITEM_TAKEN);
            let animation = self.interact_animation(InteractionHand::MainHand);
            let _ = self.swing(InteractionHand::MainHand, animation, true);
            for stack in self.remove_all_inventory_items() {
                throw_item(self, stack, self.position(), DVec3::splat(0.3), 0.3);
            }
            self.brain_memories_lock().erase(&LIKED_PLAYER);
            player.add_item_or_drop(item_in_hand, Prediction::Predicted);
            return InteractionResult::Success;
        }
        InteractionResult::Pass
    }

    fn mob_flags(&self) -> i8 {
        *self.entity_data.lock().mob().mob_flags.get()
    }

    fn set_mob_flags(&self, flags: i8) {
        self.entity_data.lock().mob_mut().mob_flags.set(flags);
    }
}

impl AllayEntity {
    /// Vanilla `removeInteractionItem`: `interactionItem.consume(1, player)`.
    fn consume_interaction_item(player: &Player, hand: InteractionHand) {
        let has_infinite_materials = player.has_infinite_materials();
        player
            .inventory
            .lock()
            .mutate_item_in_hand(hand, |item| item.consume(1, has_infinite_materials));
    }
}

impl PathfinderMob for AllayEntity {
    /// Vanilla `shouldStayCloseToLeashHolder`.
    fn should_stay_close_to_leash_holder(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests;
