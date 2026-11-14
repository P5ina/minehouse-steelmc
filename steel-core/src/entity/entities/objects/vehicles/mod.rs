//! Vehicle entity implementations.

mod abstract_boat;
mod abstract_minecart;
mod boat;
mod chest_minecart;
mod furnace_minecart;
mod hopper_minecart;
mod minecart;
mod old_minecart_behavior;
mod tnt_minecart;
mod vehicle_container;
mod vehicle_entity;

pub use abstract_boat::{AbstractBoat, BoatCore, BoatStatus};
pub use abstract_minecart::{AbstractMinecart, MinecartCore};
pub use boat::{BoatEntity, ChestBoatEntity, ChestRaftEntity, RaftEntity};
pub use chest_minecart::ChestMinecartEntity;
pub use furnace_minecart::FurnaceMinecartEntity;
pub use hopper_minecart::HopperMinecartEntity;
pub use minecart::MinecartEntity;
pub use tnt_minecart::TntMinecartEntity;
pub use vehicle_container::redstone_signal_from_container;
pub use vehicle_entity::VehicleEntity;
