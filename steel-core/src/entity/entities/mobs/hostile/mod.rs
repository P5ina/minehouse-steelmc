//! Hostile entity implementations.
//!
//! Those mobs are aggressive creatures that attack players on sight.

/// Hostile creeper entity module.
pub mod creeper;
pub use creeper::CreeperEntity;

/// Hostile endermite entity module.
pub mod endermite;
pub use endermite::EndermiteEntity;

/// Slime and magma cube, the vanilla `AbstractCubeMob` family.
pub mod cube_mob;
pub use cube_mob::{MagmaCubeEntity, SlimeEntity};

/// Hostile spider and cave spider entity module.
pub mod spider;
pub use spider::{CaveSpiderEntity, SpiderEntity};

/// Hostile zombie, husk and giant entity module.
pub mod zombie;
pub use zombie::{GiantEntity, HuskEntity, ZombieEntity, ZombieGroupData};

/// Hostile vex entity module.
pub mod vex;
pub use vex::VexEntity;

/// Hostile guardian and elder guardian entity module.
pub mod guardian;
pub use guardian::{ElderGuardianEntity, Guardian, GuardianEntity};
