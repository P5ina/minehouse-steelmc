use super::{
    ConditionalLootFunction, Identifier, ItemStack, LootCondition, LootContext, LootType,
    NumberProvider, REGISTRY, RegistryExt, RngExt, TaggedRegistryExt,
};
use crate::items::ItemRef;

/// A loot table entry that can generate items.
#[derive(Debug, Clone)]
pub enum LootEntry {
    /// Drop a specific item.
    Item {
        name: Identifier,
        weight: i32,
        quality: i32,
        conditions: &'static [LootCondition],
        functions: &'static [ConditionalLootFunction],
    },
    /// Reference another loot table by name.
    LootTableRef {
        name: Identifier,
        weight: i32,
        quality: i32,
        conditions: &'static [LootCondition],
        functions: &'static [ConditionalLootFunction],
    },
    /// Inline loot table (embedded pools directly in entry).
    InlineLootTable {
        pools: &'static [LootPool],
        weight: i32,
        quality: i32,
        conditions: &'static [LootCondition],
        functions: &'static [ConditionalLootFunction],
    },
    /// Drop items from a tag.
    Tag {
        name: Identifier,
        expand: bool,
        weight: i32,
        quality: i32,
        conditions: &'static [LootCondition],
        functions: &'static [ConditionalLootFunction],
    },
    /// Try children in order, use first that matches.
    Alternatives {
        children: &'static [LootEntry],
        conditions: &'static [LootCondition],
    },
    /// Use all children.
    Group {
        children: &'static [LootEntry],
        conditions: &'static [LootCondition],
    },
    /// Use children in sequence until one fails.
    Sequence {
        children: &'static [LootEntry],
        conditions: &'static [LootCondition],
    },
    /// Empty entry (no drop).
    Empty {
        weight: i32,
        conditions: &'static [LootCondition],
    },
    /// Dynamic content (e.g., block entity contents).
    Dynamic {
        name: Identifier,
        conditions: &'static [LootCondition],
    },
    /// Select items from specific block entity slots.
    Slots {
        /// Slots to select from (can be single slot or range).
        slots: SlotRange,
        conditions: &'static [LootCondition],
        functions: &'static [ConditionalLootFunction],
    },
}

/// A range of slots for the Slots entry type.
#[derive(Debug, Clone, Copy)]
pub enum SlotRange {
    /// A single specific slot index.
    Single(i32),
    /// A range of slots (inclusive).
    Range { min: i32, max: i32 },
    /// All contents slots.
    Contents,
    /// Specific named slots (for entities).
    Named(&'static [&'static str]),
}

impl LootEntry {
    /// Get the weight of this entry for random selection.
    #[must_use]
    pub const fn weight(&self) -> i32 {
        match self {
            Self::Item { weight, .. } => *weight,
            Self::LootTableRef { weight, .. } => *weight,
            Self::InlineLootTable { weight, .. } => *weight,
            Self::Tag { weight, .. } => *weight,
            Self::Empty { weight, .. } => *weight,
            // Composite entries don't have weight
            Self::Alternatives { .. }
            | Self::Group { .. }
            | Self::Sequence { .. }
            | Self::Dynamic { .. }
            | Self::Slots { .. } => 1,
        }
    }

    /// Get the quality modifier for luck-based weight adjustment.
    #[must_use]
    pub const fn quality(&self) -> i32 {
        match self {
            Self::Item { quality, .. } => *quality,
            Self::LootTableRef { quality, .. } => *quality,
            Self::InlineLootTable { quality, .. } => *quality,
            Self::Tag { quality, .. } => *quality,
            Self::Empty { .. }
            | Self::Alternatives { .. }
            | Self::Group { .. }
            | Self::Sequence { .. }
            | Self::Dynamic { .. }
            | Self::Slots { .. } => 0,
        }
    }

    /// Get the effective weight adjusted for luck.
    /// Formula: max(floor(weight + quality * luck), 0)
    #[must_use]
    pub fn effective_weight(&self, luck: f32) -> i32 {
        let base = self.weight() as f32;
        let quality = self.quality() as f32;
        (base + quality * luck).floor().max(0.0) as i32
    }

    /// Get the conditions for this entry.
    #[must_use]
    pub const fn conditions(&self) -> &'static [LootCondition] {
        match self {
            Self::Item { conditions, .. } => conditions,
            Self::LootTableRef { conditions, .. } => conditions,
            Self::InlineLootTable { conditions, .. } => conditions,
            Self::Tag { conditions, .. } => conditions,
            Self::Alternatives { conditions, .. } => conditions,
            Self::Group { conditions, .. } => conditions,
            Self::Sequence { conditions, .. } => conditions,
            Self::Empty { conditions, .. } => conditions,
            Self::Dynamic { conditions, .. } => conditions,
            Self::Slots { conditions, .. } => conditions,
        }
    }

    /// Get the functions for this entry.
    #[must_use]
    pub const fn functions(&self) -> &'static [ConditionalLootFunction] {
        match self {
            Self::Item { functions, .. } => functions,
            Self::LootTableRef { functions, .. } => functions,
            Self::InlineLootTable { functions, .. } => functions,
            Self::Tag { functions, .. } => functions,
            Self::Slots { functions, .. } => functions,
            Self::Empty { .. }
            | Self::Alternatives { .. }
            | Self::Group { .. }
            | Self::Sequence { .. }
            | Self::Dynamic { .. } => &[],
        }
    }
}

/// A pool of loot entries with roll counts.
#[derive(Debug, Clone)]
pub struct LootPool {
    pub rolls: NumberProvider,
    pub bonus_rolls: f32,
    pub entries: &'static [LootEntry],
    pub conditions: &'static [LootCondition],
    pub functions: &'static [ConditionalLootFunction],
}

/// A complete loot table definition.
#[derive(Debug)]
pub struct LootTable {
    pub key: Identifier,
    pub loot_type: LootType,
    pub pools: &'static [LootPool],
    pub functions: &'static [ConditionalLootFunction],
    pub random_sequence: Option<Identifier>,
}

impl LootTable {
    /// Generate random items from this loot table.
    // TODO: Add a world-aware entry point that selects the vanilla RNG before evaluation:
    // nonzero loot seed -> LegacyRandom, table random_sequence -> RandomSequences (including
    // world seed 0), otherwise the level random source.
    ///
    /// # Arguments
    /// * `ctx` - The loot context containing RNG, luck, block state, tool, etc.
    ///
    /// This follows vanilla's approach:
    /// 1. For each pool, check conditions
    /// 2. Roll `rolls + floor(bonus_rolls * luck)` times
    /// 3. Each roll does weighted random selection among valid entries
    /// 4. Apply entry-level functions to each item
    /// 5. Apply pool-level functions to all items from that pool
    /// 6. Apply table-level functions to all items from the table
    pub fn get_random_items<R: rand::Rng>(&self, ctx: &mut LootContext<'_, R>) -> Vec<ItemStack> {
        let mut result = Vec::new();
        for pool in self.pools {
            pool.add_random_items(ctx, &mut result);
        }

        // Apply table-level functions to all items
        if !self.functions.is_empty() {
            for item in &mut result {
                for cond_func in self.functions {
                    if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                        cond_func.function.apply(item, ctx);
                    }
                }
            }
            // Remove items with zero count after applying functions
            result.retain(|item| item.count > 0);
        }

        result
    }
}

impl LootPool {
    /// Add random items from this pool to the result.
    fn add_random_items<R: rand::Rng>(
        &self,
        ctx: &mut LootContext<'_, R>,
        result: &mut Vec<ItemStack>,
    ) {
        // Check pool conditions
        for condition in self.conditions {
            if !condition.test(ctx) {
                return;
            }
        }

        // Track where items from this pool start
        let start_index = result.len();

        // Calculate number of rolls
        let roll_count = self.rolls.get_int(ctx.rng) + (self.bonus_rolls * ctx.luck).floor() as i32;

        for _ in 0..roll_count {
            self.add_random_item(ctx, result);
        }

        // Apply pool-level functions to all items generated by this pool
        if !self.functions.is_empty() {
            for item in result.iter_mut().skip(start_index) {
                for cond_func in self.functions {
                    if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                        cond_func.function.apply(item, ctx);
                    }
                }
            }
            // Remove items with zero count after applying functions
            result.retain(|item| item.count > 0);
        }
    }

    /// Vanilla `LootPool.addRandomItem`: expands every entry into its
    /// singleton candidates, then creates one weighted pick.
    fn add_random_item<R: rand::Rng>(
        &self,
        ctx: &mut LootContext<'_, R>,
        result: &mut Vec<ItemStack>,
    ) {
        let mut candidates: Vec<LootCandidate> = Vec::new();
        for entry in self.entries {
            entry.expand(ctx, &mut candidates);
        }

        let mut valid_entries: Vec<(LootCandidate, i32)> = Vec::new();
        let mut total_weight = 0;
        for candidate in candidates {
            let weight = candidate.entry.effective_weight(ctx.luck);
            if weight > 0 {
                valid_entries.push((candidate, weight));
                total_weight += weight;
            }
        }

        if valid_entries.is_empty() {
            return;
        }

        let selected = if valid_entries.len() == 1 {
            valid_entries[0].0
        } else {
            let mut index = ctx.rng.random_range(0..total_weight);
            let mut selected_entry = valid_entries[0].0;
            for (candidate, weight) in &valid_entries {
                index -= weight;
                if index < 0 {
                    selected_entry = *candidate;
                    break;
                }
            }
            selected_entry
        };

        selected.create_items(ctx, result);
    }
}

/// One vanilla `LootPoolEntry` produced by `LootPoolEntryContainer.expand`:
/// a singleton entry, or one item of an expanded tag entry.
#[derive(Clone, Copy)]
struct LootCandidate {
    entry: &'static LootEntry,
    tag_item: Option<ItemRef>,
}

impl LootCandidate {
    fn create_items<R: rand::Rng>(self, ctx: &mut LootContext<'_, R>, result: &mut Vec<ItemStack>) {
        match self.tag_item {
            Some(item) => {
                let mut item = ItemStack::new(item);
                apply_functions(self.entry.functions(), &mut item, ctx);
                if item.count > 0 {
                    result.push(item);
                }
            }
            None => self.entry.create_items(ctx, result),
        }
    }
}

fn apply_functions<R: rand::Rng>(
    functions: &[ConditionalLootFunction],
    item: &mut ItemStack,
    ctx: &mut LootContext<'_, R>,
) {
    for cond_func in functions {
        if cond_func.conditions.iter().all(|c| c.test(ctx)) {
            cond_func.function.apply(item, ctx);
        }
    }
}

impl LootEntry {
    /// Vanilla `LootPoolEntryContainer.expand`: returns whether this entry
    /// ran, adding the singleton candidates it yields to `output`.
    fn expand<R: rand::Rng>(
        &'static self,
        ctx: &mut LootContext<'_, R>,
        output: &mut Vec<LootCandidate>,
    ) -> bool {
        if !self.conditions().iter().all(|c| c.test(ctx)) {
            return false;
        }
        match self {
            Self::Alternatives { children, .. } => {
                for child in *children {
                    if child.expand(ctx, output) {
                        return true;
                    }
                }
                false
            }
            Self::Sequence { children, .. } => {
                for child in *children {
                    if !child.expand(ctx, output) {
                        return false;
                    }
                }
                true
            }
            Self::Group { children, .. } => {
                for child in *children {
                    child.expand(ctx, output);
                }
                true
            }
            Self::Tag {
                name, expand: true, ..
            } => {
                // Vanilla `TagEntry.expandTag`: one candidate per tagged item,
                // each with the entry's weight; true only if any item exists.
                let Some(items) = REGISTRY.items.get_tag(name) else {
                    return false;
                };
                let any = !items.is_empty();
                for item in items {
                    output.push(LootCandidate {
                        entry: self,
                        tag_item: Some(item),
                    });
                }
                any
            }
            _ => {
                output.push(LootCandidate {
                    entry: self,
                    tag_item: None,
                });
                true
            }
        }
    }

    /// Create items from this entry and add them to the result.
    fn create_items<R: rand::Rng>(
        &self,
        ctx: &mut LootContext<'_, R>,
        result: &mut Vec<ItemStack>,
    ) {
        match self {
            LootEntry::Item {
                name, functions, ..
            } => {
                if let Some(item_ref) = REGISTRY.items.by_key(name) {
                    let mut item = ItemStack::new(item_ref);

                    // Apply functions
                    for cond_func in *functions {
                        if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                            cond_func.function.apply(&mut item, ctx);
                        }
                    }

                    if item.count > 0 {
                        result.push(item);
                    }
                }
            }
            LootEntry::LootTableRef {
                name, functions, ..
            } => {
                // Recursively get items from referenced loot table
                if let Some(table) = REGISTRY.loot_tables.by_key(name) {
                    let mut items = table.get_random_items(ctx);
                    // Apply functions to all items from the referenced table
                    for item in &mut items {
                        for cond_func in *functions {
                            if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                                cond_func.function.apply(item, ctx);
                            }
                        }
                    }
                    result.extend(items.into_iter().filter(|i| i.count > 0));
                }
            }
            LootEntry::InlineLootTable {
                pools, functions, ..
            } => {
                // Process inline loot table pools directly
                let mut items = Vec::new();
                for pool in *pools {
                    pool.add_random_items(ctx, &mut items);
                }
                // Apply functions to all items from the inline table
                for item in &mut items {
                    for cond_func in *functions {
                        if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                            cond_func.function.apply(item, ctx);
                        }
                    }
                }
                result.extend(items.into_iter().filter(|i| i.count > 0));
            }
            LootEntry::Tag {
                name, functions, ..
            } => {
                if let Some(items) = REGISTRY.items.get_tag(name) {
                    // Vanilla `TagEntry.createItemStack`: a non-expanded tag
                    // drops every tagged item; expanded tags became one
                    // candidate per item in `expand`.
                    for item_ref in items {
                        let mut item = ItemStack::new(item_ref);
                        for cond_func in *functions {
                            if cond_func.conditions.iter().all(|c| c.test(ctx)) {
                                cond_func.function.apply(&mut item, ctx);
                            }
                        }
                        if item.count > 0 {
                            result.push(item);
                        }
                    }
                }
            }
            LootEntry::Alternatives { .. }
            | LootEntry::Group { .. }
            | LootEntry::Sequence { .. } => {
                // Composite entries never become candidates: `expand` replaces
                // them with their children.
            }
            LootEntry::Empty { .. } => {
                // Empty entry produces nothing
            }
            LootEntry::Dynamic { name, .. } => {
                // Vanilla `LootContext.addDynamicDrops`: the block supplies the
                // drops for `name` (decorated pot sherds).
                if let Some(drops) = ctx.dynamic_drops {
                    result.extend(
                        drops
                            .iter()
                            .filter(|(drop_name, _)| drop_name == name)
                            .flat_map(|(_, items)| items.iter().cloned()),
                    );
                }
            }
            LootEntry::Slots {
                slots, functions, ..
            } => {
                // Slots entries select items from specific block entity slots
                // TODO: Implement when block entity system supports slot access
                // This requires:
                // 1. Block entity reference in LootContext
                // 2. Method to get items from specific slots
                // 3. Apply functions to each retrieved item
                let _ = slots;
                let _ = functions;
            }
        }
    }
}
