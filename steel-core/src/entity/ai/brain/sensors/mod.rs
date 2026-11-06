//! Vanilla sensors shared by several brains.

mod adult;
mod hurt_by;
mod nearest_living_entities;
mod tempting;

#[expect(
    unused_imports,
    reason = "the brain mobs that follow register these sensors"
)]
pub use adult::AdultSensor;
#[expect(
    unused_imports,
    reason = "the brain mobs that follow register these sensors"
)]
pub use hurt_by::HurtBySensor;
#[expect(
    unused_imports,
    reason = "the brain mobs that follow register these sensors"
)]
pub use nearest_living_entities::NearestLivingEntitySensor;
#[expect(
    unused_imports,
    reason = "the brain mobs that follow register these sensors"
)]
pub use tempting::TemptingSensor;
