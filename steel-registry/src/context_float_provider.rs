//! Vanilla `ContextFloatProvider` data, limited to the provider shapes of the
//! datapack's `context_float_provider/brewing` folder (generated into
//! [`crate::vanilla_context_float_providers`]).

use steel_utils::Identifier;

/// Vanilla `ContextFloatProvider`.
#[derive(Debug)]
pub enum ContextFloatProvider {
    /// `ConstantValue` (an inline number).
    Constant(f32),
}

impl ContextFloatProvider {
    /// Vanilla `getFloat`.
    #[must_use]
    pub const fn get_float(&self) -> f32 {
        match self {
            Self::Constant(value) => *value,
        }
    }
}

/// A named provider of the vanilla datapack.
#[derive(Debug)]
pub struct NamedContextFloatProvider {
    pub key: Identifier,
    pub provider: ContextFloatProvider,
}
