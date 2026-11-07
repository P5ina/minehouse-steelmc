//! Vanilla sensors shared by several brains.

mod adult;
mod hurt_by;
mod mob;
mod nearest_living_entities;
mod player;
mod tempting;

pub use adult::AdultSensor;
pub use hurt_by::HurtBySensor;
pub use mob::MobSensor;
pub use nearest_living_entities::NearestLivingEntitySensor;
pub use player::PlayerSensor;
pub use tempting::TemptingSensor;
