//! Shared item-frame entity behavior.

use super::HangingEntity;

/// Vanilla `ItemFrame` behavior used by comparators.
pub trait ItemFrame: HangingEntity {
    /// Returns the comparator signal strength for this frame: 0 when empty,
    /// otherwise derived from the framed item's rotation.
    fn analog_output(&self) -> i32;
}
