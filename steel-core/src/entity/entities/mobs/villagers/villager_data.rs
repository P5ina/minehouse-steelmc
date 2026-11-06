//! Vanilla `VillagerData` (type, profession, level) and `VillagerType.byBiome`,
//! shared by villagers and zombie villagers.

use std::str::FromStr;

use simdnbt::borrow::NbtCompound as BorrowedNbtCompoundView;
use simdnbt::owned::{NbtCompound, NbtTag};
use steel_registry::villager_profession::VillagerProfessionRef;
use steel_registry::villager_type::VillagerTypeRef;
use steel_registry::{
    REGISTRY, RegistryExt as _, vanilla_villager_professions, vanilla_villager_types,
};
use steel_utils::{BlockPos, Identifier};

use crate::world::World;

/// Vanilla `VillagerData` NBT key.
pub(crate) const TAG_VILLAGER_DATA: &str = "VillagerData";

/// Vanilla `VillagerData.MIN_VILLAGER_LEVEL`.
pub(crate) const MIN_VILLAGER_LEVEL: i32 = 1;

/// Vanilla `VillagerType.BY_BIOME`; every other biome gives plains villagers.
pub(crate) fn villager_type_by_biome(biome: &Identifier) -> VillagerTypeRef {
    if biome.namespace != "minecraft" {
        return &vanilla_villager_types::PLAINS;
    }
    match biome.path.as_ref() {
        "badlands" | "desert" | "eroded_badlands" | "wooded_badlands" => {
            &vanilla_villager_types::DESERT
        }
        "bamboo_jungle" | "jungle" | "sparse_jungle" => &vanilla_villager_types::JUNGLE,
        "savanna_plateau" | "savanna" | "windswept_savanna" => &vanilla_villager_types::SAVANNA,
        "deep_frozen_ocean" | "frozen_ocean" | "frozen_river" | "ice_spikes" | "snowy_beach"
        | "snowy_taiga" | "snowy_plains" | "grove" | "snowy_slopes" | "frozen_peaks"
        | "jagged_peaks" => &vanilla_villager_types::SNOW,
        "swamp" | "mangrove_swamp" => &vanilla_villager_types::SWAMP,
        "old_growth_spruce_taiga"
        | "old_growth_pine_taiga"
        | "windswept_gravelly_hills"
        | "windswept_hills"
        | "taiga"
        | "windswept_forest" => &vanilla_villager_types::TAIGA,
        _ => &vanilla_villager_types::PLAINS,
    }
}

/// Vanilla `VillagerType.byBiome` at `pos`; plains while the chunk is missing.
pub(crate) fn villager_type_at(world: &World, pos: BlockPos) -> VillagerTypeRef {
    world
        .biome_at(pos)
        .map_or(&vanilla_villager_types::PLAINS, |biome| {
            villager_type_by_biome(&biome.key)
        })
}

/// Vanilla `holderByNameCodec().fieldOf(key).orElseGet(default)`.
pub(crate) fn holder_by_name<T>(
    nbt: BorrowedNbtCompoundView<'_, '_>,
    key: &str,
    default: T,
    lookup: impl FnOnce(&Identifier) -> Option<T>,
) -> Option<T> {
    let Some(value) = nbt.string(key) else {
        return Some(default);
    };
    let id = Identifier::from_str(value.to_str().as_ref()).ok()?;
    lookup(&id)
}

/// Vanilla `VillagerData.CODEC` decode: a missing type or profession falls
/// back to the default, an unknown one fails the whole value.
pub(crate) fn read_villager_data(
    nbt: BorrowedNbtCompoundView<'_, '_>,
) -> Option<(VillagerTypeRef, VillagerProfessionRef, i32)> {
    let data = nbt.compound(TAG_VILLAGER_DATA)?;
    let villager_type = holder_by_name(data, "type", &vanilla_villager_types::PLAINS, |key| {
        REGISTRY.villager_types.by_key(key)
    })?;
    let profession = holder_by_name(
        data,
        "profession",
        &vanilla_villager_professions::NONE,
        |key| REGISTRY.villager_professions.by_key(key),
    )?;
    let level = data.int("level").unwrap_or(MIN_VILLAGER_LEVEL);
    Some((villager_type, profession, level))
}

/// Vanilla `VillagerData.CODEC` encode under [`TAG_VILLAGER_DATA`].
pub(crate) fn write_villager_data(
    nbt: &mut NbtCompound,
    villager_type: VillagerTypeRef,
    profession: VillagerProfessionRef,
    level: i32,
) {
    let mut data = NbtCompound::new();
    data.insert("type", villager_type.key.to_string());
    data.insert("profession", profession.key.to_string());
    data.insert("level", level);
    nbt.insert(TAG_VILLAGER_DATA, NbtTag::Compound(data));
}
