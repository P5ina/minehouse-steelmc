//! Vanilla `ItemFrame` and `GlowItemFrame`.

use std::sync::{Arc, Weak};

use glam::DVec3;
use rand::{RngExt as _, rng};
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::NbtCompound;
use steel_macros::entity_behavior;
use steel_math::DEGREE_90;
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::data_components::vanilla_components::{CUSTOM_NAME, MAP_DECORATIONS, MAP_ID};
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::item_stack::ItemStack;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_damage_type_tags::DamageTypeTag;
use steel_registry::vanilla_entity_data::ItemFrameEntityData;
use steel_registry::vanilla_game_rules::ENTITY_DROPS;
use steel_registry::{
    sound_events, vanilla_blocks, vanilla_entities, vanilla_game_events, vanilla_items,
};
use steel_utils::locks::SyncMutex;
use steel_utils::types::InteractionHand;
use steel_utils::{BlockPos, Direction, DowncastType, DowncastTypeKey, WorldAabb, axis::Axis};

use crate::behavior::{BLOCK_BEHAVIORS, InteractionResult};
use crate::entity::block_attached_entity::{BlockAttachedEntity, BlockAttachedEntityBase};
use crate::entity::damage::DamageSource;
use crate::entity::entities::LightningBoltEntity;
use crate::entity::{
    Entity, EntityBase, EntityBaseLoad, EntityBaseState, EntityMoveError, EntitySyncedData,
    HangingEntity, ItemFrame, SharedEntity,
};
use crate::physics::{MoveResult, MoverType};
use crate::player::Player;
use crate::world::maps::{MapFrame, TRACKED_DECORATION_LIMIT};
use crate::world::{Explosion, World};

/// Vanilla `ItemFrame.DEFAULT_DROP_CHANCE`.
const DEFAULT_DROP_CHANCE: f32 = 1.0;

/// Vanilla `ItemFrame.NUM_ROTATIONS`.
const NUM_ROTATIONS: i32 = 8;

struct FrameState {
    drop_chance: f32,
    fixed: bool,
}

/// Vanilla `ItemFrame`; with the glow item frame type it is vanilla `GlowItemFrame`, which only
/// changes the sounds and the dropped frame item.
#[entity_behavior(class = "ItemFrame", class = "GlowItemFrame")]
pub struct ItemFrameEntity {
    base: EntityBase,
    entity_type: EntityTypeRef,
    entity_data: SyncMutex<ItemFrameEntityData>,
    block_attached_entity_base: BlockAttachedEntityBase,
    state: SyncMutex<FrameState>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `ItemFrameEntity`.
unsafe impl DowncastType for ItemFrameEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:entity/item_frame");
}

impl ItemFrameEntity {
    /// Creates a fresh item frame from the generic entity factory path.
    #[must_use]
    pub fn new(entity_type: EntityTypeRef, id: i32, position: DVec3, world: Weak<World>) -> Self {
        Self::new_attached(
            entity_type,
            id,
            BlockPos::new(
                position.x.floor() as i32,
                position.y.floor() as i32,
                position.z.floor() as i32,
            ),
            Direction::South,
            world,
        )
    }

    /// Creates a fresh item frame attached to `block_pos`.
    #[must_use]
    pub fn new_attached(
        entity_type: EntityTypeRef,
        id: i32,
        block_pos: BlockPos,
        direction: Direction,
        world: Weak<World>,
    ) -> Self {
        let entity = Self {
            base: EntityBase::new_with_state(
                id,
                EntityBaseState::new_with_bounding_box(
                    Self::frame_center(block_pos, direction),
                    entity_type.dimensions,
                    Self::frame_bounding_box(block_pos, direction, false),
                )
                .with_rotation(Self::rotation_for_direction(direction)),
                world,
            ),
            entity_type,
            entity_data: SyncMutex::new(ItemFrameEntityData::new()),
            block_attached_entity_base: BlockAttachedEntityBase::new(block_pos),
            state: SyncMutex::new(FrameState {
                drop_chance: DEFAULT_DROP_CHANCE,
                fixed: false,
            }),
        };
        entity
            .entity_data
            .lock()
            .hanging_entity
            .direction
            .set(direction);
        entity
    }

    /// Creates an item frame from persistent entity data.
    #[must_use]
    pub fn from_saved(entity_type: EntityTypeRef, load: EntityBaseLoad) -> Self {
        let position = load.position;
        Self {
            base: EntityBase::from_load(load, entity_type.dimensions),
            entity_type,
            entity_data: SyncMutex::new(ItemFrameEntityData::new()),
            block_attached_entity_base: BlockAttachedEntityBase::new(BlockPos::new(
                position.x.floor() as i32,
                position.y.floor() as i32,
                position.z.floor() as i32,
            )),
            state: SyncMutex::new(FrameState {
                drop_chance: DEFAULT_DROP_CHANCE,
                fixed: false,
            }),
        }
    }

    fn is_glow(&self) -> bool {
        self.entity_type == &vanilla_entities::GLOW_ITEM_FRAME
    }

    fn remove_item_sound(&self) -> SoundEventRef {
        if self.is_glow() {
            &sound_events::ENTITY_GLOW_ITEM_FRAME_REMOVE_ITEM
        } else {
            &sound_events::ENTITY_ITEM_FRAME_REMOVE_ITEM
        }
    }

    fn break_sound(&self) -> SoundEventRef {
        if self.is_glow() {
            &sound_events::ENTITY_GLOW_ITEM_FRAME_BREAK
        } else {
            &sound_events::ENTITY_ITEM_FRAME_BREAK
        }
    }

    /// The sound of hanging this frame up.
    #[must_use]
    pub fn place_sound(&self) -> SoundEventRef {
        if self.is_glow() {
            &sound_events::ENTITY_GLOW_ITEM_FRAME_PLACE
        } else {
            &sound_events::ENTITY_ITEM_FRAME_PLACE
        }
    }

    fn add_item_sound(&self) -> SoundEventRef {
        if self.is_glow() {
            &sound_events::ENTITY_GLOW_ITEM_FRAME_ADD_ITEM
        } else {
            &sound_events::ENTITY_ITEM_FRAME_ADD_ITEM
        }
    }

    fn rotate_item_sound(&self) -> SoundEventRef {
        if self.is_glow() {
            &sound_events::ENTITY_GLOW_ITEM_FRAME_ROTATE_ITEM
        } else {
            &sound_events::ENTITY_ITEM_FRAME_ROTATE_ITEM
        }
    }

    /// The framed item.
    #[must_use]
    pub fn item(&self) -> ItemStack {
        self.entity_data.lock().item.get().clone()
    }

    /// The rotation of the framed item, in eighths of a turn.
    #[must_use]
    pub fn item_rotation(&self) -> i32 {
        *self.entity_data.lock().rotation.get()
    }

    /// Whether the frame is fixed: it cannot be broken, emptied or rotated in survival.
    #[must_use]
    pub fn is_fixed(&self) -> bool {
        self.state.lock().fixed
    }

    /// Sets the framed item, matching vanilla by storing a single item.
    pub fn set_item(&self, item: ItemStack) {
        self.set_item_with_update(item, true);
    }

    /// Vanilla `ItemFrame.setItem` with its comparator-update flag.
    pub(crate) fn set_item_with_update(&self, item: ItemStack, update_comparators: bool) {
        let has_item = !item.is_empty();
        self.store_item(item);
        if has_item {
            self.play_sound(self.add_item_sound(), 1.0, 1.0);
        }
        if update_comparators {
            self.update_comparators();
        }
    }

    /// Stores the item without the sound and neighbor update `setItem` makes.
    fn store_item(&self, mut item: ItemStack) {
        if !item.is_empty() {
            item.set_count(1);
        }
        self.entity_data.lock().item.set(item);
        self.recalculate_position_or_warn();
    }

    /// Vanilla `ItemFrame.setRotation` with its comparator-update flag.
    fn set_item_rotation(&self, rotation: i32, update_comparators: bool) {
        self.entity_data
            .lock()
            .rotation
            .set(rotation % NUM_ROTATIONS);
        if update_comparators {
            self.update_comparators();
        }
    }

    fn update_comparators(&self) {
        if let Some(world) = self.level() {
            world.update_neighbor_for_output_signal(
                self.block_attached_entity_base.pos(),
                &vanilla_blocks::AIR,
            );
        }
    }

    fn set_direction(&self, direction: Direction) {
        self.entity_data
            .lock()
            .hanging_entity
            .direction
            .set(direction);
        self.base
            .set_rotation(Self::rotation_for_direction(direction));
        self.recalculate_position_or_warn();
    }

    fn try_recalculate_position(&self) -> Result<(), EntityMoveError> {
        let block_pos = self.block_attached_entity_base.pos();
        let direction = *self.entity_data.lock().hanging_entity.direction.get();
        let position = Self::frame_center(block_pos, direction);
        self.base.try_set_position(position)?;
        self.base.set_bounding_box(Self::frame_bounding_box(
            block_pos,
            direction,
            self.has_framed_map(),
        ));

        Ok(())
    }

    fn recalculate_position_or_warn(&self) {
        if let Err(error) = self.try_recalculate_position() {
            log::warn!(
                "failed to commit item frame {} position recalculation: {error}",
                self.base.id()
            );
        }
    }

    fn has_framed_map(&self) -> bool {
        self.entity_data.lock().item.get().has(MAP_ID)
    }

    /// Vanilla `ItemFrame.getFrameItemStackWithData`.
    fn frame_item_with_data(&self) -> ItemStack {
        let mut item = ItemStack::new(if self.is_glow() {
            &vanilla_items::GLOW_ITEM_FRAME
        } else {
            &vanilla_items::ITEM_FRAME
        });
        if let Some(name) = self.custom_name() {
            item.set(CUSTOM_NAME, name);
        }
        item
    }

    /// Vanilla `ItemFrame.dropItem(level, causedBy, withFrame)`.
    fn drop_contents(&self, world: &World, caused_by: Option<&dyn Entity>, with_frame: bool) {
        if self.is_fixed() {
            return;
        }
        let item = self.item();
        self.set_item(ItemStack::empty());
        if !world.get_game_rule(&ENTITY_DROPS) {
            if caused_by.is_none() {
                self.remove_framed_map(&item);
            }
        } else if caused_by
            .and_then(Entity::as_player)
            .is_some_and(Player::has_infinite_materials)
        {
            self.remove_framed_map(&item);
        } else {
            if with_frame {
                self.spawn_in_front(self.frame_item_with_data());
            }
            if !item.is_empty() {
                self.remove_framed_map(&item);
                let drop_chance = self.state.lock().drop_chance;
                if rng().random::<f32>() < drop_chance {
                    self.spawn_in_front(item);
                }
            }
        }
    }

    /// Vanilla `ItemFrame.removeFramedMap`: the map forgets its marker for this frame.
    fn remove_framed_map(&self, item: &ItemStack) {
        if let Some(id) = item.get(MAP_ID).copied()
            && let Some(world) = self.level()
            && let Some(data) = world.map_data(id)
        {
            data.lock()
                .removed_from_frame(self.block_attached_entity_base.pos(), self.id());
        }
    }

    /// The marker this frame puts on the map it holds.
    #[must_use]
    pub fn map_frame(&self) -> MapFrame {
        MapFrame {
            pos: self.block_attached_entity_base.pos(),
            rotation: i32::from(direction_2d_data_value(self.direction())) * 90,
            entity_id: self.id(),
        }
    }

    /// The framed-map part of vanilla `ServerEntity.sendChanges`: every player in the world
    /// carries the framed map and receives its changes.
    pub fn sync_framed_map(&self, world: &World) {
        let item = self.item();
        if !item.is(&vanilla_items::FILLED_MAP) {
            return;
        }
        let Some(id) = item.get(MAP_ID).copied() else {
            return;
        };
        let Some(data) = world.map_data(id) else {
            return;
        };
        let static_decorations = item.get(MAP_DECORATIONS).cloned();
        let frame = self.map_frame();
        let mut data = data.lock();
        world.players.iter_players(|_, player| {
            data.tick_carried_by(player, Some(id), static_decorations.as_ref(), Some(frame));
            if let Some(packet) = data.update_packet(id, player) {
                player.send_packet(packet);
            }
            true
        });
    }

    fn frame_center(block_pos: BlockPos, direction: Direction) -> DVec3 {
        let off = direction.offset_vec().as_dvec3() * 0.46875;
        block_pos.0.as_dvec3() + DVec3::splat(0.5) - off
    }

    fn rotation_for_direction(direction: Direction) -> (f32, f32) {
        if direction.is_horizontal() {
            (
                f32::from(direction_2d_data_value(direction)) * DEGREE_90,
                0.0,
            )
        } else {
            let pitch = match direction {
                Direction::Up => -DEGREE_90,
                Direction::Down => DEGREE_90,
                Direction::North | Direction::South | Direction::West | Direction::East => 0.0,
            };
            (0.0, pitch)
        }
    }

    fn frame_bounding_box(
        block_pos: BlockPos,
        direction: Direction,
        has_framed_map: bool,
    ) -> WorldAabb {
        let center = Self::frame_center(block_pos, direction);
        let size = if has_framed_map { 1.0 } else { 0.75 };
        let x_size = if direction.axis() == Axis::X {
            0.0625
        } else {
            size
        };
        let y_size = if direction.axis() == Axis::Y {
            0.0625
        } else {
            size
        };
        let z_size = if direction.axis() == Axis::Z {
            0.0625
        } else {
            size
        };
        WorldAabb::new(
            center.x - x_size / 2.0,
            center.y - y_size / 2.0,
            center.z - z_size / 2.0,
            center.x + x_size / 2.0,
            center.y + y_size / 2.0,
            center.z + z_size / 2.0,
        )
    }
}

/// Vanilla `ItemFrame.canHurtWhenFixed`.
fn can_hurt_when_fixed(source: &DamageSource) -> bool {
    source.bypasses_invulnerability() || source.is_creative_player()
}

impl HangingEntity for ItemFrameEntity {
    fn direction(&self) -> Direction {
        *self.entity_data.lock().hanging_entity.direction.get()
    }

    /// Vanilla `ItemFrame.getPopBox`: the frame's box as if it held no map.
    fn pop_box(&self) -> WorldAabb {
        Self::frame_bounding_box(
            self.block_attached_entity_base.pos(),
            self.direction(),
            false,
        )
    }
}

impl ItemFrame for ItemFrameEntity {
    fn analog_output(&self) -> i32 {
        let entity_data = self.entity_data.lock();
        if entity_data.item.get().is_empty() {
            0
        } else {
            *entity_data.rotation.get() % NUM_ROTATIONS + 1
        }
    }
}

impl Entity for ItemFrameEntity {
    /// Vanilla `BlockAttachedEntity.thunderHit` ignores lightning.
    fn thunder_hit(&self, _world: &World, _lightning: &LightningBoltEntity) {}

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn entity_type(&self) -> EntityTypeRef {
        self.entity_type
    }

    fn spawn_data(&self) -> i32 {
        direction_3d_data_value(*self.entity_data.lock().hanging_entity.direction.get())
    }

    fn spawn_position(&self) -> DVec3 {
        let block_pos = self.block_attached_entity_base.pos();
        DVec3::new(
            f64::from(block_pos.x()),
            f64::from(block_pos.y()),
            f64::from(block_pos.z()),
        )
    }

    fn synced_data(&self) -> Option<&dyn EntitySyncedData> {
        Some(&self.entity_data)
    }

    fn tick(self: Arc<Self>) {
        self.tick_block_attached_entity();
    }

    fn is_pickable(&self) -> bool {
        self.is_pickable_block_attached_entity()
    }

    fn ignore_explosion(&self, explosion: &dyn Explosion) -> bool {
        self.ignore_explosion_block_attached_entity(explosion)
    }

    /// Vanilla `ItemFrame.hurtServer`: the first hit knocks the item out, the next breaks the
    /// frame; a fixed frame only breaks for creative players and invulnerability bypasses.
    fn hurt(&self, world: &World, source: &DamageSource, amount: f32) -> bool {
        if self.is_fixed() {
            return can_hurt_when_fixed(source)
                && self.hurt_block_attached_entity(world, source, amount);
        }
        if self.is_invulnerable_to_base(source) {
            return false;
        }
        if !source.is(&DamageTypeTag::IS_EXPLOSION) && !self.item().is_empty() {
            let causing_entity = source.causing_entity();
            let caused_by = causing_entity.map(AsRef::as_ref);
            self.drop_contents(world, caused_by, false);
            self.game_event_with_source_entity(&vanilla_game_events::BLOCK_CHANGE, caused_by);
            self.play_sound(self.remove_item_sound(), 1.0, 1.0);
            return true;
        }
        self.hurt_block_attached_entity(world, source, amount)
    }

    /// Vanilla `BlockAttachedEntity.kill`, which lets the frame forget its map first.
    fn kill(&self, _world: &World) {
        self.kill_attributed(None);
    }

    fn skip_attack_interaction(&self, source: &SharedEntity) -> bool {
        self.skip_attack_interaction_block_attached_entity(source)
    }

    fn push_impulse(&self, impulse: DVec3) {
        if !self.is_fixed() {
            self.push_impulse_block_attached_entity(impulse);
        }
    }

    fn move_entity(self: Arc<Self>, mover_type: MoverType, delta: DVec3) -> Option<MoveResult> {
        if self.is_fixed() {
            return None;
        }
        self.move_entity_block_attached_entity(mover_type, delta)
    }

    fn refresh_dimensions(&self) {
        self.refresh_dimensions_block_attached_entity();
    }

    fn try_set_position(&self, pos: DVec3) -> Result<(), EntityMoveError> {
        self.try_set_position_block_attached_entity(pos)
    }

    /// Vanilla `ItemFrame.interact`: puts the held item in an empty frame, otherwise turns the
    /// framed item.
    fn interact(
        &self,
        player: &Player,
        hand: InteractionHand,
        _location: DVec3,
    ) -> InteractionResult {
        if self.is_fixed() {
            return InteractionResult::Pass;
        }
        if !self.item().is_empty() {
            self.play_sound(self.rotate_item_sound(), 1.0, 1.0);
            self.set_item_rotation(self.item_rotation() + 1, true);
            self.game_event_with_source_entity(&vanilla_game_events::BLOCK_CHANGE, Some(player));
            return InteractionResult::Success;
        }
        let held = player.inventory.lock().get_item_in_hand(hand).clone();
        if held.is_empty() || self.is_removed() {
            return InteractionResult::Pass;
        }
        if let Some(id) = held.get(MAP_ID).copied()
            && let Some(data) = player.get_world().map_data(id)
            && data
                .lock()
                .is_tracked_count_over_limit(TRACKED_DECORATION_LIMIT)
        {
            return InteractionResult::Fail;
        }
        self.set_item(held);
        self.game_event_with_source_entity(&vanilla_game_events::BLOCK_CHANGE, Some(player));
        let has_infinite_materials = player.has_infinite_materials();
        player
            .inventory
            .lock()
            .mutate_item_in_hand(hand, |item| item.consume(1, has_infinite_materials));
        InteractionResult::Success
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.save_block_attached_entity(nbt);

        let invisible = self.is_invisible();
        let state = self.state.lock();
        let entity_data = self.entity_data.lock();
        let item = entity_data.item.get();
        if !item.is_empty() {
            nbt.insert("Item", item.to_nbt_tag_ref());
        }
        nbt.insert("ItemRotation", *entity_data.rotation.get() as i8);
        nbt.insert("ItemDropChance", state.drop_chance);
        nbt.insert(
            "Facing",
            direction_3d_data_value(*entity_data.hanging_entity.direction.get()) as i8,
        );
        nbt.insert("Invisible", i8::from(invisible));
        nbt.insert("Fixed", i8::from(state.fixed));
    }

    /// Vanilla `ItemFrame.readAdditionalSaveData`.
    ///
    /// Vanilla's `setItem` here also plays the add-item sound to players nearby whenever a chunk
    /// with filled frames loads; the item is stored silently instead.
    fn load_additional(&self, nbt: BorrowedNbtCompoundView<'_, '_>) {
        self.load_block_attached_entity(nbt);

        let item = nbt
            .compound("Item")
            .and_then(|item_tag| ItemStack::from_borrowed_compound(&item_tag))
            .unwrap_or_else(ItemStack::empty);
        let current = self.item();
        if !current.is_empty() && !ItemStack::matches(&item, &current) {
            self.remove_framed_map(&current);
        }
        self.store_item(item);
        self.set_item_rotation(nbt.byte("ItemRotation").map_or(0, i32::from), false);
        {
            let mut state = self.state.lock();
            state.drop_chance = nbt.float("ItemDropChance").unwrap_or(DEFAULT_DROP_CHANCE);
            state.fixed = nbt.byte("Fixed").is_some_and(|fixed| fixed != 0);
        }

        let facing = nbt
            .byte("Facing")
            .and_then(|value| direction_from_3d_data_value(i32::from(value)))
            .or_else(|| nbt.int("Facing").and_then(direction_from_3d_data_value))
            .unwrap_or(Direction::Down);
        self.set_direction(facing);
        EntitySyncedData::set_base_invisible_flag(
            &self.entity_data,
            nbt.byte("Invisible")
                .is_some_and(|invisible| invisible != 0),
        );
    }
}

impl BlockAttachedEntity for ItemFrameEntity {
    fn block_attached_entity_base(&self) -> &BlockAttachedEntityBase {
        &self.block_attached_entity_base
    }

    /// Vanilla `ItemFrame.survives`: the frame needs free space and a solid block behind it (or
    /// a repeater or comparator for a frame on a wall).
    fn survives(&self) -> bool {
        if self.is_fixed() {
            return true;
        }
        let Some(world) = self.level() else {
            return true;
        };
        if self.has_level_collision(&world, &self.pop_box()) {
            return false;
        }
        let direction = self.direction();
        let behind = world.get_block_state(
            self.block_attached_entity_base
                .pos()
                .relative(direction.opposite()),
        );
        let supported = behind.is_solid()
            || (direction.is_horizontal()
                && BLOCK_BEHAVIORS.get_behavior(behind.get_block()).is_diode());
        supported && self.can_coexist(&world, true)
    }

    /// Vanilla `ItemFrame.dropItem(level, causedBy)`: the frame breaks with its item.
    fn drop_item(&self, caused_by: Option<&dyn Entity>) {
        let Some(world) = self.level() else {
            return;
        };
        self.play_sound(self.break_sound(), 1.0, 1.0);
        self.drop_contents(&world, caused_by, true);
        self.game_event_with_source_entity(&vanilla_game_events::BLOCK_CHANGE, caused_by);
    }

    /// Vanilla `ItemFrame.onKilled`.
    fn on_killed(&self) {
        self.remove_framed_map(&self.item());
    }

    fn recalculate_bounding_box(&self) -> Result<(), EntityMoveError> {
        self.try_recalculate_position()
    }
}

const fn direction_3d_data_value(direction: Direction) -> i32 {
    match direction {
        Direction::Down => 0,
        Direction::Up => 1,
        Direction::North => 2,
        Direction::South => 3,
        Direction::West => 4,
        Direction::East => 5,
    }
}

const fn direction_from_3d_data_value(value: i32) -> Option<Direction> {
    match value {
        0 => Some(Direction::Down),
        1 => Some(Direction::Up),
        2 => Some(Direction::North),
        3 => Some(Direction::South),
        4 => Some(Direction::West),
        5 => Some(Direction::East),
        _ => None,
    }
}

const fn direction_2d_data_value(direction: Direction) -> u8 {
    match direction {
        Direction::South | Direction::Down | Direction::Up => 0,
        Direction::West => 1,
        Direction::North => 2,
        Direction::East => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;
    use steel_registry::items::item::BlockHitResult;
    use steel_registry::{
        RegistryReference, init_vanilla_registry, vanilla_damage_types,
        vanilla_map_decoration_types,
    };
    use steel_utils::ChunkPos;
    use steel_utils::Downcast as _;
    use steel_utils::types::UpdateFlags;

    use crate::behavior::items::{ItemFrameItem, create_map};
    use crate::behavior::{ItemBehavior as _, UseOnContext, init_behaviors};
    use crate::entity::entities::ItemEntity;
    use crate::entity::next_entity_id;
    use crate::player::ResetReason;
    use crate::test_support::{
        TestPlayerBuilder, dropped_items, fresh_test_world, insert_ready_full_chunk,
    };
    use crate::world::maps::MapItemSavedData;

    fn player_attack(player: &Arc<Player>) -> DamageSource {
        let player: SharedEntity = Arc::clone(player) as SharedEntity;
        DamageSource::environment(&vanilla_damage_types::PLAYER_ATTACK)
            .with_causing_entity(Arc::clone(&player))
            .with_direct_entity(player)
    }

    fn frames_at(world: &World, pos: BlockPos) -> Vec<SharedEntity> {
        world.get_entities_in_aabb_matching(
            &WorldAabb::new(
                f64::from(pos.x()),
                f64::from(pos.y()),
                f64::from(pos.z()),
                f64::from(pos.x() + 1),
                f64::from(pos.y() + 1),
                f64::from(pos.z() + 1),
            ),
            |entity| entity.as_item_frame().is_some(),
        )
    }

    fn dropped_stacks(world: &World, center: DVec3) -> Vec<ItemStack> {
        dropped_items(world, center)
            .iter()
            .filter_map(|entity| {
                entity
                    .downcast_ref::<ItemEntity>()
                    .map(ItemEntity::get_item)
            })
            .collect()
    }

    #[test]
    fn frames_hang_on_walls_hold_one_item_and_break_in_two_hits() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("item_frame_lifecycle");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let wall = BlockPos::new(2, 64, 2);
        assert!(world.set_block(
            wall,
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let player = TestPlayerBuilder::new(Arc::clone(world), "Framer", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        player.inventory.lock().set_item_in_hand(
            InteractionHand::MainHand,
            ItemStack::with_count(&vanilla_items::GLOW_ITEM_FRAME, 2),
        );

        let hit = |direction: Direction| BlockHitResult {
            location: wall.0.as_dvec3() + DVec3::splat(0.5),
            direction,
            block_pos: wall,
            miss: false,
            inside: false,
            world_border_hit: false,
        };
        let item = ItemFrameItem::new(&vanilla_entities::GLOW_ITEM_FRAME);
        let mut context = UseOnContext::new(
            &player,
            InteractionHand::MainHand,
            hit(Direction::East),
            world,
            Arc::clone(&player.inventory),
        );
        assert_eq!(item.use_on(&mut context), InteractionResult::Success);
        let frame_pos = wall.relative(Direction::East);
        let frames = frames_at(world, frame_pos);
        assert_eq!(frames.len(), 1);
        let frame = frames[0]
            .downcast_ref::<ItemFrameEntity>()
            .expect("an item frame entity");
        assert_eq!(frame.entity_type(), &vanilla_entities::GLOW_ITEM_FRAME);
        assert_eq!(frame.direction(), Direction::East);
        assert!(frame.survives());

        // A second frame cannot hang in the same spot facing the same way.
        let mut context = UseOnContext::new(
            &player,
            InteractionHand::MainHand,
            hit(Direction::East),
            world,
            Arc::clone(&player.inventory),
        );
        assert_eq!(item.use_on(&mut context), InteractionResult::Consume);
        assert_eq!(frames_at(world, frame_pos).len(), 1);

        player.inventory.lock().set_item_in_hand(
            InteractionHand::MainHand,
            ItemStack::with_count(&vanilla_items::DIAMOND, 2),
        );
        let held = || {
            player
                .inventory
                .lock()
                .get_item_in_hand(InteractionHand::MainHand)
                .count()
        };
        assert_eq!(
            frame.interact(&player, InteractionHand::MainHand, DVec3::ZERO),
            InteractionResult::Success
        );
        assert!(frame.item().is(&vanilla_items::DIAMOND));
        assert_eq!(frame.item().count(), 1);
        assert_eq!(held(), 1);
        frame.interact(&player, InteractionHand::MainHand, DVec3::ZERO);
        assert_eq!(frame.item_rotation(), 1);
        assert_eq!(held(), 1);
        assert_eq!(frame.analog_output(), 2);

        // The first hit knocks the item out, the second breaks the frame.
        let center = frame.position();
        assert!(frame.hurt(world, &player_attack(&player), 1.0));
        assert!(frame.item().is_empty());
        assert!(!frame.is_removed());
        let drops = dropped_stacks(world, center);
        assert_eq!(drops.len(), 1);
        assert!(drops[0].is(&vanilla_items::DIAMOND));

        assert!(frame.hurt(world, &player_attack(&player), 1.0));
        assert!(frame.is_removed());
        let drops = dropped_stacks(world, center);
        assert_eq!(drops.len(), 2);
        assert!(
            drops
                .iter()
                .any(|stack| stack.is(&vanilla_items::GLOW_ITEM_FRAME))
        );
    }

    #[test]
    fn frames_need_a_solid_block_behind_them_unless_fixed() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("item_frame_support");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let wall = BlockPos::new(4, 64, 4);
        assert!(world.set_block(
            wall,
            vanilla_blocks::STONE.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        let frame = ItemFrameEntity::new_attached(
            &vanilla_entities::ITEM_FRAME,
            next_entity_id(),
            wall.above(),
            Direction::Up,
            Arc::downgrade(world),
        );
        assert!(frame.survives());
        assert!(world.set_block(
            wall,
            vanilla_blocks::AIR.default_state(),
            UpdateFlags::UPDATE_ALL,
        ));
        assert!(!frame.survives());

        frame.state.lock().fixed = true;
        assert!(frame.survives());
        let mut nbt = NbtCompound::new();
        frame.save_additional(&mut nbt);
        assert_eq!(nbt.byte("Fixed"), Some(1));
    }

    #[test]
    fn framed_maps_mark_their_frame_until_taken_out() {
        init_vanilla_registry();
        init_behaviors();
        let fixture = fresh_test_world("item_frame_map");
        let world = &fixture.world;
        insert_ready_full_chunk(world, ChunkPos::new(0, 0));
        let player = TestPlayerBuilder::new(Arc::clone(world), "Viewer", next_entity_id()).build();
        assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
        let map = create_map(world, 0, 0, 0, true, false);
        let id = *map.get(MAP_ID).expect("map id");
        let frame_pos = BlockPos::new(10, 64, -6);
        let frame = ItemFrameEntity::new_attached(
            &vanilla_entities::ITEM_FRAME,
            next_entity_id(),
            frame_pos,
            Direction::West,
            Arc::downgrade(world),
        );
        frame.set_item_with_update(map, false);
        frame.sync_framed_map(world);

        let data = world.map_data(id).expect("map data");
        {
            let data = data.lock();
            let frames: Vec<_> = data.frames().copied().collect();
            assert_eq!(frames, vec![frame.map_frame()]);
            assert_eq!(frames[0].rotation, 90);
            let decorations: Vec<_> = data.decorations().collect();
            assert_eq!(decorations.len(), 1);
            assert_eq!(
                decorations[0].decoration_type,
                RegistryReference::new(&vanilla_map_decoration_types::FRAME)
            );
            assert_eq!((decorations[0].x, decorations[0].y), (20, -11));
            assert_eq!(decorations[0].rot, 4);

            let reloaded =
                MapItemSavedData::from_nbt_bytes(&data.to_nbt_bytes()).expect("the map reloads");
            assert_eq!(reloaded.frames().copied().collect::<Vec<_>>(), frames);
            assert_eq!(reloaded.decorations().count(), 1);
        }

        assert!(frame.hurt(world, &player_attack(&player), 1.0));
        let data = data.lock();
        assert_eq!(data.frames().count(), 0);
        assert_eq!(data.decorations().count(), 0);
    }

    #[test]
    fn item_frame_persists_structure_marker_state() {
        let frame = ItemFrameEntity::new_attached(
            &vanilla_entities::ITEM_FRAME,
            1,
            BlockPos::new(12, 80, 14),
            Direction::West,
            Weak::new(),
        );
        frame.set_item(ItemStack::new(&vanilla_items::ELYTRA));

        let mut nbt = NbtCompound::new();
        frame.save_additional(&mut nbt);

        assert_eq!(nbt.byte("Facing"), Some(4));
        assert_eq!(nbt.byte("ItemRotation"), Some(0));
        assert_eq!(nbt.float("ItemDropChance"), Some(1.0));
        assert_eq!(nbt.byte("Invisible"), Some(0));
        assert_eq!(nbt.byte("Fixed"), Some(0));
        let Some(item) = nbt.compound("Item") else {
            panic!("item frame should save framed item");
        };
        assert_eq!(
            item.string("id").map(ToString::to_string),
            Some("minecraft:elytra".to_owned())
        );
        assert_eq!(item.int("count"), Some(1));
    }

    #[test]
    fn item_frame_is_pickable_like_vanilla() {
        let frame = ItemFrameEntity::new_attached(
            &vanilla_entities::ITEM_FRAME,
            1,
            BlockPos::new(12, 80, 14),
            Direction::West,
            Weak::new(),
        );

        assert!(frame.is_pickable());
    }

    #[test]
    fn analog_output_uses_item_presence_and_rotation() {
        let frame = ItemFrameEntity::new_attached(
            &vanilla_entities::ITEM_FRAME,
            1,
            BlockPos::new(12, 80, 14),
            Direction::West,
            Weak::new(),
        );
        assert_eq!(frame.analog_output(), 0);

        frame.set_item_with_update(ItemStack::new(&vanilla_items::ELYTRA), false);
        assert_eq!(frame.analog_output(), 1);
        frame.entity_data.lock().rotation.set(7);
        assert_eq!(frame.analog_output(), 8);
    }
}
