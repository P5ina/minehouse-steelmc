//! Vanilla `ContextIntProvider` data, limited to the provider shapes of the
//! datapack's `context_int_provider/compostable` folder (generated into
//! [`crate::vanilla_context_int_providers`]).

use rand::RngExt as _;
use steel_utils::{BlockStateId, Identifier};

use crate::loot_table::{LootCondition, LootContext};

/// Vanilla `ContextIntProvider`.
#[derive(Debug)]
pub enum ContextIntProvider {
    /// `ConstantValue` (an inline number).
    Constant(i32),
    /// `NumberDispatcher`: the value of the first case whose condition passes.
    NumberDispatcher {
        cases: &'static [DispatcherCase],
        default: &'static ContextIntProvider,
    },
    /// `WeightedListValue`: one entry picked by weight.
    WeightedList(&'static [WeightedEntry]),
}

/// Vanilla `DispatcherProvider.Case`.
#[derive(Debug)]
pub struct DispatcherCase {
    pub condition: LootCondition,
    pub value: ContextIntProvider,
}

/// One entry of a vanilla `WeightedList`.
#[derive(Debug)]
pub struct WeightedEntry {
    pub data: ContextIntProvider,
    pub weight: i32,
}

impl ContextIntProvider {
    /// Vanilla `getIntUnsafe` in a `BLOCK_INTERACT` context that knows the
    /// block state.
    pub fn get_int<R: rand::Rng>(&self, block_state: Option<BlockStateId>, rng: &mut R) -> i32 {
        match self {
            Self::Constant(value) => *value,
            Self::NumberDispatcher { cases, default } => {
                let selected = {
                    let mut ctx = LootContext::new(&mut *rng);
                    ctx.block_state = block_state;
                    cases
                        .iter()
                        .find(|case| case.condition.test(&mut ctx))
                        .map_or(*default, |case| &case.value)
                };
                selected.get_int(block_state, rng)
            }
            Self::WeightedList(entries) => {
                // Vanilla `WeightedList.getRandomOrThrow`.
                let total: i32 = entries.iter().map(|entry| entry.weight).sum();
                if total <= 0 {
                    // Vanilla throws on an empty list; the datapack has none.
                    return 0;
                }
                let mut roll = rng.random_range(0..total);
                for entry in *entries {
                    roll -= entry.weight;
                    if roll < 0 {
                        return entry.data.get_int(block_state, rng);
                    }
                }
                0
            }
        }
    }
}

/// A named provider of the vanilla datapack.
#[derive(Debug)]
pub struct NamedContextIntProvider {
    pub key: Identifier,
    pub provider: ContextIntProvider,
}
