//! Passive entity implementations.
/// Those mobs are passive creatures that run away when attacked by a player.
mod armadillo;
mod axolotl;
mod chicken;
mod cow;
mod dolphin;
/// Horses, donkeys, mules and the undead horses.
pub mod equine;
mod feline;
mod fish;
mod goat;
mod mooshroom;
mod nautilus;
mod parrot;
mod pig;
mod sheep;
mod squid;

pub use armadillo::ArmadilloEntity;
pub use axolotl::AxolotlEntity;
pub use chicken::ChickenEntity;
pub use cow::CowEntity;
pub use dolphin::DolphinEntity;
pub use equine::{
    CamelEntity, CamelHuskEntity, DonkeyEntity, HorseEntity, LlamaEntity, MuleEntity,
    SkeletonHorseEntity, TraderLlamaEntity, ZombieHorseEntity,
};
pub use feline::{CatEntity, OcelotEntity};
pub use fish::{
    COMMON_TROPICAL_FISH_VARIANTS, CodEntity, PufferfishEntity, SalmonEntity, SalmonVariant,
    TropicalFishBase, TropicalFishEntity, TropicalFishPattern, TropicalFishVariant,
};
pub use goat::GoatEntity;
pub use mooshroom::MushroomCowEntity;
pub use nautilus::{AbstractNautilus, NautilusEntity, ZombieNautilusEntity};
pub use parrot::ParrotEntity;
pub use pig::PigEntity;
pub use sheep::SheepEntity;
pub use squid::{GlowSquidEntity, Squid, SquidEntity};
mod strider;
pub use strider::StriderEntity;
mod panda;
pub use panda::{PandaEntity, PandaGene};
mod fox;
pub use fox::FoxEntity;
