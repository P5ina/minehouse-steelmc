//! Vanilla `ItemCost`, `MerchantOffer` and `MerchantOffers`.

use simdnbt::FromNbtTag as _;
use simdnbt::ToNbtTag as _;
use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
use steel_registry::data_components::{DataComponentMap, DataComponentPatch};
use steel_registry::item_stack::ItemStack;
use steel_registry::items::ItemRef;
use steel_registry::{REGISTRY, RegistryExt as _};
use steel_utils::Identifier;
use steel_utils::nbt::NbtNumeric as _;

const TAG_RECIPES: &str = "Recipes";
/// Vanilla `MerchantOffer` defaults, omitted from NBT when equal.
const DEFAULT_MAX_USES: i32 = 4;
const DEFAULT_XP: i32 = 1;

/// Vanilla `ItemCost`: an item, a count and an exact component predicate.
#[derive(Debug, Clone)]
pub struct ItemCost {
    item: ItemRef,
    count: i32,
    components: DataComponentMap,
}

impl ItemCost {
    /// Creates a new value.
    #[must_use]
    pub const fn new(item: ItemRef, count: i32, components: DataComponentMap) -> Self {
        Self {
            item,
            count,
            components,
        }
    }

    /// The cost item.
    #[must_use]
    pub const fn item(&self) -> ItemRef {
        self.item
    }

    /// The base cost count.
    #[must_use]
    pub const fn count(&self) -> i32 {
        self.count
    }

    /// The components the paid stack must carry.
    #[must_use]
    pub const fn components(&self) -> &DataComponentMap {
        &self.components
    }

    /// Vanilla `ItemCost.itemStack`: the cost as a stack carrying the required components.
    #[must_use]
    pub fn item_stack(&self) -> ItemStack {
        let mut patch = DataComponentPatch::new();
        for (key, data) in self.components.iter() {
            patch.set_raw(key.clone(), data.clone());
        }
        ItemStack::with_count_and_patch(self.item, self.count, patch)
    }

    /// Vanilla `ItemCost.test`: same item and every required component equal.
    #[must_use]
    pub fn test(&self, stack: &ItemStack) -> bool {
        stack.is(self.item)
            && self
                .components
                .iter()
                .all(|(key, data)| stack.get_effective_value_raw(key) == Some(data))
    }

    fn to_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert("id", self.item.key.to_string());
        // Vanilla `optionalAlwaysPresentFieldOf`: the count is always written.
        nbt.insert("count", self.count);
        if !self.components.is_empty() {
            nbt.insert("components", self.components.to_nbt_tag_ref());
        }
        nbt
    }

    fn from_nbt(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let id: Identifier = nbt.string("id")?.to_str().parse().ok()?;
        let item = REGISTRY.items.by_key(&id)?;
        // Vanilla `POSITIVE_INT` with default 1.
        let count = match nbt.get("count") {
            Some(tag) => tag.codec_i32().filter(|count| *count > 0)?,
            None => 1,
        };
        let components = match nbt.get("components") {
            Some(tag) => DataComponentMap::from_nbt_tag(tag)?,
            None => DataComponentMap::new(),
        };
        Some(Self::new(item, count, components))
    }
}

/// Vanilla `MerchantOffer`.
#[derive(Debug, Clone)]
pub struct MerchantOffer {
    base_cost_a: ItemCost,
    cost_b: Option<ItemCost>,
    result: ItemStack,
    uses: i32,
    max_uses: i32,
    reward_exp: bool,
    special_price_diff: i32,
    demand: i32,
    price_multiplier: f32,
    xp: i32,
}

impl MerchantOffer {
    /// Vanilla `MerchantOffer(baseCostA, costB, result, maxUses, xp, priceMultiplier)`.
    #[must_use]
    pub const fn new(
        base_cost_a: ItemCost,
        cost_b: Option<ItemCost>,
        result: ItemStack,
        max_uses: i32,
        xp: i32,
        price_multiplier: f32,
    ) -> Self {
        Self {
            base_cost_a,
            cost_b,
            result,
            uses: 0,
            max_uses,
            reward_exp: true,
            special_price_diff: 0,
            demand: 0,
            price_multiplier,
            xp,
        }
    }

    /// Vanilla `getBaseCostA`.
    #[must_use]
    pub fn base_cost_a(&self) -> ItemStack {
        self.base_cost_a.item_stack()
    }

    /// Vanilla `getCostA`: the first cost adjusted by demand and special price.
    #[must_use]
    pub fn cost_a(&self) -> ItemStack {
        self.base_cost_a
            .item_stack()
            .copy_with_count(self.modified_cost_count(&self.base_cost_a))
    }

    /// Vanilla `getModifiedCostCount`.
    fn modified_cost_count(&self, cost: &ItemCost) -> i32 {
        let base_price = cost.count();
        // Vanilla `Mth.floor(float)`.
        let demand_diff = (base_price.wrapping_mul(self.demand) as f32 * self.price_multiplier)
            .floor()
            .max(0.0) as i32;
        (base_price + demand_diff + self.special_price_diff)
            .clamp(1, cost.item_stack().max_stack_size())
    }

    /// Vanilla `getCostB`.
    #[must_use]
    pub fn cost_b(&self) -> ItemStack {
        self.cost_b
            .as_ref()
            .map_or_else(ItemStack::empty, ItemCost::item_stack)
    }

    /// Vanilla `getItemCostA`.
    #[must_use]
    pub const fn item_cost_a(&self) -> &ItemCost {
        &self.base_cost_a
    }

    /// Vanilla `getItemCostB`.
    #[must_use]
    pub const fn item_cost_b(&self) -> Option<&ItemCost> {
        self.cost_b.as_ref()
    }

    /// Vanilla `getResult`.
    #[must_use]
    pub const fn result(&self) -> &ItemStack {
        &self.result
    }

    /// Vanilla `updateDemand`.
    pub const fn update_demand(&mut self) {
        self.demand = self.demand + self.uses - (self.max_uses - self.uses);
    }

    /// Vanilla `assemble`.
    #[must_use]
    pub fn assemble(&self) -> ItemStack {
        self.result.clone()
    }

    /// Vanilla `getUses`.
    #[must_use]
    pub const fn uses(&self) -> i32 {
        self.uses
    }

    /// Vanilla `resetUses`.
    pub const fn reset_uses(&mut self) {
        self.uses = 0;
    }

    /// Vanilla `getMaxUses`.
    #[must_use]
    pub const fn max_uses(&self) -> i32 {
        self.max_uses
    }

    /// Vanilla `increaseUses`.
    pub const fn increase_uses(&mut self) {
        self.uses += 1;
    }

    /// Vanilla `getDemand`.
    #[must_use]
    pub const fn demand(&self) -> i32 {
        self.demand
    }

    /// Vanilla `addToSpecialPriceDiff`.
    pub const fn add_to_special_price_diff(&mut self, add: i32) {
        self.special_price_diff += add;
    }

    /// Vanilla `resetSpecialPriceDiff`.
    pub const fn reset_special_price_diff(&mut self) {
        self.special_price_diff = 0;
    }

    /// Vanilla `getSpecialPriceDiff`.
    #[must_use]
    pub const fn special_price_diff(&self) -> i32 {
        self.special_price_diff
    }

    /// Vanilla `setSpecialPriceDiff`.
    pub const fn set_special_price_diff(&mut self, value: i32) {
        self.special_price_diff = value;
    }

    /// Vanilla `getPriceMultiplier`.
    #[must_use]
    pub const fn price_multiplier(&self) -> f32 {
        self.price_multiplier
    }

    /// Vanilla `getXp`.
    #[must_use]
    pub const fn xp(&self) -> i32 {
        self.xp
    }

    /// Vanilla `isOutOfStock`.
    #[must_use]
    pub const fn is_out_of_stock(&self) -> bool {
        self.uses >= self.max_uses
    }

    /// Vanilla `setToOutOfStock`.
    pub const fn set_to_out_of_stock(&mut self) {
        self.uses = self.max_uses;
    }

    /// Vanilla `needsRestock`.
    #[must_use]
    pub const fn needs_restock(&self) -> bool {
        self.uses > 0
    }

    /// Vanilla `shouldRewardExp`.
    #[must_use]
    pub const fn should_reward_exp(&self) -> bool {
        self.reward_exp
    }

    /// Vanilla `satisfiedBy`.
    #[must_use]
    pub fn satisfied_by(&self, buy_a: &ItemStack, buy_b: &ItemStack) -> bool {
        if !self.base_cost_a.test(buy_a)
            || buy_a.count() < self.modified_cost_count(&self.base_cost_a)
        {
            return false;
        }
        match &self.cost_b {
            None => buy_b.is_empty(),
            Some(cost_b) => cost_b.test(buy_b) && buy_b.count() >= cost_b.count(),
        }
    }

    /// Vanilla `take`: consumes the costs from the payment stacks.
    pub fn take(&self, buy_a: &mut ItemStack, buy_b: &mut ItemStack) -> bool {
        if !self.satisfied_by(buy_a, buy_b) {
            return false;
        }
        buy_a.shrink(self.cost_a().count());
        let cost_b = self.cost_b();
        if !cost_b.is_empty() {
            buy_b.shrink(cost_b.count());
        }
        true
    }

    /// Vanilla `MerchantOffer.CODEC` encode; fields equal to their default are omitted.
    #[must_use]
    pub fn to_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        nbt.insert("buy", NbtTag::Compound(self.base_cost_a.to_nbt()));
        if let Some(cost_b) = &self.cost_b {
            nbt.insert("buyB", NbtTag::Compound(cost_b.to_nbt()));
        }
        nbt.insert("sell", self.result.clone().to_nbt_tag());
        if self.uses != 0 {
            nbt.insert("uses", self.uses);
        }
        if self.max_uses != DEFAULT_MAX_USES {
            nbt.insert("maxUses", self.max_uses);
        }
        if !self.reward_exp {
            nbt.insert("rewardExp", 0_i8);
        }
        if self.special_price_diff != 0 {
            nbt.insert("specialPrice", self.special_price_diff);
        }
        if self.demand != 0 {
            nbt.insert("demand", self.demand);
        }
        if self.price_multiplier != 0.0 {
            nbt.insert("priceMultiplier", self.price_multiplier);
        }
        if self.xp != DEFAULT_XP {
            nbt.insert("xp", self.xp);
        }
        nbt
    }

    /// Vanilla `MerchantOffer.CODEC` decode. `buy` and `sell` are required; the
    /// other fields are lenient and fall back to their default when malformed.
    #[must_use]
    pub fn from_nbt(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let base_cost_a = ItemCost::from_nbt(nbt.compound("buy")?)?;
        let cost_b = nbt.compound("buyB").and_then(ItemCost::from_nbt);
        let result = ItemStack::from_borrowed_compound(&nbt.compound("sell")?)?;
        if result.is_empty() {
            return None;
        }
        let int = |key: &str, default: i32| {
            nbt.get(key)
                .and_then(|tag| tag.codec_i32())
                .unwrap_or(default)
        };
        Some(Self {
            base_cost_a,
            cost_b,
            result,
            uses: int("uses", 0),
            max_uses: int("maxUses", DEFAULT_MAX_USES),
            reward_exp: nbt
                .get("rewardExp")
                .and_then(|tag| tag.codec_bool())
                .unwrap_or(true),
            special_price_diff: int("specialPrice", 0),
            demand: int("demand", 0),
            price_multiplier: nbt
                .get("priceMultiplier")
                .and_then(|tag| tag.codec_f32())
                .unwrap_or(0.0),
            xp: int("xp", DEFAULT_XP),
        })
    }
}

/// Vanilla `MerchantOffers`.
#[derive(Debug, Clone, Default)]
pub struct MerchantOffers {
    offers: Vec<MerchantOffer>,
}

impl MerchantOffers {
    /// Creates an empty offer list.
    #[must_use]
    pub const fn new() -> Self {
        Self { offers: Vec::new() }
    }

    /// Appends an offer.
    pub fn push(&mut self, offer: MerchantOffer) {
        self.offers.push(offer);
    }

    /// Appends every offer of `other`, in order.
    pub fn append(&mut self, mut other: Self) {
        self.offers.append(&mut other.offers);
    }

    /// Number of offers.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.offers.len()
    }

    /// Whether there are no offers.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.offers.is_empty()
    }

    /// The offer at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&MerchantOffer> {
        self.offers.get(index)
    }

    /// The offer at `index`, mutably.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut MerchantOffer> {
        self.offers.get_mut(index)
    }

    /// Iterates over the offers.
    pub fn iter(&self) -> impl Iterator<Item = &MerchantOffer> {
        self.offers.iter()
    }

    /// Iterates mutably over the offers.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut MerchantOffer> {
        self.offers.iter_mut()
    }

    /// Vanilla `getRecipeFor`.
    #[must_use]
    pub fn recipe_for(
        &self,
        buy_a: &ItemStack,
        buy_b: &ItemStack,
        selection_hint: i32,
    ) -> Option<&MerchantOffer> {
        self.recipe_index_for(buy_a, buy_b, selection_hint)
            .and_then(|index| self.offers.get(index))
    }

    /// Vanilla `getRecipeFor`, as the index of the offer.
    #[must_use]
    pub fn recipe_index_for(
        &self,
        buy_a: &ItemStack,
        buy_b: &ItemStack,
        selection_hint: i32,
    ) -> Option<usize> {
        if selection_hint > 0
            && let Ok(hint) = usize::try_from(selection_hint)
            && let Some(offer) = self.offers.get(hint)
        {
            return offer.satisfied_by(buy_a, buy_b).then_some(hint);
        }
        self.offers
            .iter()
            .position(|offer| offer.satisfied_by(buy_a, buy_b))
    }

    /// Vanilla `MerchantOffers.CODEC` encode: `{Recipes: [...]}`, empty list omitted.
    #[must_use]
    pub fn to_nbt(&self) -> NbtCompound {
        let mut nbt = NbtCompound::new();
        if !self.offers.is_empty() {
            let recipes = self.offers.iter().map(MerchantOffer::to_nbt).collect();
            nbt.insert(TAG_RECIPES, NbtList::Compound(recipes));
        }
        nbt
    }

    /// Vanilla `MerchantOffers.CODEC` decode: one malformed offer fails the whole list.
    #[must_use]
    pub fn from_nbt(nbt: BorrowedNbtCompoundView<'_, '_>) -> Option<Self> {
        let Some(list) = nbt.list(TAG_RECIPES) else {
            return Some(Self::new());
        };
        if list.empty() {
            return Some(Self::new());
        }
        let offers = list
            .compounds()?
            .into_iter()
            .map(MerchantOffer::from_nbt)
            .collect::<Option<Vec<_>>>()?;
        Some(Self { offers })
    }
}
