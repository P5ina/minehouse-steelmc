//! Domain-wide map storage (vanilla keeps map data in the overworld's `DimensionDataStorage`).

mod banner;
mod data;

use std::io;
use std::sync::{Arc, OnceLock};

use rustc_hash::FxHashMap;
use steel_registry::data_components::components::MapId;
use steel_utils::locks::SyncMutex;
use steel_utils::saved_data::{SavedDataManager, names as saved_data_names};
use wincode::{SchemaRead, SchemaWrite};

pub use banner::MapBanner;
pub(crate) use data::java_floor;
pub use data::{HoldingPlayer, MAP_SIZE, MAX_SCALE, MapItemSavedData, TRACKED_DECORATION_LIMIT};

/// A loaded map, shared by every world in the domain.
pub type SharedMapData = Arc<SyncMutex<MapItemSavedData>>;

/// Vanilla `MapIndex.NO_MAP_ID`.
const NO_MAP_ID: i32 = -1;

#[derive(SchemaWrite, SchemaRead)]
struct MapFile {
    /// Vanilla `MapItemSavedData.CODEC` as NBT.
    nbt: Vec<u8>,
}

#[derive(SchemaWrite, SchemaRead)]
struct MapIndexFile {
    last_map_id: i32,
}

struct MapIndex {
    last_map_id: i32,
    dirty: bool,
}

/// The maps of one domain, persisted through the domain default world.
pub struct DomainMaps {
    storage: OnceLock<SavedDataManager>,
    index: SyncMutex<Option<MapIndex>>,
    /// Loaded maps; `None` caches a missing id like vanilla's data storage.
    maps: SyncMutex<FxHashMap<i32, Option<SharedMapData>>>,
}

impl DomainMaps {
    /// Creates empty map storage; it persists once [`Self::set_storage`] is called.
    #[must_use]
    pub fn new() -> Self {
        Self {
            storage: OnceLock::new(),
            index: SyncMutex::new(None),
            maps: SyncMutex::new(FxHashMap::default()),
        }
    }

    /// Binds the storage of the domain default world. Later calls are ignored.
    pub fn set_storage(&self, storage: SavedDataManager) {
        let _ = self.storage.set(storage);
    }

    /// Vanilla `Level.getMapData`.
    #[must_use]
    pub fn get(&self, id: MapId) -> Option<SharedMapData> {
        let mut maps = self.maps.lock();
        if let Some(loaded) = maps.get(&id.id()) {
            return loaded.clone();
        }
        let loaded = self.load(id).map(|data| Arc::new(SyncMutex::new(data)));
        maps.insert(id.id(), loaded.clone());
        loaded
    }

    /// Vanilla `Level.setMapData`: the new map is saved on the next save.
    pub fn set(&self, id: MapId, mut data: MapItemSavedData) -> SharedMapData {
        data.set_dirty();
        let shared = Arc::new(SyncMutex::new(data));
        self.maps.lock().insert(id.id(), Some(Arc::clone(&shared)));
        shared
    }

    /// Vanilla `Level.getFreeMapId` through `MapIndex.getNextMapId`.
    pub fn free_map_id(&self) -> MapId {
        let mut index = self.index.lock();
        let index = index.get_or_insert_with(|| MapIndex {
            last_map_id: self.load_last_map_id(),
            dirty: false,
        });
        index.last_map_id += 1;
        index.dirty = true;
        MapId::new(index.last_map_id)
    }

    fn load(&self, id: MapId) -> Option<MapItemSavedData> {
        let storage = self.storage.get()?;
        match storage.sync_load_numbered_wincode::<MapFile>(saved_data_names::MAPS, id.id()) {
            Ok(Some(file)) => match MapItemSavedData::from_nbt_bytes(&file.nbt) {
                Ok(data) => Some(data),
                Err(error) => {
                    log::error!("Failed to read map {}: {error}", id.id());
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                log::error!("Failed to load map {}: {error}", id.id());
                None
            }
        }
    }

    fn load_last_map_id(&self) -> i32 {
        let Some(storage) = self.storage.get() else {
            return NO_MAP_ID;
        };
        match storage.sync_load_wincode::<MapIndexFile>(saved_data_names::MAP_INDEX) {
            Ok(file) => file.map_or(NO_MAP_ID, |file| file.last_map_id),
            Err(error) => {
                log::error!("Failed to load the map index: {error}");
                NO_MAP_ID
            }
        }
    }

    /// Writes the map index and every dirty map, returning how many maps were written.
    pub fn save(&self) -> io::Result<usize> {
        let Some(storage) = self.storage.get() else {
            return Ok(0);
        };

        let last_map_id = {
            let mut index = self.index.lock();
            index.as_mut().filter(|index| index.dirty).map(|index| {
                index.dirty = false;
                index.last_map_id
            })
        };
        if let Some(last_map_id) = last_map_id {
            storage
                .sync_save_wincode(saved_data_names::MAP_INDEX, &MapIndexFile { last_map_id })?;
        }

        let loaded: Vec<(i32, SharedMapData)> = self
            .maps
            .lock()
            .iter()
            .filter_map(|(id, data)| data.as_ref().map(|data| (*id, Arc::clone(data))))
            .collect();
        let mut saved = 0;
        for (id, data) in loaded {
            let nbt = {
                let mut data = data.lock();
                if !data.is_dirty() {
                    continue;
                }
                data.clear_dirty();
                data.to_nbt_bytes()
            };
            storage.sync_save_numbered_wincode(saved_data_names::MAPS, id, &MapFile { nbt })?;
            saved += 1;
        }
        Ok(saved)
    }
}

impl Default for DomainMaps {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::env::temp_dir;
    use std::time::{SystemTime, UNIX_EPOCH};

    use steel_utils::Identifier;

    use super::*;

    #[test]
    fn maps_and_the_index_survive_a_save() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after the epoch")
            .as_nanos();
        let dir = temp_dir().join(format!("steel-maps-{unique}"));
        let storage = SavedDataManager::new(Some(dir.as_path()));

        let maps = DomainMaps::new();
        maps.set_storage(storage.clone());
        let id = maps.free_map_id();
        assert_eq!(id, MapId::new(0));
        let mut data = MapItemSavedData::create_fresh(
            300.0,
            -20.0,
            2,
            true,
            false,
            Identifier::new_static("minecraft", "the_nether"),
        );
        data.set_color(5, 7, 46);
        maps.set(id, data);
        assert_eq!(maps.save().expect("maps save"), 1);
        assert_eq!(maps.save().expect("nothing left to save"), 0);

        let reloaded = DomainMaps::new();
        reloaded.set_storage(storage);
        let data = reloaded.get(id).expect("the saved map loads");
        {
            let data = data.lock();
            assert_eq!((data.center_x, data.center_z, data.scale), (192, 192, 2));
            assert_eq!(data.dimension.path, "the_nether");
            assert_eq!(data.color(5, 7), 46);
            assert!(!data.is_dirty());
        }
        assert!(reloaded.get(MapId::new(9)).is_none());
        assert_eq!(reloaded.free_map_id(), MapId::new(1));
    }
}
