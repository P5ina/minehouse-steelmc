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

/// Skeleton, stray, bogged, wither skeleton and parched entity module.
pub mod skeleton;
pub use skeleton::{
    BoggedEntity, ParchedEntity, SkeletonEntity, StrayEntity, WitherSkeletonEntity,
};

/// Hostile spider and cave spider entity module.
pub mod spider;
pub use spider::{CaveSpiderEntity, SpiderEntity};

/// Blaze, the Nether fortress fireball shooter.
pub mod blaze;
pub use blaze::BlazeEntity;
/// Hostile zombie, husk and giant entity module.
pub mod zombie;
pub use zombie::{GiantEntity, HuskEntity, ZombieEntity, ZombieGroupData};

/// Hostile vex entity module.
pub mod vex;
pub use vex::VexEntity;

/// Ghast, the floating Nether fireball shooter.
pub mod ghast;
pub use ghast::GhastEntity;

/// Hostile guardian and elder guardian entity module.
pub mod guardian;
pub use guardian::{ElderGuardianEntity, Guardian, GuardianEntity};

/// Phantom, the undead flyer that hunts players who skip sleep.
pub mod phantom;
pub use phantom::PhantomEntity;
