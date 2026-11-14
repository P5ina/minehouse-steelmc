//! Normal mob-spawner block entity and its persistent spawn configuration.

mod base_spawner;
mod spawn_data;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Weak};

use simdnbt::borrow::BaseNbtCompound as BorrowedNbtCompound;
use simdnbt::owned::NbtCompound;
use steel_registry::data_components::components::EntityData;
use steel_registry::entity_type::EntityTypeRef;
use steel_registry::vanilla_block_entity_types;
use steel_utils::{BlockPos, BlockStateId, DowncastType, DowncastTypeKey};

use crate::block_entity::{BlockEntity, BlockEntityBase};
use crate::world::World;

pub use base_spawner::{BaseSpawner, BaseSpawnerConfig};
pub(crate) use base_spawner::{
    choose_weighted, configured_or_random_position, entity_type_from_owned_nbt, equip_from_table,
};
pub use spawn_data::{CustomSpawnRules, EquipmentTable, LightRange, SpawnData, WeightedSpawnData};

/// Vanilla `Spawner`: a block entity a spawn egg can set the mob of.
pub trait Spawner {
    /// Sets the next spawn entity type.
    fn set_entity_id(&self, entity_type: EntityTypeRef);

    /// Sets the next spawn entity type and merges the rest of `entity_data` into it.
    fn set_entity_data(&self, entity_data: &EntityData);
}

/// Concrete normal mob-spawner block entity.
pub struct SpawnerBlockEntity {
    base: BlockEntityBase,
    spawner: BaseSpawner,
}

// SAFETY: This key is owned by Steel and uniquely identifies `SpawnerBlockEntity`.
unsafe impl DowncastType for SpawnerBlockEntity {
    const TYPE_KEY: DowncastTypeKey = DowncastTypeKey::new("steel:block_entity/spawner");
}

impl SpawnerBlockEntity {
    /// Creates a normal mob-spawner block entity.
    #[must_use]
    pub fn new(level: Weak<World>, pos: BlockPos, state: BlockStateId) -> Self {
        Self {
            base: BlockEntityBase::new(&vanilla_block_entity_types::MOB_SPAWNER, level, pos, state),
            spawner: BaseSpawner::new(),
        }
    }

    fn save_and_sync(&self) {
        self.set_changed();
        if let Some(world) = self.get_level() {
            world.send_block_updated(self.get_block_pos());
        }
    }
}

impl Spawner for SpawnerBlockEntity {
    fn set_entity_id(&self, entity_type: EntityTypeRef) {
        self.spawner.set_entity_id(entity_type);
        self.save_and_sync();
    }

    fn set_entity_data(&self, entity_data: &EntityData) {
        self.spawner.set_entity_data(entity_data);
        self.save_and_sync();
    }
}

impl SpawnerBlockEntity {
    /// Returns the ticks left until the next spawn cycle.
    #[must_use]
    pub fn spawn_delay(&self) -> i32 {
        self.spawner.spawn_delay()
    }

    /// Sets the ticks left until the next spawn cycle.
    pub fn set_spawn_delay(&self, delay: i32) {
        self.spawner.set_spawn_delay(delay);
        self.save_and_sync();
    }

    /// Returns the spawn-cycle limits.
    #[must_use]
    pub fn config(&self) -> BaseSpawnerConfig {
        self.spawner.config()
    }

    /// Replaces the spawn-cycle limits.
    pub fn set_config(&self, config: BaseSpawnerConfig) {
        self.spawner.set_config(config);
        self.save_and_sync();
    }

    /// Returns the weighted entries the next spawn data is picked from.
    #[must_use]
    pub fn spawn_potentials(&self) -> Vec<WeightedSpawnData> {
        self.spawner.spawn_potentials()
    }

    /// Replaces the weighted entries the next spawn data is picked from.
    pub fn set_spawn_potentials(&self, potentials: Vec<WeightedSpawnData>) {
        self.spawner.set_spawn_potentials(potentials);
        self.save_and_sync();
    }

    /// Returns the active spawn data, if one has been selected.
    #[must_use]
    pub fn next_spawn_data(&self) -> Option<SpawnData> {
        self.spawner.next_spawn_data()
    }

    /// Replaces the active spawn data.
    pub fn set_next_spawn_data(&self, data: SpawnData) {
        self.spawner.set_next_spawn_data(data);
        self.save_and_sync();
    }
}

impl BlockEntity for SpawnerBlockEntity {
    fn base(&self) -> &BlockEntityBase {
        &self.base
    }

    fn load_additional(&self, nbt: &BorrowedNbtCompound<'_>) {
        self.spawner.load(nbt);
    }

    fn save_additional(&self, nbt: &mut NbtCompound) {
        self.spawner.save(nbt);
    }

    fn get_update_tag(&self) -> Option<NbtCompound> {
        let mut nbt = self.save_custom_only();
        nbt.remove("SpawnPotentials");
        Some(nbt)
    }

    fn trigger_event(&self, event: i32, _data: i32) -> bool {
        self.spawner.on_event_triggered(event)
    }

    fn as_spawner(&self) -> Option<&dyn Spawner> {
        Some(self)
    }

    fn tick(&self, world: &Arc<World>) {
        let result = self.spawner.server_tick(world, self.get_block_pos());
        if result.state_changed {
            self.set_changed();
        }
        if result.next_spawn_data_changed {
            world.send_block_updated(self.get_block_pos());
        }
    }
}
