//! Passive entity implementations.
/// Those mobs are passive creatures that run away when attacked by a player.
mod armadillo;
mod chicken;
mod cow;
mod mooshroom;
mod pig;
mod sheep;
mod squid;

pub use armadillo::ArmadilloEntity;
pub use chicken::ChickenEntity;
pub use cow::CowEntity;
pub use mooshroom::MushroomCowEntity;
pub use pig::PigEntity;
pub use sheep::SheepEntity;
pub use squid::SquidEntity;
