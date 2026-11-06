//! Vanilla-shaped mob AI foundations.
#![expect(
    dead_code,
    reason = "pathfinding controls are foundation code consumed by upcoming goals and navigation"
)]

pub mod brain;
pub mod control;
pub mod goal;
pub mod navigation;
pub mod node;
pub mod node_evaluator;
pub mod path;
pub mod pathfinder;
pub(crate) mod sensing;
pub(crate) mod smooth_swimming;
pub mod swim;
pub(crate) mod targeting;
pub mod walk;
