//! Template-backed structure pieces: vanilla `TemplateStructurePiece` subclasses.
//!
//! Each family is a port of its vanilla load constructor (`XPiece(..., CompoundTag)`), which
//! rebuilds the `StructurePlaceSettings` from family constants and a few saved fields. The
//! constants match the ones Steel's generator uses for the same family.

use std::io::Read;

use flate2::read::GzDecoder;
use glam::IVec3;
use simdnbt::owned::{NbtCompound, NbtList};
use steel_registry::structure::{
    LiquidSettingsData, OceanRuinBiomeTempData, RuinedPortalPlacementData,
};
use steel_registry::vanilla_template_pools::vanilla_template_nbt_bytes;
use steel_utils::{Identifier, Rotation};
use steel_worldgen::structure::{
    RuinedPortalProperties, StructureBlockIgnore, StructureMirror, StructurePiecePayload,
    TemplateMarkerHandling, TemplatePieceData, TemplatePlacementAdjustment, TemplatePlacementClip,
    TemplatePostProcess, TemplateProcessorList,
};

use super::nbt;
use crate::chunk_saver::{ChunkStorage, PersistentBoundingBox, PersistentStructurePiecePayload};

/// Vanilla `ShipwreckPieces.PIVOT`.
const SHIPWRECK_PIVOT: IVec3 = IVec3::new(4, 0, 15);

/// Converts the payload of a template piece of vanilla type `piece_type` and returns it with the
/// piece's bounding box, or returns `None` when the type is not a template piece.
///
/// The bounding box is computed again from the template, as `TemplateStructurePiece`'s load
/// constructor does: the saved one can be moved by a placement adjustment (igloos move theirs
/// to the terrain height while placing), and vanilla does not keep that after a reload.
pub(super) fn template_payload(
    piece_type: &str,
    piece: &NbtCompound,
) -> Option<Result<(PersistentStructurePiecePayload, PersistentBoundingBox), String>> {
    let data = match piece_type {
        "minecraft:shipwreck" => shipwreck(piece),
        "minecraft:iglu" => igloo(piece),
        "minecraft:orp" => ocean_ruin(piece),
        "minecraft:rupo" => ruined_portal(piece),
        "minecraft:nefos" => nether_fossil(piece),
        "minecraft:ecp" => end_city(piece),
        "minecraft:wmp" => woodland_mansion(piece),
        _ => return None,
    };
    Some(data.and_then(|data| {
        let bounding_box = template_bounding_box(&data)?;
        let payload = ChunkStorage::structure_piece_payload_to_persistent(
            &StructurePiecePayload::Template(data),
        );
        Ok((payload, bounding_box))
    }))
}

/// `StructureTemplate.getBoundingBox(StructurePlaceSettings, BlockPos)`: the template's corners
/// transformed by `StructureTemplate.transform` (mirror, then rotation around the pivot).
fn template_bounding_box(data: &TemplatePieceData) -> Result<PersistentBoundingBox, String> {
    let [size_x, size_y, size_z] = template_size(&data.template_id)?;
    let transform = |pos: IVec3| {
        let mirrored = match data.mirror {
            StructureMirror::None => pos,
            StructureMirror::LeftRight => IVec3::new(pos.x, pos.y, -pos.z),
            StructureMirror::FrontBack => IVec3::new(-pos.x, pos.y, pos.z),
        };
        data.rotation.transform_pos(mirrored, data.rotation_pivot)
    };
    let first = transform(IVec3::ZERO);
    let second = transform(IVec3::new(size_x - 1, size_y - 1, size_z - 1));
    let min = first.min(second) + data.template_position;
    let max = first.max(second) + data.template_position;
    Ok(PersistentBoundingBox {
        min_x: min.x,
        min_y: min.y,
        min_z: min.z,
        max_x: max.x,
        max_y: max.y,
        max_z: max.z,
    })
}

/// Settings shared by every family, from `StructurePlaceSettings`' defaults.
struct Base {
    template_id: Identifier,
    template_position: IVec3,
    rotation: Rotation,
}

impl Base {
    /// Reads `TemplateStructurePiece(type, tag, ...)`: `Template` and `TPX`/`TPY`/`TPZ`.
    /// `prefix` is the folder vanilla's `makeTemplateLocation` puts before a short name.
    fn read(piece: &NbtCompound, prefix: Option<&str>, rotation_key: &str) -> Result<Self, String> {
        let name = nbt::string(piece, "Template").ok_or("template piece without a template")?;
        let template_id = match prefix {
            Some(prefix) => Identifier::vanilla(format!("{prefix}/{name}")),
            None => nbt::identifier(&name).ok_or("malformed template id")?,
        };
        if vanilla_template_nbt_bytes(&template_id).is_none() {
            return Err(format!("structure template {template_id} is not bundled"));
        }
        Ok(Self {
            template_id,
            template_position: IVec3::new(
                int(piece, "TPX")?,
                int(piece, "TPY")?,
                int(piece, "TPZ")?,
            ),
            rotation: rotation(piece, rotation_key)?,
        })
    }

    fn into_data(
        self,
        mirror: StructureMirror,
        rotation_pivot: IVec3,
        block_ignore: StructureBlockIgnore,
        family: Family,
    ) -> TemplatePieceData {
        TemplatePieceData {
            template_id: self.template_id,
            template_position: self.template_position,
            rotation: self.rotation,
            mirror,
            rotation_pivot,
            block_ignore,
            late_block_ignore: family.late_block_ignore,
            processors: family.processors,
            liquid_settings: family.liquid_settings,
            marker_handling: family.marker_handling,
            placement_adjustment: family.placement_adjustment,
            placement_clip: family.placement_clip,
            post_process: family.post_process,
        }
    }
}

/// Family behavior that is not part of `StructurePlaceSettings`' rotation and mirror.
struct Family {
    late_block_ignore: StructureBlockIgnore,
    processors: TemplateProcessorList,
    liquid_settings: LiquidSettingsData,
    marker_handling: TemplateMarkerHandling,
    placement_adjustment: TemplatePlacementAdjustment,
    placement_clip: TemplatePlacementClip,
    post_process: TemplatePostProcess,
}

impl Family {
    const fn new(marker_handling: TemplateMarkerHandling) -> Self {
        Self {
            late_block_ignore: StructureBlockIgnore::None,
            processors: TemplateProcessorList::Empty,
            liquid_settings: LiquidSettingsData::ApplyWaterlogging,
            marker_handling,
            placement_adjustment: TemplatePlacementAdjustment::None,
            placement_clip: TemplatePlacementClip::CenterChunk,
            post_process: TemplatePostProcess::None,
        }
    }
}

/// `ShipwreckPieces.ShipwreckPiece(StructureTemplateManager, CompoundTag)`.
fn shipwreck(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, None, "Rot")?;
    let family = Family {
        placement_adjustment: TemplatePlacementAdjustment::Shipwreck {
            is_beached: nbt::boolean(piece, "isBeached"),
            height_adjusted: nbt::boolean(piece, "height_adjusted"),
        },
        ..Family::new(TemplateMarkerHandling::Shipwreck)
    };
    Ok(base.into_data(
        StructureMirror::None,
        SHIPWRECK_PIVOT,
        StructureBlockIgnore::StructureAndAir,
        family,
    ))
}

/// `IglooPieces.IglooPiece(StructureTemplateManager, CompoundTag)` with `IglooPieces.PIVOTS` and
/// `IglooPieces.OFFSETS`.
fn igloo(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, None, "Rot")?;
    let (pivot, offset, post_process) = match base.template_id.path.as_ref() {
        "igloo/top" => (
            IVec3::new(3, 5, 5),
            (0, 0, 0),
            TemplatePostProcess::IglooTop,
        ),
        "igloo/middle" => (IVec3::new(1, 3, 1), (2, -3, 4), TemplatePostProcess::None),
        "igloo/bottom" => (IVec3::new(3, 6, 7), (0, -3, -2), TemplatePostProcess::None),
        _ => return Err(format!("{} is not an igloo template", base.template_id)),
    };
    let family = Family {
        liquid_settings: LiquidSettingsData::IgnoreWaterlogging,
        placement_adjustment: TemplatePlacementAdjustment::Igloo {
            template_offset: offset,
        },
        post_process,
        ..Family::new(TemplateMarkerHandling::Igloo)
    };
    Ok(base.into_data(
        StructureMirror::None,
        pivot,
        StructureBlockIgnore::StructureBlock,
        family,
    ))
}

/// `OceanRuinPieces.OceanRuinPiece.create(StructureTemplateManager, CompoundTag)`.
fn ocean_ruin(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, None, "Rot")?;
    let biome_temp = match nbt::string(piece, "BiomeType").as_deref() {
        Some("WARM") => OceanRuinBiomeTempData::Warm,
        Some("COLD") => OceanRuinBiomeTempData::Cold,
        other => return Err(format!("invalid ocean ruin biome type {other:?}")),
    };
    let family = Family {
        late_block_ignore: StructureBlockIgnore::StructureAndAir,
        processors: TemplateProcessorList::OceanRuin {
            biome_temp,
            integrity: piece.float("Integrity").unwrap_or(0.0),
        },
        placement_adjustment: TemplatePlacementAdjustment::OceanRuin,
        ..Family::new(TemplateMarkerHandling::OceanRuin {
            is_large: nbt::boolean(piece, "IsLarge"),
        })
    };
    Ok(base.into_data(
        StructureMirror::None,
        IVec3::ZERO,
        StructureBlockIgnore::None,
        family,
    ))
}

/// `RuinedPortalPiece(StructurePieceSerializationContext, CompoundTag)`.
fn ruined_portal(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, None, "Rotation")?;
    let mirror = mirror(piece, "Mirror")?;
    let vertical_placement = match nbt::string(piece, "VerticalPlacement").as_deref() {
        Some("on_land_surface") => RuinedPortalPlacementData::OnLandSurface,
        Some("partly_buried") => RuinedPortalPlacementData::PartlyBuried,
        Some("on_ocean_floor") => RuinedPortalPlacementData::OnOceanFloor,
        Some("in_mountain") => RuinedPortalPlacementData::InMountain,
        Some("underground") => RuinedPortalPlacementData::Underground,
        Some("in_nether") => RuinedPortalPlacementData::InNether,
        other => return Err(format!("invalid ruined portal placement {other:?}")),
    };
    let properties = piece
        .compound("Properties")
        .ok_or("ruined portal without properties")?;
    let flag = |key: &str| {
        properties
            .byte(key)
            .map(|value| value != 0)
            .ok_or_else(|| format!("ruined portal properties without {key}"))
    };
    let properties = RuinedPortalProperties {
        cold: flag("cold")?,
        mossiness: properties
            .float("mossiness")
            .ok_or("ruined portal properties without mossiness")?,
        air_pocket: flag("air_pocket")?,
        overgrown: flag("overgrown")?,
        vines: flag("vines")?,
        replace_with_blackstone: flag("replace_with_blackstone")?,
    };
    // The pivot is the template's horizontal center, as in `makeSettings(..., tag, location)`.
    let [size_x, _, size_z] = template_size(&base.template_id)?;
    let block_ignore = if properties.air_pocket {
        StructureBlockIgnore::StructureBlock
    } else {
        StructureBlockIgnore::StructureAndAir
    };
    let family = Family {
        processors: TemplateProcessorList::RuinedPortal {
            vertical_placement,
            properties,
        },
        placement_clip: TemplatePlacementClip::CenterChunkContainsTemplateCenterExpandedToTemplate,
        post_process: TemplatePostProcess::RuinedPortal,
        ..Family::new(TemplateMarkerHandling::Ignore)
    };
    Ok(base.into_data(
        mirror,
        IVec3::new(size_x / 2, 0, size_z / 2),
        block_ignore,
        family,
    ))
}

/// `NetherFossilPieces.NetherFossilPiece(StructureTemplateManager, CompoundTag)`.
fn nether_fossil(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, None, "Rot")?;
    let family = Family {
        placement_clip: TemplatePlacementClip::CenterChunkExpandedToTemplate,
        post_process: TemplatePostProcess::NetherFossil,
        ..Family::new(TemplateMarkerHandling::Ignore)
    };
    Ok(base.into_data(
        StructureMirror::None,
        IVec3::ZERO,
        StructureBlockIgnore::StructureAndAir,
        family,
    ))
}

/// `EndCityPieces.EndCityPiece(StructureTemplateManager, CompoundTag)`.
fn end_city(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, Some("end_city"), "Rot")?;
    let block_ignore = if nbt::boolean(piece, "OW") {
        StructureBlockIgnore::StructureBlock
    } else {
        StructureBlockIgnore::StructureAndAir
    };
    Ok(base.into_data(
        StructureMirror::None,
        IVec3::ZERO,
        block_ignore,
        Family::new(TemplateMarkerHandling::EndCity),
    ))
}

/// `WoodlandMansionPieces.WoodlandMansionPiece(StructureTemplateManager, CompoundTag)`.
fn woodland_mansion(piece: &NbtCompound) -> Result<TemplatePieceData, String> {
    let base = Base::read(piece, Some("woodland_mansion"), "Rot")?;
    let mirror = mirror(piece, "Mi")?;
    Ok(base.into_data(
        mirror,
        IVec3::ZERO,
        StructureBlockIgnore::StructureBlock,
        Family::new(TemplateMarkerHandling::WoodlandMansion),
    ))
}

/// The `size` of a bundled template, `StructureTemplate.getSize`.
fn template_size(template_id: &Identifier) -> Result<[i32; 3], String> {
    let compressed = vanilla_template_nbt_bytes(template_id)
        .ok_or_else(|| format!("structure template {template_id} is not bundled"))?;
    let mut bytes = Vec::new();
    GzDecoder::new(compressed)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("structure template {template_id}: {error}"))?;
    let root = nbt::read_root(&bytes)?;
    root.list("size")
        .and_then(NbtList::ints)
        .and_then(|size| size.try_into().ok())
        .ok_or_else(|| format!("structure template {template_id} has no size"))
}

/// `Rotation.LEGACY_CODEC`.
pub(super) fn rotation(piece: &NbtCompound, key: &str) -> Result<Rotation, String> {
    match nbt::string(piece, key).as_deref() {
        Some("NONE") => Ok(Rotation::None),
        Some("CLOCKWISE_90") => Ok(Rotation::Clockwise90),
        Some("CLOCKWISE_180") => Ok(Rotation::Clockwise180),
        Some("COUNTERCLOCKWISE_90") => Ok(Rotation::CounterClockwise90),
        other => Err(format!("invalid rotation {other:?}")),
    }
}

/// `Mirror.LEGACY_CODEC`.
fn mirror(piece: &NbtCompound, key: &str) -> Result<StructureMirror, String> {
    match nbt::string(piece, key).as_deref() {
        Some("NONE") => Ok(StructureMirror::None),
        Some("LEFT_RIGHT") => Ok(StructureMirror::LeftRight),
        Some("FRONT_BACK") => Ok(StructureMirror::FrontBack),
        other => Err(format!("invalid mirror {other:?}")),
    }
}

fn int(compound: &NbtCompound, key: &str) -> Result<i32, String> {
    compound.int(key).ok_or_else(|| format!("missing {key}"))
}
