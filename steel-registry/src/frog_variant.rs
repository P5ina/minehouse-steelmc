use steel_utils::random::Random;

use crate::biome::BiomeRef;
use crate::shared_structs::{
    SpawnConditionEntry, insert_spawn_conditions, pick_spawn_conditioned_entry,
};
use rustc_hash::FxHashMap;
use simdnbt::ToNbtTag;
use simdnbt::owned::NbtTag;
use steel_utils::Identifier;

/// Represents a full frog variant definition from a data pack JSON file.
#[derive(Debug)]
pub struct FrogVariant {
    pub key: Identifier,
    pub asset_id: Identifier,
    pub spawn_conditions: &'static [SpawnConditionEntry],
}

impl ToNbtTag for &FrogVariant {
    fn to_nbt_tag(self) -> NbtTag {
        use simdnbt::owned::NbtCompound;
        let mut compound = NbtCompound::new();
        let asset_id = self.asset_id.to_string();
        compound.insert("asset_id", asset_id.as_str());
        compound.insert("baby_asset_id", asset_id.as_str());
        insert_spawn_conditions(&mut compound, self.spawn_conditions);
        NbtTag::Compound(compound)
    }
}

pub type FrogVariantRef = &'static FrogVariant;

pub struct FrogVariantRegistry {
    frog_variants_by_id: Vec<FrogVariantRef>,
    frog_variants_by_key: FxHashMap<Identifier, usize>,
    allows_registering: bool,
}

impl FrogVariantRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frog_variants_by_id: Vec::new(),
            frog_variants_by_key: FxHashMap::default(),
            allows_registering: true,
        }
    }

    /// Vanilla `VariantUtils.selectVariantToSpawn` for frogs: the matching
    /// variant with the highest spawn condition priority.
    #[must_use]
    pub fn select_spawn_variant(
        &self,
        biome: BiomeRef,
        random: &mut impl Random,
    ) -> Option<FrogVariantRef> {
        pick_spawn_conditioned_entry(
            self.iter().map(|(_, variant)| variant),
            |variant| variant.spawn_conditions,
            biome,
            random,
        )
    }
}

crate::impl_standard_methods!(
    FrogVariantRegistry,
    FrogVariantRef,
    frog_variants_by_id,
    frog_variants_by_key,
    allows_registering
);

crate::impl_registry!(
    FrogVariantRegistry,
    FrogVariant,
    frog_variants_by_id,
    frog_variants_by_key,
    frog_variants
);

#[cfg(test)]
mod tests {
    use steel_utils::random::legacy_random::LegacyRandom;

    use crate::{REGISTRY, init_vanilla_registry, vanilla_biomes, vanilla_frog_variants};

    #[test]
    fn select_spawn_variant_follows_the_biome() {
        init_vanilla_registry();

        let pick = |biome| {
            let mut random = LegacyRandom::from_seed(0);
            REGISTRY
                .frog_variants
                .select_spawn_variant(biome, &mut random)
                .map(|variant| &variant.key)
        };
        assert_eq!(
            pick(&vanilla_biomes::SWAMP),
            Some(&vanilla_frog_variants::TEMPERATE.key)
        );
        assert_eq!(
            pick(&vanilla_biomes::MANGROVE_SWAMP),
            Some(&vanilla_frog_variants::WARM.key)
        );
        assert_eq!(
            pick(&vanilla_biomes::SNOWY_PLAINS),
            Some(&vanilla_frog_variants::COLD.key)
        );
    }
}
