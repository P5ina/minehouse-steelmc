use std::sync::OnceLock;

use rustc_hash::FxHashMap;
use simdnbt::ToNbtTag;
use simdnbt::owned::NbtTag;
use steel_utils::Identifier;

use crate::TaggedRegistryExt;
use crate::sound_event::SoundEventRef;
use crate::{REGISTRY, RegistryTags};

#[derive(Debug)]
pub struct Biome {
    pub key: Identifier,
    pub has_precipitation: bool,
    pub temperature: f32,
    pub downfall: f32,
    pub temperature_modifier: TemperatureModifier,
    pub effects: BiomeEffects,
    pub creature_spawn_probability: f32,
    /// The `minecraft:gameplay/snow_golem_melts` environment attribute, when this biome sets it.
    pub snow_golem_melts: Option<bool>,
    /// The `minecraft:gameplay/can_pillager_patrol_spawn` environment attribute, when this
    /// biome sets it.
    pub can_pillager_patrol_spawn: Option<bool>,
    pub spawners: FxHashMap<String, Vec<SpawnerData>>,
    pub spawn_costs: FxHashMap<Identifier, SpawnCost>,
    pub carvers: Vec<Identifier>,
    pub features: Vec<Vec<Identifier>>,
    /// The syncable environment attributes as JSON, as sent to the client.
    pub network_attributes: &'static str,
    /// Cached registry ID, set during registration for O(1) lookup on hot paths.
    pub id: OnceLock<usize>,
}

impl Biome {
    /// Returns `true` if this biome is tagged with the given tag.
    pub fn has_tag(&'static self, tag: &Identifier) -> bool {
        REGISTRY.biomes.is_in_tag(self, tag)
    }
}

#[derive(Debug)]
pub struct BiomeEffects {
    pub fog_color: i32,
    pub sky_color: i32,
    pub water_color: i32,
    pub water_fog_color: i32,
    pub foliage_color: Option<i32>,
    pub grass_color: Option<i32>,
    pub dry_foliage_color: Option<i32>,
    pub grass_color_modifier: GrassColorModifier,
    pub music: Option<Vec<WeightedMusic>>,
    pub ambient_sound: Option<SoundEventRef>,
    pub additions_sound: Option<AdditionsSound>,
    pub mood_sound: Option<MoodSound>,
    pub particle: Option<Particle>,
}

#[derive(Debug)]
pub struct SpawnerData {
    pub entity_type: Identifier,
    pub weight: i32,
    pub min_count: i32,
    pub max_count: i32,
}

#[derive(Debug)]
pub struct SpawnCost {
    pub energy_budget: f64,
    pub charge: f64,
}

#[derive(Debug, Default)]
pub enum TemperatureModifier {
    #[default]
    None,
    Frozen,
}

#[derive(Debug)]
pub enum GrassColorModifier {
    None,
    DarkForest,
    Swamp,
}

#[derive(Debug)]
pub struct WeightedMusic {
    pub data: Music,
    pub weight: i32,
}

#[derive(Debug)]
pub struct Music {
    pub replace_current_music: bool,
    pub max_delay: i32,
    pub min_delay: i32,
    pub sound: SoundEventRef,
}

#[derive(Debug)]
pub struct AdditionsSound {
    pub sound: SoundEventRef,
    pub tick_chance: f64,
}

#[derive(Debug)]
pub struct MoodSound {
    pub sound: SoundEventRef,
    pub tick_delay: i32,
    pub block_search_extent: i32,
    pub offset: f64,
}

#[derive(Debug)]
pub struct Particle {
    pub options: ParticleOptions,
    pub probability: f32,
}

#[derive(Debug)]
pub struct ParticleOptions {
    pub particle_type: Identifier,
}

/// Encodes the biome like vanilla `Biome.NETWORK_CODEC`: climate settings, the syncable
/// environment attributes and the special effects. Generation settings and spawn data stay
/// on the server.
impl ToNbtTag for &Biome {
    fn to_nbt_tag(self) -> NbtTag {
        use simdnbt::owned::NbtCompound;
        let mut compound = NbtCompound::new();
        compound.insert("has_precipitation", self.has_precipitation);
        compound.insert("temperature", self.temperature);
        if let TemperatureModifier::Frozen = self.temperature_modifier {
            compound.insert("temperature_modifier", "frozen");
        }
        compound.insert("downfall", self.downfall);

        let attributes: serde_json::Value = serde_json::from_str(self.network_attributes)
            .expect("generated biome attributes are valid JSON");
        if let NbtTag::Compound(attributes) = json_to_nbt(&attributes)
            && !attributes.is_empty()
        {
            compound.insert("attributes", NbtTag::Compound(attributes));
        }

        let mut effects = NbtCompound::new();
        effects.insert("water_color", self.effects.water_color);
        if let Some(fc) = self.effects.foliage_color {
            effects.insert("foliage_color", fc);
        }
        if let Some(dfc) = self.effects.dry_foliage_color {
            effects.insert("dry_foliage_color", dfc);
        }
        if let Some(gc) = self.effects.grass_color {
            effects.insert("grass_color", gc);
        }
        match self.effects.grass_color_modifier {
            GrassColorModifier::None => {}
            GrassColorModifier::DarkForest => {
                effects.insert("grass_color_modifier", "dark_forest");
            }
            GrassColorModifier::Swamp => {
                effects.insert("grass_color_modifier", "swamp");
            }
        }
        compound.insert("effects", NbtTag::Compound(effects));

        NbtTag::Compound(compound)
    }
}

/// Converts datapack JSON to NBT the way `JsonOps` → `NbtOps` would: booleans become bytes,
/// integral numbers ints and other numbers floats.
fn json_to_nbt(value: &serde_json::Value) -> NbtTag {
    use serde_json::Value;
    use simdnbt::owned::{NbtCompound, NbtList};
    match value {
        Value::Null => NbtTag::Compound(NbtCompound::new()),
        Value::Bool(b) => NbtTag::Byte(i8::from(*b)),
        Value::Number(n) => match n.as_i64().and_then(|i| i32::try_from(i).ok()) {
            Some(i) => NbtTag::Int(i),
            #[expect(clippy::cast_possible_truncation, reason = "datapack floats are f32")]
            None => NbtTag::Float(n.as_f64().unwrap_or_default() as f32),
        },
        Value::String(s) => NbtTag::String(s.as_str().into()),
        Value::Array(items) => {
            let tags: Vec<NbtTag> = items.iter().map(json_to_nbt).collect();
            NbtTag::List(if tags.iter().all(|t| matches!(t, NbtTag::Compound(_))) {
                NbtList::Compound(
                    tags.into_iter()
                        .filter_map(|t| match t {
                            NbtTag::Compound(c) => Some(c),
                            _ => None,
                        })
                        .collect(),
                )
            } else if tags.iter().all(|t| matches!(t, NbtTag::String(_))) {
                NbtList::String(
                    tags.into_iter()
                        .filter_map(|t| match t {
                            NbtTag::String(s) => Some(s),
                            _ => None,
                        })
                        .collect(),
                )
            } else {
                // Mixed or numeric lists: wrap each element like NbtOps does for
                // heterogeneous lists.
                NbtList::Compound(
                    tags.into_iter()
                        .map(|t| {
                            let mut c = NbtCompound::new();
                            c.insert("", t);
                            c
                        })
                        .collect(),
                )
            })
        }
        Value::Object(map) => {
            let mut compound = NbtCompound::new();
            for (key, value) in map {
                compound.insert(key.as_str(), json_to_nbt(value));
            }
            NbtTag::Compound(compound)
        }
    }
}

pub type BiomeRef = &'static Biome;

pub struct BiomeRegistry {
    biomes_by_id: Vec<BiomeRef>,
    biomes_by_key: FxHashMap<Identifier, usize>,
    tags: RegistryTags,
    allows_registering: bool,
}

impl BiomeRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            biomes_by_id: Vec::new(),
            biomes_by_key: FxHashMap::default(),
            tags: RegistryTags::default(),
            allows_registering: true,
        }
    }
}

impl BiomeRegistry {
    pub fn register(&mut self, entry: BiomeRef) -> usize {
        assert!(
            self.allows_registering,
            "Cannot register Biome after registry has been frozen"
        );
        let id = self.biomes_by_id.len();
        let cached = entry.id.get_or_init(|| id);
        assert_eq!(*cached, id, "biome registered with conflicting id");
        self.biomes_by_id.push(entry);
        self.biomes_by_key.insert(entry.key.clone(), id);
        id
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, BiomeRef)> + '_ {
        self.biomes_by_id
            .iter()
            .enumerate()
            .map(|(id, &entry)| (id, entry))
    }
}

impl Default for BiomeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

crate::impl_registry_ext!(BiomeRegistry, Biome, biomes_by_id, biomes_by_key);
crate::impl_tagged_registry!(BiomeRegistry, biomes_by_key, "biome");

crate::impl_registry_entry_eq!(Biome);

impl crate::RegistryEntry for Biome {
    fn key(&self) -> &Identifier {
        &self.key
    }

    fn try_id(&self) -> Option<usize> {
        self.id.get().copied()
    }
}

#[cfg(test)]
mod tests {
    use simdnbt::ToNbtTag;
    use simdnbt::owned::NbtTag;

    use crate::vanilla_biomes;

    fn compound(tag: &NbtTag) -> &simdnbt::owned::NbtCompound {
        match tag {
            NbtTag::Compound(c) => c,
            other => panic!("expected compound, got {other:?}"),
        }
    }

    /// Nether fog comes from the biome's `visual/fog_color`; without it the client draws the
    /// attribute default instead of the vanilla red haze.
    #[test]
    fn nether_wastes_sends_fog_color_attribute() {
        let tag = (&*vanilla_biomes::NETHER_WASTES).to_nbt_tag();
        let biome = compound(&tag);
        let attributes = compound(biome.get("attributes").expect("attributes sent"));
        assert_eq!(
            attributes
                .get("minecraft:visual/fog_color")
                .and_then(NbtTag::string)
                .map(ToString::to_string),
            Some("#330808".to_string())
        );
        assert!(
            attributes
                .get("minecraft:gameplay/natural_mob_spawns")
                .is_none(),
            "non-syncable attributes stay on the server"
        );
        let effects = compound(biome.get("effects").expect("effects sent"));
        assert!(effects.get("fog_color").is_none());
        assert!(biome.get("features").is_none());
    }
}
