//! Vanilla sensors shared by several brains.

mod adult;
mod hurt_by;
mod is_in_water;
mod mob;
mod nearest_items;
mod nearest_living_entities;
mod players;
mod tempting;

pub use adult::AdultSensor;
pub use hurt_by::HurtBySensor;
pub use is_in_water::IsInWaterSensor;
pub use mob::MobSensor;
pub use nearest_items::NearestItemSensor;
pub use nearest_living_entities::NearestLivingEntitySensor;
pub use players::PlayerSensor;
pub use tempting::TemptingSensor;
