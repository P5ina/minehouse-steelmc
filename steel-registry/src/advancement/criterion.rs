//! Advancement criteria as they come from the datapack.
//!
//! The conditions are kept as their JSON text: matching them needs the world,
//! entities and players, so `steel-core` parses them into typed trigger
//! instances (vanilla `CriterionTrigger.codec()`).

use steel_utils::Identifier;

/// Vanilla `Criterion`: a trigger type and that trigger's conditions.
#[derive(Debug)]
pub struct Criterion {
    /// The trigger type id, such as `minecraft:inventory_changed`.
    pub trigger: Identifier,
    /// The `conditions` object as JSON text, `{}` when absent.
    pub conditions: &'static str,
}

impl Criterion {
    /// Creates a criterion from its trigger id and conditions JSON.
    #[must_use]
    pub const fn new(trigger: Identifier, conditions: &'static str) -> Self {
        Self {
            trigger,
            conditions,
        }
    }
}
