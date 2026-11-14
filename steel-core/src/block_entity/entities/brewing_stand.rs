//! Brewing stand block entity.

use std::array::from_fn;
use std::mem;
use std::sync::{Arc, Weak};

use simdnbt::ToNbtTag as _;
use simdnbt::borrow::{BaseNbtCompound as BorrowedNbtCompound, NbtCompound as NbtCompoundView};
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::blocks::block_state_ext::BlockStateExt as _;
use steel_registry::blocks::properties::{BlockStateProperties, BoolProperty, Direction};
use steel_registry::context_float_provider::ContextFloatProvider;
use steel_registry::data_components::vanilla_components::BREWING_FUEL;
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::recipe::{
    BrewingInput, BrewingRecipe, CachedRecipeCheck, vanilla_recipe_types,
};
use steel_registry::{
    REGISTRY, level_events, vanilla_block_entity_types, vanilla_blocks,
    vanilla_context_float_providers, vanilla_context_int_providers, vanilla_items,
};
use steel_utils::locks::IntoShared;
use steel_utils::{
    BlockPos, BlockStateId, DowncastType, DowncastTypeKey, locks::SyncMutex, types::UpdateFlags,
};

use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::inventory::container::Container;
use crate::inventory::lock::{ContainerRef, SharedContainer};
use crate::inventory::recipe_access::{BREWING_REAGENTS, RECIPE_ACCESS, is_potion_input};
use crate::world::World;

/// Vanilla `BrewingStandBlockEntity`'s container size.
pub const BREWING_STAND_SLOTS: usize = 5;
/// Vanilla `BrewingStandBlockEntity.INGREDIENT_SLOT`.
pub const INGREDIENT_SLOT: usize = 3;
/// Vanilla `BrewingStandBlockEntity.FUEL_SLOT`.
pub const FUEL_SLOT: usize = 4;
/// The three bottle slots.
pub const BOTTLE_SLOTS: [usize; 3] = [0, 1, 2];

const SLOTS_FOR_UP: &[usize] = &[INGREDIENT_SLOT];
const SLOTS_FOR_DOWN: &[usize] = &[0, 1, 2, INGREDIENT_SLOT];
const SLOTS_FOR_SIDES: &[usize] = &[0, 1, 2, FUEL_SLOT];
/// Vanilla `BrewingStandBlockEntity.BREWING_TIME_SECONDS` in ticks.
const BREWING_TIME: f32 = 400.0;
const DEFAULT_BREW_TIME: i32 = 0;
const DEFAULT_TOTAL_BREW_TIME: i32 = 400;
const DEFAULT_FUEL: i32 = 0;
const DEFAULT_FUEL_USES: i32 = 20;
const DEFAULT_SPEED_MULTIPLIER: f32 = 1.0;

/// Vanilla `BrewingStandBlock.HAS_BOTTLE`.
pub const HAS_BOTTLE: [&BoolProperty; 3] = [
    &BlockStateProperties::HAS_BOTTLE_0,
    &BlockStateProperties::HAS_BOTTLE_1,
    &BlockStateProperties::HAS_BOTTLE_2,
];

/// Independently lockable brewing stand inventory and progress data.
pub struct BrewingStandContainer {
    items: [ItemStack; BREWING_STAND_SLOTS],
    brew_time: i32,
    total_brew_time: i32,
    /// Vanilla `lastPotionCount`, unset until the first tick.
    last_potion_count: Option<[bool; 3]>,
    /// The reagent item the current brew started with.
    ingredient: Option<ItemRef>,
    fuel: i32,
    total_fuel: i32,
    speed_multiplier: f32,
    quick_check: CachedRecipeCheck<BrewingRecipe, BrewingInput>,
}

// SAFETY: This Steel-owned key uniquely identifies brewing stand inventory/progress storage.
unsafe impl DowncastType for BrewingStandContainer {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:container/brewing_stand");
}

/// What one tick asks the block entity to do in the world.
#[derive(Default)]
struct BrewingTickResult {
    changed: bool,
    brewed: bool,
    /// Crafting remainders that did not fit back into their slot.
    drops: Vec<ItemStack>,
    /// New `HAS_BOTTLE` values when bottle presence changed.
    potion_bits: Option<[bool; 3]>,
}

impl BrewingStandContainer {
    fn new() -> Self {
        Self {
            items: from_fn(|_| ItemStack::empty()),
            brew_time: DEFAULT_BREW_TIME,
            total_brew_time: DEFAULT_TOTAL_BREW_TIME,
            last_potion_count: None,
            ingredient: None,
            fuel: DEFAULT_FUEL,
            total_fuel: DEFAULT_FUEL_USES,
            speed_multiplier: DEFAULT_SPEED_MULTIPLIER,
            quick_check: CachedRecipeCheck::new(&vanilla_recipe_types::BREWING),
        }
    }

    /// Vanilla `dataAccess`: brew time, fuel, total brew time, total fuel.
    #[must_use]
    pub(crate) const fn data(&self) -> [i16; 4] {
        [
            self.brew_time as i16,
            self.fuel as i16,
            self.total_brew_time as i16,
            self.total_fuel as i16,
        ]
    }

    fn has_recipe(&mut self, bottle: &ItemStack, ingredient: &ItemStack) -> bool {
        let input = BrewingInput::new(bottle.clone(), ingredient.clone());
        self.quick_check
            .find_match(&REGISTRY.recipes, &input)
            .is_some()
    }

    /// Vanilla `isBrewable`.
    fn is_brewable(&mut self) -> bool {
        let ingredient = self.items[INGREDIENT_SLOT].clone();
        if ingredient.is_empty()
            || !RECIPE_ACCESS
                .property_set(&BREWING_REAGENTS)
                .test(&ingredient)
        {
            return false;
        }
        for slot in BOTTLE_SLOTS {
            let bottle = self.items[slot].clone();
            if !bottle.is_empty() && self.has_recipe(&bottle, &ingredient) {
                return true;
            }
        }
        false
    }

    /// Vanilla `doBrew`, returning a remainder that must be dropped.
    fn do_brew(&mut self) -> Option<ItemStack> {
        let ingredient = self.items[INGREDIENT_SLOT].clone();
        for slot in BOTTLE_SLOTS {
            let input = BrewingInput::new(self.items[slot].clone(), ingredient.clone());
            if let Some(recipe) = self.quick_check.find_match(&REGISTRY.recipes, &input) {
                self.items[slot] = recipe.data().assemble();
            }
        }

        let remainder = ingredient.item().get_crafting_remainder();
        let ingredient = &mut self.items[INGREDIENT_SLOT];
        ingredient.shrink(1);
        if remainder.is_empty() {
            return None;
        }
        if ingredient.is_empty() {
            *ingredient = remainder;
            None
        } else {
            Some(remainder)
        }
    }

    /// Vanilla `getPotionBits`.
    fn potion_bits(&self) -> [bool; 3] {
        BOTTLE_SLOTS.map(|slot| !self.items[slot].is_empty())
    }

    /// Vanilla `serverTick` without the world side effects.
    fn tick(&mut self, state: BlockStateId) -> BrewingTickResult {
        let mut result = BrewingTickResult::default();

        if self.fuel <= 0
            && let Some(brewing_fuel) = self.items[FUEL_SLOT].get(BREWING_FUEL).cloned()
        {
            // Vanilla `getUses` / `getSpeedMultiplier` fall back to 0 and 1.
            self.fuel = vanilla_context_int_providers::get(&brewing_fuel.uses)
                .map_or(0, |provider| {
                    provider.get_int(Some(state), &mut rand::rng())
                });
            self.total_fuel = self.fuel;
            self.speed_multiplier =
                vanilla_context_float_providers::get(&brewing_fuel.speed_multiplier)
                    .map_or(1.0, ContextFloatProvider::get_float);
            let remainder = self.items[FUEL_SLOT].item().get_crafting_remainder();
            let fuel = &mut self.items[FUEL_SLOT];
            fuel.shrink(1);
            if !remainder.is_empty() {
                if fuel.is_empty() {
                    *fuel = remainder;
                } else {
                    result.drops.push(remainder);
                }
            }
            result.changed = true;
        }

        let brewable = self.is_brewable();
        let is_brewing = self.brew_time > 0;
        if is_brewing {
            self.brew_time -= 1;
            let is_done_brewing = self.brew_time == 0;
            if is_done_brewing && brewable {
                result.drops.extend(self.do_brew());
                result.brewed = true;
            } else if !brewable
                || self
                    .ingredient
                    .is_none_or(|ingredient| !self.items[INGREDIENT_SLOT].is(ingredient))
            {
                self.brew_time = 0;
            }
            result.changed = true;
        } else if brewable && self.fuel > 0 {
            let speed_multiplier = if self.speed_multiplier > 0.0 {
                self.speed_multiplier
            } else {
                1.0
            };
            self.fuel -= 1;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Vanilla casts the ceiled float brewing time to int"
            )]
            let brew_time = (BREWING_TIME / speed_multiplier).ceil() as i32;
            self.brew_time = brew_time;
            self.total_brew_time = brew_time;
            self.ingredient = Some(self.items[INGREDIENT_SLOT].item());
            result.changed = true;
        }

        let bits = self.potion_bits();
        if self.last_potion_count != Some(bits) {
            self.last_potion_count = Some(bits);
            result.potion_bits = Some(bits);
        }
        result
    }
}

impl Container for BrewingStandContainer {
    fn items(&self) -> &[ItemStack] {
        &self.items
    }

    fn items_mut(&mut self) -> &mut [ItemStack] {
        &mut self.items
    }

    fn set_changed(&mut self) {}

    /// Vanilla `BrewingStandBlockEntity.canPlaceItem`.
    fn can_place_item(&self, slot: usize, stack: &ItemStack) -> bool {
        match slot {
            FUEL_SLOT => stack.has(BREWING_FUEL),
            INGREDIENT_SLOT => RECIPE_ACCESS.property_set(&BREWING_REAGENTS).test(stack),
            _ => is_potion_input(stack) && self.get_item(slot).is_empty(),
        }
    }

    fn slots_for_face(&self, direction: Direction) -> Option<&'static [usize]> {
        Some(match direction {
            Direction::Up => SLOTS_FOR_UP,
            Direction::Down => SLOTS_FOR_DOWN,
            _ => SLOTS_FOR_SIDES,
        })
    }

    fn can_take_item_through_face(
        &self,
        slot: usize,
        stack: &ItemStack,
        _direction: Direction,
    ) -> bool {
        slot != INGREDIENT_SLOT || stack.is(&vanilla_items::GLASS_BOTTLE)
    }
}

/// Vanilla `BrewingStandBlockEntity`.
pub struct BrewingStandBlockEntity {
    base: Arc<BlockEntityBase>,
    container: Arc<SyncMutex<BrewingStandContainer>>,
    container_ref: ContainerRef,
}

// SAFETY: This Steel-owned key uniquely identifies this concrete block entity.
unsafe impl DowncastType for BrewingStandBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/brewing_stand");
}

impl BrewingStandBlockEntity {
    /// Creates the block entity at a live world position.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        let base = Arc::new(BlockEntityBase::new(
            &vanilla_block_entity_types::BREWING_STAND,
            level,
            pos,
            state,
        ));
        let container = BrewingStandContainer::new().into_shared();
        let shared: SharedContainer = Arc::<SyncMutex<BrewingStandContainer>>::clone(&container);
        Self {
            container_ref: ContainerRef::owned_by_block_entity(shared, Arc::clone(&base)),
            base,
            container,
        }
    }

    /// Vanilla `BrewingStandBlockEntity.serverTick`.
    fn server_tick(&self, world: &Arc<World>) {
        let pos = self.base.pos();
        let self_state = self.base.block_state();
        let result = self.container.lock().tick(self_state);

        for drop in result.drops {
            world.drop_item_stack(pos, drop);
        }
        if result.brewed {
            world.level_event(level_events::SOUND_BREWING_STAND_BREW, pos, 0, None);
        }
        if result.changed {
            self.base.set_changed();
        }
        if let Some(bits) = result.potion_bits
            && self_state.get_block() == &vanilla_blocks::BREWING_STAND
        {
            let mut state = self_state;
            for (property, has_bottle) in HAS_BOTTLE.into_iter().zip(bits) {
                state = state.set_value(property, has_bottle);
            }
            world.set_block(pos, state, UpdateFlags::UPDATE_CLIENTS);
        }
    }
}

impl BlockEntity for BrewingStandBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn pre_remove_side_effects(&self, pos: BlockPos, _state: BlockStateId) {
        let Some(world) = self.get_level() else {
            return;
        };
        let items = mem::replace(
            &mut self.container.lock().items,
            from_fn(|_| ItemStack::empty()),
        );
        for item in items {
            world.drop_item_stack(pos, item);
        }
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        let nbt: NbtCompoundView<'_, '_> = nbt.into();
        let mut stand = self.container.lock();
        stand.items.fill(ItemStack::empty());
        if let Some(items) = nbt.list("Items").and_then(|list| list.compounds()) {
            for compound in items {
                let Some(slot) = compound.byte("Slot").map(|slot| slot as usize) else {
                    continue;
                };
                if slot < BREWING_STAND_SLOTS
                    && let Some(stack) = ItemStack::from_borrowed_compound(&compound)
                {
                    stand.items[slot] = stack;
                }
            }
        }
        stand.brew_time = nbt.int("BrewTime").unwrap_or(DEFAULT_BREW_TIME);
        stand.total_brew_time = nbt
            .int("total_brew_time")
            .unwrap_or(DEFAULT_TOTAL_BREW_TIME);
        if stand.brew_time > 0 {
            stand.ingredient = Some(stand.items[INGREDIENT_SLOT].item());
        }
        stand.fuel = nbt.int("Fuel").unwrap_or(DEFAULT_FUEL);
        stand.total_fuel = nbt.int("total_fuel").unwrap_or(DEFAULT_FUEL_USES);
        stand.speed_multiplier = nbt
            .float("speed_multiplier")
            .unwrap_or(DEFAULT_SPEED_MULTIPLIER);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        let stand = self.container.lock();
        nbt.insert("BrewTime", stand.brew_time);
        nbt.insert("total_brew_time", stand.total_brew_time);

        let mut items = Vec::new();
        for (slot, stack) in stand.items.iter().enumerate() {
            if !stack.is_empty()
                && let NbtTag::Compound(mut item) = stack.clone().to_nbt_tag()
            {
                item.insert("Slot", slot as i8);
                items.push(item);
            }
        }
        nbt.insert("Items", NbtList::Compound(items));
        nbt.insert("Fuel", stand.fuel);
        nbt.insert("total_fuel", stand.total_fuel);
        nbt.insert("speed_multiplier", stand.speed_multiplier);
    }

    fn tick(&self, world: &Arc<World>) {
        self.server_tick(world);
    }

    fn container_ref(&self) -> Option<ContainerRef> {
        Some(self.container_ref.clone())
    }
}

#[cfg(test)]
mod tests {
    use steel_registry::data_components::vanilla_components::{POTION_CONTENTS, PotionContents};
    use steel_registry::potion::PotionRef;
    use steel_registry::{init_vanilla_registry, vanilla_potions};

    use super::*;

    fn potion(item: ItemRef, potion: PotionRef) -> ItemStack {
        PotionContents::create_item_stack(item, potion)
    }

    fn state() -> BlockStateId {
        vanilla_blocks::BREWING_STAND.default_state()
    }

    #[test]
    fn brews_awkward_potions_from_water_and_nether_wart() {
        init_vanilla_registry();
        let mut stand = BrewingStandContainer::new();
        stand.items[0] = potion(&vanilla_items::POTION, &vanilla_potions::WATER);
        stand.items[2] = potion(&vanilla_items::POTION, &vanilla_potions::WATER);
        stand.items[INGREDIENT_SLOT] = ItemStack::new(&vanilla_items::NETHER_WART);
        stand.items[FUEL_SLOT] = ItemStack::new(&vanilla_items::BLAZE_POWDER);

        let first = stand.tick(state());
        assert_eq!(first.potion_bits, Some([true, false, true]));
        assert!(stand.items[FUEL_SLOT].is_empty());
        assert_eq!(stand.total_fuel, 20);
        assert_eq!(stand.fuel, 19);
        assert_eq!(stand.brew_time, 400);
        assert_eq!(stand.total_brew_time, 400);

        for _ in 0..399 {
            assert!(!stand.tick(state()).brewed);
        }
        let last = stand.tick(state());
        assert!(last.brewed);
        assert!(stand.items[INGREDIENT_SLOT].is_empty());
        for slot in [0, 2] {
            assert!(
                stand.items[slot]
                    .get(POTION_CONTENTS)
                    .is_some_and(|contents| contents.is(&vanilla_potions::AWKWARD))
            );
        }
        assert!(stand.items[1].is_empty());
    }

    #[test]
    fn changing_the_reagent_mid_brew_stops_brewing() {
        init_vanilla_registry();
        let mut stand = BrewingStandContainer::new();
        stand.items[0] = potion(&vanilla_items::POTION, &vanilla_potions::AWKWARD);
        stand.items[INGREDIENT_SLOT] = ItemStack::new(&vanilla_items::SUGAR);
        stand.items[FUEL_SLOT] = ItemStack::new(&vanilla_items::BLAZE_POWDER);
        stand.tick(state());
        assert_eq!(stand.brew_time, 400);

        stand.items[INGREDIENT_SLOT] = ItemStack::new(&vanilla_items::RABBIT_FOOT);
        stand.tick(state());
        assert_eq!(stand.brew_time, 0);
    }

    #[test]
    fn nothing_brews_without_a_matching_bottle() {
        init_vanilla_registry();
        let mut stand = BrewingStandContainer::new();
        stand.items[0] = potion(&vanilla_items::POTION, &vanilla_potions::AWKWARD);
        stand.items[INGREDIENT_SLOT] = ItemStack::new(&vanilla_items::NETHER_WART);
        stand.items[FUEL_SLOT] = ItemStack::new(&vanilla_items::BLAZE_POWDER);
        stand.tick(state());
        // The blaze powder is still burned into fuel uses, as in Vanilla.
        assert_eq!(stand.fuel, 20);
        assert_eq!(stand.brew_time, 0);
    }

    #[test]
    fn slot_rules_match_vanilla() {
        init_vanilla_registry();
        let mut stand = BrewingStandContainer::new();
        let blaze = ItemStack::new(&vanilla_items::BLAZE_POWDER);
        let wart = ItemStack::new(&vanilla_items::NETHER_WART);
        let bottle = ItemStack::new(&vanilla_items::GLASS_BOTTLE);
        let water = potion(&vanilla_items::POTION, &vanilla_potions::WATER);

        assert!(stand.can_place_item(FUEL_SLOT, &blaze));
        assert!(!stand.can_place_item(FUEL_SLOT, &wart));
        assert!(stand.can_place_item(INGREDIENT_SLOT, &wart));
        assert!(!stand.can_place_item(INGREDIENT_SLOT, &water));
        assert!(stand.can_place_item(0, &water));
        assert!(stand.can_place_item(0, &bottle));
        assert!(!stand.can_place_item(0, &wart));
        stand.items[0] = water.clone();
        assert!(!stand.can_place_item(0, &water));

        assert_eq!(stand.slots_for_face(Direction::Up), Some(SLOTS_FOR_UP));
        assert_eq!(stand.slots_for_face(Direction::Down), Some(SLOTS_FOR_DOWN));
        assert_eq!(stand.slots_for_face(Direction::East), Some(SLOTS_FOR_SIDES));
        assert!(!stand.can_take_item_through_face(INGREDIENT_SLOT, &wart, Direction::Down));
        assert!(stand.can_take_item_through_face(INGREDIENT_SLOT, &bottle, Direction::Down));
        assert!(stand.can_take_item_through_face(0, &water, Direction::Down));
    }
}
