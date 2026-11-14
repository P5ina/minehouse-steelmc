//! Hopper block entity: pulls items from above and pushes them where it faces.

mod transfer;

use std::mem;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::ToNbtTag;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::BlockStateProperties;
use steel_registry::item_stack::ItemStack;
use steel_registry::vanilla_block_entity_types;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_utils::geometry::WorldAabb;
use steel_utils::{
    BlockPos, BlockStateId, Direction, Downcast as _, DowncastType, DowncastTypeKey,
    locks::SyncMutex,
};
use text_components::TextComponent;

use crate::behavior::blocks::{chest_container, composter_container};
use crate::block_entity::container_loot::{ContainerLoot, LOOT_TABLE_SEED_TAG, LOOT_TABLE_TAG};
use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::entity::entities::ItemEntity;
use crate::entity::{Entity as _, RemovalReason};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerId, ContainerLockGuard, ContainerRef, SharedContainer};
use crate::world::World;
use rand::RngExt as _;

pub(crate) use transfer::{add_item, is_full_container};

/// Number of slots in a hopper.
pub const HOPPER_SLOTS: usize = 5;

/// Ticks a hopper waits after moving an item.
pub(crate) const MOVE_ITEM_SPEED: i32 = 8;

/// Area above and inside a hopper where it picks up item entities, relative to its position.
const SUCK_AABB: WorldAabb = WorldAabb::new(0.0, 11.0 / 16.0, 0.0, 1.0, 2.0, 1.0);

/// Hopper block entity.
///
/// `CustomName` and `lock` are kept as loaded; Steel does not apply them yet.
pub struct HopperBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<HopperContainer>>,
    container_ref: ContainerRef,
    /// Vanilla `RandomizableContainerBlockEntity` loot table.
    loot: Arc<ContainerLoot>,
    preserved: SyncMutex<NbtCompound>,
}

/// Hopper inventory together with its transfer timing, so another hopper
/// filling this one can adjust the cooldown under the same container lock.
pub(crate) struct HopperContainer {
    items: Vec<ItemStack>,
    cooldown_time: i32,
    ticked_game_time: i64,
}

impl HopperContainer {
    const fn is_on_cooldown(&self) -> bool {
        self.cooldown_time > 0
    }

    const fn is_on_custom_cooldown(&self) -> bool {
        self.cooldown_time > MOVE_ITEM_SPEED
    }

    fn inventory_full(&self) -> bool {
        self.items
            .iter()
            .all(|item| !item.is_empty() && item.count() == item.max_stack_size())
    }
}

// SAFETY: This key is owned by Steel and uniquely identifies `HopperBlockEntity`.
unsafe impl DowncastType for HopperBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/hopper");
}

// SAFETY: This key is owned by Steel and uniquely identifies the lockable hopper inventory.
unsafe impl DowncastType for HopperContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/hopper");
}

/// What a hopper tries to pull in after pushing.
enum Intake<'a> {
    /// The container or item entities above (`suckInItems`).
    Above,
    /// One item entity that touched the hopper.
    Entity(&'a ItemEntity),
}

impl HopperBlockEntity {
    /// Creates a new hopper block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::HOPPER,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(HopperContainer {
            items: vec![ItemStack::empty(); HOPPER_SLOTS],
            cooldown_time: -1,
            ticked_game_time: 0,
        }));
        let shared_container: SharedContainer =
            Arc::<SyncMutex<HopperContainer>>::clone(&container);
        let loot = Arc::new(ContainerLoot::new());
        Self {
            container_ref: ContainerRef::owned_by_randomizable_block_entity(
                shared_container,
                Arc::clone(&base),
                Arc::clone(&loot),
            ),
            base,
            container,
            loot,
            preserved: SyncMutex::new(NbtCompound::new()),
        }
    }

    /// Returns the saved custom name, if any.
    #[must_use]
    pub fn custom_name(&self) -> Option<TextComponent> {
        let preserved = self.preserved.lock();
        TextComponent::from_nbt(preserved.get("CustomName")?)
    }

    /// The containers at `pos` a hopper or dropper can reach, in slot order.
    ///
    /// A double chest yields both halves. Vanilla `HopperBlockEntity.getContainerAt`.
    #[must_use]
    pub fn get_container_at(world: &Arc<World>, pos: BlockPos) -> Option<Vec<ContainerRef>> {
        let center = DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.5,
            f64::from(pos.z()) + 0.5,
        );
        get_container_at_point(world, pos, center)
    }

    /// Hopper behavior when an entity is inside the block.
    pub fn entity_inside(&self, world: &Arc<World>, entity: &ItemEntity) {
        if entity.get_item().is_empty() {
            return;
        }
        let pos = self.base.pos();
        let relative = entity.bounding_box().translate(-DVec3::new(
            f64::from(pos.x()),
            f64::from(pos.y()),
            f64::from(pos.z()),
        ));
        if relative.intersects(SUCK_AABB) {
            self.try_move_items(world, &Intake::Entity(entity));
        }
    }

    fn try_move_items(&self, world: &Arc<World>, intake: &Intake<'_>) -> bool {
        let state = self.base.block_state();
        let on_cooldown = self.container.lock().is_on_cooldown();
        if on_cooldown || !state.get_value(&BlockStateProperties::ENABLED) {
            return false;
        }
        // Vanilla `isEmpty` rolls a pending loot table first.
        self.container_ref.unpack_loot_table(None);
        let is_empty = self.container.lock().is_empty();

        let mut moved = false;
        if !is_empty {
            moved = self.eject_items(world, state);
        }
        if !self.container.lock().inventory_full() {
            moved |= match intake {
                Intake::Above => self.suck_in_items(world),
                Intake::Entity(entity) => self.add_item_entity(entity),
            };
        }
        if moved {
            self.container.lock().cooldown_time = MOVE_ITEM_SPEED;
            self.base.set_changed();
        }
        moved
    }

    fn eject_items(&self, world: &Arc<World>, state: BlockStateId) -> bool {
        let facing: Direction = state.get_value(&BlockStateProperties::FACING_HOPPER);
        let Some(targets) = Self::get_container_at(world, self.base.pos().relative(facing)) else {
            return false;
        };
        let direction = facing.opposite();
        let hopper = self.container_ref.container_id();
        let target_ids: Vec<ContainerId> = targets.iter().map(ContainerRef::container_id).collect();

        let mut locked = vec![self.container_ref.clone()];
        locked.extend(targets);
        let mut guard = ContainerLockGuard::lock_all(&locked);
        if is_full_container(&guard, &target_ids, direction) {
            return false;
        }

        for slot in 0..HOPPER_SLOTS {
            let Some(original) = guard
                .get(hopper)
                .map(|container| container.get_item(slot).clone())
                .filter(|item| !item.is_empty())
            else {
                continue;
            };
            let Some(container) = guard.get_mut(hopper) else {
                return false;
            };
            let one = container.get_item_mut(slot).split(1);
            let rest = add_item(&mut guard, Some(hopper), &target_ids, one, Some(direction));
            if rest.is_empty() {
                for &target in &target_ids {
                    guard.set_changed(target);
                }
                return true;
            }
            if let Some(container) = guard.get_mut(hopper) {
                *container.get_item_mut(slot) = original;
            }
        }
        false
    }

    fn suck_in_items(&self, world: &Arc<World>) -> bool {
        suck_in_items(world, &self.container_ref, self.level_position(), true)
    }

    /// Moves as much of `entity`'s stack into the hopper as fits.
    fn add_item_entity(&self, entity: &ItemEntity) -> bool {
        add_item_entity(&self.container_ref, entity)
    }

    /// Vanilla `HopperBlockEntity.getLevelX/Y/Z`: the block's center.
    fn level_position(&self) -> DVec3 {
        let pos = self.base.pos();
        DVec3::new(
            f64::from(pos.x()) + 0.5,
            f64::from(pos.y()) + 0.5,
            f64::from(pos.z()) + 0.5,
        )
    }
}

/// Vanilla `HopperBlockEntity.getContainerAt(level, pos, state, x, y, z)`: the
/// block's container, or else a container entity around `point`.
fn get_container_at_point(
    world: &Arc<World>,
    pos: BlockPos,
    point: DVec3,
) -> Option<Vec<ContainerRef>> {
    get_block_container(world, pos)
        .or_else(|| get_entity_container(world, point).map(|container| vec![container]))
}

/// Vanilla `HopperBlockEntity.getBlockContainer`.
fn get_block_container(world: &Arc<World>, pos: BlockPos) -> Option<Vec<ContainerRef>> {
    let state = world.get_block_state(pos);
    if let Some(composter) = composter_container(world, pos, state) {
        return Some(vec![composter]);
    }
    if let Some(chest) = chest_container(state, world, pos, true) {
        return (!chest.is_empty()).then_some(chest);
    }
    let container = world.get_block_entity(pos)?.container_ref()?;
    Some(vec![container])
}

/// Vanilla `HopperBlockEntity.getEntityContainer`: a random container entity
/// (chest or hopper minecart) in the block-sized box around `point`.
fn get_entity_container(world: &World, point: DVec3) -> Option<ContainerRef> {
    let area = WorldAabb::new(
        point.x - 0.5,
        point.y - 0.5,
        point.z - 0.5,
        point.x + 0.5,
        point.y + 0.5,
        point.z + 0.5,
    );
    let entities =
        world.get_entities_in_aabb_matching(&area, |entity| entity.container_entity().is_some());
    if entities.is_empty() {
        return None;
    }
    let index = rand::rng().random_range(0..entities.len());
    entities[index].container_entity()
}

/// Vanilla `HopperBlockEntity.suckInItems(level, hopper)`: pulls one item from
/// the container above `level_position`, or else picks up item entities there.
///
/// `grid_aligned` hoppers (the block) are blocked by a full block above;
/// hopper minecarts are not.
pub(crate) fn suck_in_items(
    world: &Arc<World>,
    hopper: &ContainerRef,
    level_position: DVec3,
    grid_aligned: bool,
) -> bool {
    let above = BlockPos::containing(level_position.x, level_position.y + 1.0, level_position.z);
    let source_point = DVec3::new(level_position.x, level_position.y + 1.0, level_position.z);
    if let Some(sources) = get_container_at_point(world, above, source_point) {
        let hopper_id = hopper.container_id();
        let source_ids: Vec<ContainerId> = sources.iter().map(ContainerRef::container_id).collect();
        let mut locked = vec![hopper.clone()];
        locked.extend(sources);
        let mut guard = ContainerLockGuard::lock_all(&locked);
        return transfer::get_slots(&guard, &source_ids, Some(Direction::Down))
            .into_iter()
            .any(|slot| {
                transfer::try_take_in_item_from_slot(
                    &mut guard,
                    hopper_id,
                    &source_ids,
                    slot,
                    Direction::Down,
                )
            });
    }

    let above_state = world.get_block_state(above);
    let blocks_intake = grid_aligned
        && world.is_collision_shape_full_block_at(above, above_state)
        && !above_state
            .get_block()
            .has_tag(&BlockTag::DOES_NOT_BLOCK_HOPPERS);
    if blocks_intake {
        return false;
    }

    let area = SUCK_AABB.translate(level_position - DVec3::splat(0.5));
    world
        .get_entities_in_aabb_matching(&area, |entity| {
            entity.is_alive() && entity.downcast_ref::<ItemEntity>().is_some()
        })
        .iter()
        .filter_map(|entity| entity.downcast_ref::<ItemEntity>())
        .any(|item| add_item_entity(hopper, item))
}

/// Vanilla `HopperBlockEntity.addItem(container, entity)`: moves as much of
/// `entity`'s stack into `hopper` as fits, discarding the entity when emptied.
pub(crate) fn add_item_entity(hopper: &ContainerRef, entity: &ItemEntity) -> bool {
    let hopper_id = hopper.container_id();
    let rest = {
        let mut guard = ContainerLockGuard::lock_all(&[hopper]);
        add_item(&mut guard, None, &[hopper_id], entity.get_item(), None)
    };
    if rest.is_empty() {
        entity.set_item(ItemStack::empty());
        entity.set_removed(RemovalReason::Discarded);
        true
    } else {
        entity.set_item(rest);
        false
    }
}

impl BlockEntity for HopperBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn tick(&self, world: &Arc<World>) {
        let ready = {
            let mut container = self.container.lock();
            container.cooldown_time -= 1;
            container.ticked_game_time = world.game_time();
            if container.is_on_cooldown() {
                false
            } else {
                container.cooldown_time = 0;
                true
            }
        };
        if ready {
            self.try_move_items(world, &Intake::Above);
        }
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        // Vanilla `Containers.dropContents` reads every slot, rolling the loot.
        self.container_ref.unpack_loot_table(None);
        let items = {
            let mut container = self.container.lock();
            mem::replace(&mut container.items, vec![ItemStack::empty(); HOPPER_SLOTS])
        };
        let Some(world) = self.get_level() else {
            return;
        };
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt_view: NbtCompoundView<'_, '_> = nbt.into();
        let mut container = self.container.lock();
        container.items.fill(ItemStack::empty());
        if !self.loot.load(nbt_view)
            && let Some(compounds) = nbt_view.list("Items").and_then(|list| list.compounds())
        {
            for compound in compounds {
                let Some(slot) = compound.byte("Slot") else {
                    continue;
                };
                let slot = slot as usize;
                if slot < HOPPER_SLOTS
                    && let Some(item) = ItemStack::from_borrowed_compound(&compound)
                {
                    container.items[slot] = item;
                }
            }
        }
        container.cooldown_time = nbt_view.int("TransferCooldown").unwrap_or(-1);

        let mut preserved = nbt_view.to_owned();
        for key in [
            "Items",
            "TransferCooldown",
            "components",
            LOOT_TABLE_TAG,
            LOOT_TABLE_SEED_TAG,
        ] {
            while preserved.remove(key).is_some() {}
        }
        *self.preserved.lock() = preserved;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        for (key, value) in self.preserved.lock().iter() {
            nbt.insert(key.to_string(), value.clone());
        }
        let container = self.container.lock();
        if !self.loot.save(nbt) {
            let items: Vec<NbtCompound> = container
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.is_empty())
                .filter_map(|(slot, item)| match item.clone().to_nbt_tag() {
                    NbtTag::Compound(mut item_nbt) => {
                        item_nbt.insert("Slot", slot as i8);
                        Some(item_nbt)
                    }
                    _ => None,
                })
                .collect();
            nbt.insert("Items", NbtList::Compound(items));
        }
        nbt.insert("TransferCooldown", container.cooldown_time);
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        None
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

impl Container for HopperContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn get_container_size(&self) -> usize {
        HOPPER_SLOTS
    }

    fn set_item(&mut self, slot: usize, mut stack: ItemStack) {
        if slot < HOPPER_SLOTS {
            let max_stack_size = self.get_max_stack_size_for_item(&stack);
            if !stack.is_empty() && stack.count() > max_stack_size {
                stack.set_count(max_stack_size);
            }
            self.items[slot] = stack;
        }
    }

    fn get_max_stack_size(&self) -> i32 {
        64
    }

    fn set_changed(&mut self) {}
}
