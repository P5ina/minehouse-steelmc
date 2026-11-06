//! Sensors fill brain memories on a fixed scan rate (vanilla `Sensor`).

use std::sync::Arc;

use super::memory::Memories;
use crate::world::World;

/// Default ticks between two scans of a sensor.
pub const DEFAULT_SCAN_RATE: i32 = 20;

/// Reads the world around `E` and writes memories.
pub trait Sensor<E: ?Sized>: Send {
    /// Ticks between two scans.
    fn scan_rate(&self) -> i32 {
        DEFAULT_SCAN_RATE
    }

    /// Registers the memories this sensor writes (vanilla `Sensor.requires`).
    fn register_memories(&self, memories: &mut Memories);

    /// Scans once (vanilla `Sensor.doTick`).
    fn do_tick(&mut self, world: &Arc<World>, body: &E);
}

/// A sensor with its countdown to the next scan.
pub(super) struct SensorSlot<E: ?Sized> {
    sensor: Box<dyn Sensor<E>>,
    time_to_tick: i64,
}

impl<E: ?Sized> SensorSlot<E> {
    /// Wraps `sensor`, delaying its first scan by a random part of its
    /// scan rate (vanilla `Sensor.randomlyDelayStart`).
    pub(super) fn new(sensor: Box<dyn Sensor<E>>) -> Self {
        let scan_rate = sensor.scan_rate().max(1);
        Self {
            sensor,
            time_to_tick: i64::from(rand::random_range(0..scan_rate)),
        }
    }

    pub(super) fn sensor(&self) -> &dyn Sensor<E> {
        self.sensor.as_ref()
    }

    /// Vanilla `Sensor.tick`.
    // TODO: Vanilla also refreshes the shared sensor targeting conditions to the
    // body's follow range here; add it with the first sensor that targets entities.
    pub(super) fn tick(&mut self, world: &Arc<World>, body: &E) {
        self.time_to_tick -= 1;
        if self.time_to_tick <= 0 {
            self.time_to_tick = i64::from(self.sensor.scan_rate());
            self.sensor.do_tick(world, body);
        }
    }
}
