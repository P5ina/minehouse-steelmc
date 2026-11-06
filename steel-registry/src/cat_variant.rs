use rustc_hash::FxHashMap;
use simdnbt::ToNbtTag;
use simdnbt::owned::NbtTag;
use std::cmp::Ordering;
use std::str::FromStr as _;

use steel_utils::Identifier;
use steel_utils::random::Random;

use crate::biome::BiomeRef;
use crate::{REGISTRY, TaggedRegistryExt as _};

/// Represents a full cat variant definition from a data pack JSON file.
#[derive(Debug)]
pub struct CatVariant {
    pub key: Identifier,
    pub asset_id: Identifier,
    pub baby_asset_id: Identifier,
    pub spawn_conditions: &'static [SpawnConditionEntry],
}

/// A single entry in the list of spawn conditions.
#[derive(Debug)]
pub struct SpawnConditionEntry {
    pub priority: i32,
    pub condition: Option<SpawnCondition>,
}

/// Defines various spawn conditions for cat variants.
#[derive(Debug)]
pub enum SpawnCondition {
    Structure { structures: &'static str },
    MoonBrightness { min: Option<f32>, max: Option<f32> },
    Biome { biomes: &'static str },
}

impl ToNbtTag for &CatVariant {
    fn to_nbt_tag(self) -> NbtTag {
        use simdnbt::owned::{NbtCompound, NbtList, NbtTag};
        let mut compound = NbtCompound::new();
        compound.insert("asset_id", self.asset_id.clone());
        compound.insert("baby_asset_id", self.baby_asset_id.clone());
        let conditions: Vec<NbtCompound> = self
            .spawn_conditions
            .iter()
            .map(|entry| {
                let mut e = NbtCompound::new();
                e.insert("priority", entry.priority);
                if let Some(cond) = &entry.condition {
                    let mut c = NbtCompound::new();
                    match cond {
                        SpawnCondition::Structure { structures } => {
                            c.insert("type", "minecraft:in_structure");
                            c.insert("structures", *structures);
                        }
                        SpawnCondition::MoonBrightness { min, max } => {
                            c.insert("type", "minecraft:moon_brightness");
                            if let Some(min) = min {
                                c.insert("min", *min);
                            }
                            if let Some(max) = max {
                                c.insert("max", *max);
                            }
                        }
                        SpawnCondition::Biome { biomes } => {
                            c.insert("type", "minecraft:biome");
                            c.insert("biomes", *biomes);
                        }
                    }
                    e.insert("condition", NbtTag::Compound(c));
                }
                e
            })
            .collect();
        compound.insert(
            "spawn_conditions",
            NbtTag::List(NbtList::Compound(conditions)),
        );
        NbtTag::Compound(compound)
    }
}

impl SpawnCondition {
    /// Vanilla `BiomeCheck.test`: `biomes` is a biome id or a `#tag`.
    #[must_use]
    pub fn matches_biome(biomes: &str, biome: BiomeRef) -> bool {
        if let Some(tag) = biomes.strip_prefix('#') {
            Identifier::from_str(tag).is_ok_and(|tag| REGISTRY.biomes.is_in_tag(biome, &tag))
        } else {
            Identifier::from_str(biomes).is_ok_and(|key| biome.key == key)
        }
    }

    /// Vanilla `MoonBrightnessCheck.test`: `MinMaxBounds.Doubles.matches`,
    /// inclusive at both ends.
    #[must_use]
    pub fn matches_moon_brightness(min: Option<f32>, max: Option<f32>, brightness: f32) -> bool {
        min.is_none_or(|min| brightness >= min) && max.is_none_or(|max| brightness <= max)
    }
}

pub type CatVariantRef = &'static CatVariant;

pub struct CatVariantRegistry {
    cat_variants_by_id: Vec<CatVariantRef>,
    cat_variants_by_key: FxHashMap<Identifier, usize>,
    allows_registering: bool,
}

impl CatVariantRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cat_variants_by_id: Vec::new(),
            cat_variants_by_key: FxHashMap::default(),
            allows_registering: true,
        }
    }
}

impl CatVariantRegistry {
    /// Vanilla `VariantUtils.selectVariantToSpawn` for cats, using vanilla
    /// `PriorityProvider.pick`: among the variants whose conditions `matches`
    /// accepts, a random one of the highest priority.
    #[must_use]
    pub fn select_spawn_variant(
        &self,
        matches: impl Fn(&SpawnCondition) -> bool,
        random: &mut impl Random,
    ) -> Option<CatVariantRef> {
        let mut selected = Vec::new();
        let mut highest_priority = i32::MIN;
        for (_, variant) in self.iter() {
            for entry in variant.spawn_conditions {
                if !entry.condition.as_ref().is_none_or(&matches) {
                    continue;
                }
                match entry.priority.cmp(&highest_priority) {
                    Ordering::Greater => {
                        selected.clear();
                        selected.push(variant);
                        highest_priority = entry.priority;
                    }
                    Ordering::Equal => selected.push(variant),
                    Ordering::Less => {}
                }
            }
        }
        let bound = i32::try_from(selected.len()).ok()?;
        if bound == 0 {
            return None;
        }
        selected
            .get(random.next_i32_bounded(bound) as usize)
            .copied()
    }
}

crate::impl_registry!(
    CatVariantRegistry,
    CatVariant,
    cat_variants_by_id,
    cat_variants_by_key,
    cat_variants
);

crate::impl_standard_methods!(
    CatVariantRegistry,
    CatVariantRef,
    cat_variants_by_id,
    cat_variants_by_key,
    allows_registering
);
