//! Vanilla fish: cod, salmon, pufferfish and tropical fish.

mod cod;
mod pufferfish;
mod salmon;
mod tropical_fish;

pub use cod::CodEntity;
pub use pufferfish::PufferfishEntity;
pub use salmon::{SalmonEntity, SalmonVariant};
pub use tropical_fish::{
    COMMON_TROPICAL_FISH_VARIANTS, TropicalFishBase, TropicalFishEntity, TropicalFishPattern,
    TropicalFishVariant,
};
