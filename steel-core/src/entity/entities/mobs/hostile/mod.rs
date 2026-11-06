//! Hostile entity implementations.
//!
//! Those mobs are aggressive creatures that attack players on sight.

/// Hostile creeper entity module.
pub mod creeper;
pub use creeper::CreeperEntity;

/// Hostile endermite entity module.
pub mod endermite;
pub use endermite::EndermiteEntity;
