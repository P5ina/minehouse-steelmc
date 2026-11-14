//! Vanilla `DecoratedPotBlockEntity`: the pot's four sherd faces and the one
//! stack of items it holds.

use std::mem;
use std::sync::{Arc, Weak};

use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::NbtCompound;
use simdnbt::{FromNbtTag as _, ToNbtTag as _};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, Direction};
use steel_registry::data_components::vanilla_components::{CONTAINER, POT_DECORATIONS};
use steel_registry::data_components::{DataComponentMap, ItemContainerContents, PotDecorations};
use steel_registry::item_stack::ItemStack;
use steel_registry::{ItemStackTemplate, vanilla_block_entity_types, vanilla_items};
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex};

use crate::block_entity::container_loot::ContainerLoot;
use crate::block_entity::{BlockEntity, BlockEntityBase, ImplicitComponentGetter};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::world::World;

/// NBT key of the pot's sherds.
const SHERDS_TAG: &str = "sherds";
/// NBT key of the pot's stored stack.
const ITEM_TAG: &str = "item";
/// Vanilla `DecoratedPotBlockEntity.EVENT_POT_WOBBLES`.
pub const EVENT_POT_WOBBLES: i32 = 1;

/// Vanilla `DecoratedPotBlockEntity.WobbleStyle`: how the pot shakes when a
/// player uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WobbleStyle {
    /// An item went in.
    Positive,
    /// Nothing went in.
    Negative,
}

impl WobbleStyle {
    const ALL: [Self; 2] = [Self::Positive, Self::Negative];

    /// The ordinal sent as the block event data.
    #[must_use]
    pub const fn ordinal(self) -> i32 {
        match self {
            Self::Positive => 0,
            Self::Negative => 1,
        }
    }

    /// How many ticks the wobble lasts.
    #[must_use]
    pub const fn duration(self) -> i32 {
        match self {
            Self::Positive => 7,
            Self::Negative => 10,
        }
    }

    fn from_ordinal(ordinal: i32) -> Option<Self> {
        Self::ALL.get(usize::try_from(ordinal).ok()?).copied()
    }
}

/// The last wobble a pot received (Vanilla `wobbleStartedAtTick` and
/// `lastWobbleStyle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wobble {
    /// Game time when the wobble started.
    pub started_at_tick: i64,
    /// The wobble's style.
    pub style: WobbleStyle,
}

/// The pot's single slot (Vanilla `ContainerSingleItem`).
struct PotContainer {
    item: [ItemStack; 1],
}

// SAFETY: This key is owned by Steel and uniquely identifies the independently
// lockable inventory data used by a decorated pot block entity.
unsafe impl DowncastType for PotContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/decorated_pot");
}

impl Container for PotContainer {
    fn items(&self) -> &[ItemStack] {
        &self.item
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.item
    }

    fn set_changed(&mut self) {}
}

/// Decorated pot block entity.
pub struct DecoratedPotBlockEntity {
    base: Arc<BlockEntityBase>,
    decorations: SyncMutex<PotDecorations>,
    container: Arc<SyncMutex<PotContainer>>,
    container_ref: ContainerRef,
    /// Vanilla `RandomizableContainer` loot table.
    loot: Arc<ContainerLoot>,
    wobble: SyncMutex<Option<Wobble>>,
}

// SAFETY: This key is owned by Steel and uniquely identifies `DecoratedPotBlockEntity`.
unsafe impl DowncastType for DecoratedPotBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/decorated_pot");
}

impl DecoratedPotBlockEntity {
    /// Creates an undecorated, empty pot.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::DECORATED_POT,
            level,
            pos,
            state,
        ));
        let container = Arc::new(SyncMutex::new(PotContainer {
            item: [ItemStack::empty()],
        }));
        let shared_container: SharedContainer = Arc::<SyncMutex<PotContainer>>::clone(&container);
        let loot = Arc::new(ContainerLoot::new());
        Self {
            container_ref: ContainerRef::owned_by_randomizable_block_entity(
                shared_container,
                Arc::clone(&base),
                Arc::clone(&loot),
            ),
            loot,
            base,
            decorations: SyncMutex::new(PotDecorations::EMPTY),
            container,
            wobble: SyncMutex::new(None),
        }
    }

    /// Vanilla `getDirection`: the pot's horizontal facing.
    #[must_use]
    pub fn direction(&self) -> Direction {
        self.base
            .block_state()
            .get_value(&BlockStateProperties::HORIZONTAL_FACING)
    }

    /// The pot's sherd faces.
    #[must_use]
    pub fn decorations(&self) -> PotDecorations {
        self.decorations.lock().clone()
    }

    /// Vanilla `createDecoratedPotInstance`: a decorated pot item showing
    /// `decorations`.
    #[must_use]
    pub fn create_decorated_pot_instance(decorations: PotDecorations) -> ItemStack {
        let mut stack = ItemStack::new(&vanilla_items::DECORATED_POT);
        stack.set(POT_DECORATIONS, decorations);
        stack
    }

    /// Vanilla `getTheItem`: rolls a pending loot table first.
    #[must_use]
    pub fn the_item(&self) -> ItemStack {
        self.container_ref.unpack_loot_table(None);
        self.container.lock().item[0].clone()
    }

    /// Vanilla `setTheItem`, rolling a pending loot table first.
    pub fn set_the_item(&self, stack: ItemStack) {
        self.container_ref.unpack_loot_table(None);
        self.container.lock().item[0] = stack;
    }

    /// Vanilla `potItem.grow(1)` on the stored stack.
    pub fn grow_the_item(&self) -> ItemStack {
        self.container_ref.unpack_loot_table(None);
        let mut container = self.container.lock();
        container.item[0].grow(1);
        container.item[0].clone()
    }

    /// Vanilla `wobble`: block event 1 with the style's ordinal.
    pub fn wobble(&self, style: WobbleStyle) {
        let Some(world) = self.get_level() else {
            return;
        };
        world.block_event(
            self.base.pos(),
            self.base.block_state().get_block(),
            EVENT_POT_WOBBLES,
            style.ordinal(),
        );
    }

    /// The last wobble this pot received, if any.
    #[must_use]
    pub fn last_wobble(&self) -> Option<Wobble> {
        *self.wobble.lock()
    }

    /// The container behind this pot, for hoppers, comparators and the like.
    #[must_use]
    pub const fn container(&self) -> &ContainerRef {
        &self.container_ref
    }
}

impl BlockEntity for DecoratedPotBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn trigger_event(&self, event: i32, data: i32) -> bool {
        if event != EVENT_POT_WOBBLES {
            return false;
        }
        let (Some(world), Some(style)) = (self.get_level(), WobbleStyle::from_ordinal(data)) else {
            return false;
        };
        *self.wobble.lock() = Some(Wobble {
            started_at_tick: world.game_time(),
            style,
        });
        true
    }

    /// Vanilla `BlockEntity.preRemoveSideEffects`: `Containers.dropContents`.
    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        self.container_ref.unpack_loot_table_for_removal();
        let item = mem::take(&mut self.container.lock().item[0]);
        let Some(world) = self.get_level() else {
            return;
        };
        world.drop_item_stack(pos, item);
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        *self.decorations.lock() = nbt
            .get(SHERDS_TAG)
            .and_then(PotDecorations::from_nbt_tag)
            .unwrap_or(PotDecorations::EMPTY);
        let item = if self.loot.load(nbt) {
            ItemStack::empty()
        } else {
            nbt.compound(ITEM_TAG)
                .and_then(|item| ItemStack::from_borrowed_compound(&item))
                .unwrap_or_else(ItemStack::empty)
        };
        self.container.lock().item[0] = item;
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let decorations = self.decorations.lock().clone();
        if decorations != PotDecorations::EMPTY {
            nbt.insert(SHERDS_TAG, decorations.to_nbt_tag());
        }
        if self.loot.save(nbt) {
            return;
        }
        let container = self.container.lock();
        if !container.item[0].is_empty() {
            nbt.insert(ITEM_TAG, container.item[0].to_nbt_tag_ref());
        }
    }

    /// Vanilla `getUpdateTag`: `saveCustomOnly`.
    fn get_update_tag(&self) -> Option<NbtCompound> {
        Some(self.save_custom_only())
    }

    fn apply_implicit_components(&self, components: &mut ImplicitComponentGetter<'_>) {
        *self.decorations.lock() =
            components.get_or_default(POT_DECORATIONS, PotDecorations::EMPTY);
        let contents = components.get_or_default(CONTAINER, ItemContainerContents::empty());
        self.container.lock().item[0] = contents
            .items()
            .first()
            .and_then(Option::as_ref)
            .map_or_else(ItemStack::empty, ItemStackTemplate::create);
    }

    fn collect_implicit_components(&self, components: &mut DataComponentMap) {
        components.set(POT_DECORATIONS, Some(self.decorations.lock().clone()));
        // Vanilla `ItemContainerContents.fromItems(List.of(item))` drops the
        // trailing empty slot.
        let item = self.container.lock().item[0].clone();
        let slots = if item.is_empty() {
            Vec::new()
        } else {
            vec![ItemStackTemplate::from_stack(&item).ok()]
        };
        components.set(
            CONTAINER,
            Some(ItemContainerContents::new(slots).unwrap_or_default()),
        );
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use simdnbt::borrow::read_compound;
    use steel_registry::items::ItemRef;
    use steel_registry::{init_vanilla_registry, vanilla_blocks};

    use super::*;

    fn test_pot() -> DecoratedPotBlockEntity {
        init_vanilla_registry();
        DecoratedPotBlockEntity::new(
            Weak::new(),
            BlockPos::new(1, 2, 3),
            vanilla_blocks::DECORATED_POT.default_state(),
        )
    }

    fn sherd(item: ItemRef) -> ItemStackTemplate {
        ItemStackTemplate::new(item)
    }

    fn reload(pot: &DecoratedPotBlockEntity) -> DecoratedPotBlockEntity {
        let saved = pot.save_custom_only();
        let reloaded = test_pot();
        let mut bytes = Vec::new();
        saved.write(&mut bytes);
        let borrowed =
            read_compound(&mut Cursor::new(bytes.as_slice())).expect("saved pot NBT reads back");
        reloaded.load_additional(&borrowed);
        reloaded
    }

    #[test]
    fn sherds_and_item_survive_a_save_and_load() {
        let pot = test_pot();
        *pot.decorations.lock() = PotDecorations::new(
            Some(sherd(&vanilla_items::BRICK)),
            Some(sherd(&vanilla_items::ANGLER_POTTERY_SHERD)),
            None,
            Some(sherd(&vanilla_items::SKULL_POTTERY_SHERD)),
        );
        pot.container.lock().item[0] = ItemStack::with_count(&vanilla_items::DIAMOND, 5);

        let reloaded = reload(&pot);

        assert_eq!(reloaded.decorations(), pot.decorations());
        let item = reloaded.container.lock().item[0].clone();
        assert!(item.is(&vanilla_items::DIAMOND));
        assert_eq!(item.count(), 5);
    }

    #[test]
    fn an_empty_undecorated_pot_saves_nothing() {
        let pot = test_pot();
        let saved = pot.save_custom_only();
        assert!(saved.get(SHERDS_TAG).is_none());
        assert!(saved.get(ITEM_TAG).is_none());
    }

    #[test]
    fn implicit_components_carry_the_sherds_and_the_item() {
        let pot = test_pot();
        let decorations = PotDecorations::new(
            Some(sherd(&vanilla_items::ARCHER_POTTERY_SHERD)),
            None,
            None,
            None,
        );
        *pot.decorations.lock() = decorations.clone();
        pot.container.lock().item[0] = ItemStack::with_count(&vanilla_items::EMERALD, 3);

        let mut components = DataComponentMap::new();
        pot.collect_implicit_components(&mut components);

        assert_eq!(components.get(POT_DECORATIONS), Some(decorations));
        let contents = components.get(CONTAINER).expect("a container component");
        assert_eq!(contents.items().len(), 1);
        assert_eq!(
            contents.items()[0].as_ref().map(ItemStackTemplate::count),
            Some(3)
        );
    }
}
