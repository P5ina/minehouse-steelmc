//! Conversion of vanilla structure starts (`StructureStart.createTag`) into Steel's
//! `PersistentStructureStart`.
//!
//! A start is converted only when every one of its pieces is. A start with a piece Steel cannot
//! represent, or with data that does not parse, is dropped whole and counted by structure id:
//! a start with missing pieces would generate a different structure than vanilla did.

use simdnbt::owned::{NbtCompound, NbtList};
use steel_registry::vanilla_template_pools::vanilla_template_nbt_bytes;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::Identifier;

use super::nbt;
use super::report::ImportReport;
use crate::chunk_saver::{
    PersistentBoundingBox, PersistentJigsawJunction, PersistentJigsawPieceData,
    PersistentPoolElement, PersistentProcessorList, PersistentStructurePiece,
    PersistentStructurePiecePayload, PersistentStructureStart,
};

/// Vanilla `StructureStart.INVALID_START_ID`.
const INVALID_START: &str = "INVALID";
/// Vanilla `StructurePieceType.JIGSAW`.
const JIGSAW_PIECE: &str = "minecraft:jigsaw";
/// NBT list type id of an empty list.
const END_TAG: u8 = 0;

/// Converts the structure starts of a chunk's `structures` compound.
pub(super) fn structure_starts(
    structures: &NbtCompound,
    report: &mut ImportReport,
) -> Vec<PersistentStructureStart> {
    let Some(starts) = structures.compound("starts") else {
        return Vec::new();
    };
    let mut converted = Vec::new();
    for (structure, start) in starts.iter() {
        let Some(start) = start.compound() else {
            continue;
        };
        if nbt::string(start, "id").is_none_or(|id| id == INVALID_START) {
            continue;
        }
        let structure = structure.to_str();
        match structure_start(&structure, start) {
            Ok(start) => converted.push(start),
            Err(_) => {
                ImportReport::count(&mut report.structure_starts_dropped, structure.into_owned());
            }
        }
    }
    converted
}

/// Converts one start, vanilla `StructureStart.loadStaticStart`.
fn structure_start(
    structure: &str,
    start: &NbtCompound,
) -> Result<PersistentStructureStart, String> {
    let structure = nbt::identifier(structure).ok_or("malformed structure id")?;
    if REGISTRY.structures.by_key(&structure).is_none() {
        return Err(format!("unknown structure {structure}"));
    }
    let pieces = compound_list(start, "Children")?
        .iter()
        .map(structure_piece)
        .collect::<Result<Vec<_>, _>>()?;
    if pieces.is_empty() {
        return Err("structure start has no pieces".to_owned());
    }
    Ok(PersistentStructureStart {
        structure,
        chunk_x: int(start, "ChunkX")?,
        chunk_z: int(start, "ChunkZ")?,
        references: int(start, "references")?,
        pieces,
    })
}

/// Converts one piece. The common fields are vanilla `StructurePiece(type, tag)`.
fn structure_piece(piece: &NbtCompound) -> Result<PersistentStructurePiece, String> {
    let piece_type = nbt::string(piece, "id").ok_or("piece without id")?;
    let bounding_box = bounding_box(piece)?;
    let gen_depth = int(piece, "GD")?;
    let orientation = int(piece, "O")?;
    if !(-1..=3).contains(&orientation) {
        return Err(format!("invalid piece orientation {orientation}"));
    }
    let orientation = orientation as i8;
    match piece_type.as_str() {
        JIGSAW_PIECE => jigsaw_piece(piece, bounding_box, gen_depth, orientation),
        other => Err(format!("unsupported structure piece {other}")),
    }
}

/// Vanilla `PoolElementStructurePiece(StructurePieceSerializationContext, CompoundTag)`.
fn jigsaw_piece(
    piece: &NbtCompound,
    bounding_box: PersistentBoundingBox,
    gen_depth: i32,
    orientation: i8,
) -> Result<PersistentStructurePiece, String> {
    let element = piece
        .compound("pool_element")
        .ok_or("jigsaw piece without pool_element")?;
    let (pool_element, projection) = pool_element(element)?;
    let rotation = match nbt::string(piece, "rotation").as_deref() {
        Some("NONE") => 0,
        Some("CLOCKWISE_90") => 1,
        Some("CLOCKWISE_180") => 2,
        Some("COUNTERCLOCKWISE_90") => 3,
        other => return Err(format!("invalid jigsaw rotation {other:?}")),
    };
    // `LiquidSettings.CODEC` falls back to `APPLY_WATERLOGGING` when the field is absent.
    let liquid_settings = match nbt::string(piece, "liquid_settings").as_deref() {
        None | Some("apply_waterlogging") => 0,
        Some("ignore_waterlogging") => 1,
        Some(other) => return Err(format!("invalid liquid settings {other}")),
    };
    let junctions = compound_list(piece, "junctions")?
        .iter()
        .map(junction)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PersistentStructurePiece {
        piece_type: Identifier::vanilla_static("jigsaw"),
        bounding_box,
        gen_depth,
        orientation,
        payload: PersistentStructurePiecePayload::Jigsaw(PersistentJigsawPieceData {
            pool_element,
            position: [
                int(piece, "PosX")?,
                int(piece, "PosY")?,
                int(piece, "PosZ")?,
            ],
            rotation,
            liquid_settings,
        }),
        ground_level_delta: int(piece, "ground_level_delta")?,
        projection,
        junctions,
    })
}

/// Converts a pool element encoded with `StructurePoolElement.CODEC`, returning it with its
/// projection (`StructurePoolElement.getProjection`).
///
/// Elements are checked against what Steel can place: a template that is not bundled, or a
/// processor list or placed feature Steel does not know, would fail when the piece is placed.
fn pool_element(element: &NbtCompound) -> Result<(PersistentPoolElement, i8), String> {
    let element_type = nbt::string(element, "element_type").ok_or("pool element without type")?;
    if element_type == "minecraft:empty_pool_element" {
        return Ok((PersistentPoolElement::Empty, 0));
    }
    let projection = match nbt::string(element, "projection").as_deref() {
        Some("rigid") => 0,
        Some("terrain_matching") => 1,
        other => return Err(format!("invalid pool element projection {other:?}")),
    };
    let converted = match element_type.as_str() {
        "minecraft:single_pool_element" => {
            let (location, processors) = single_element(element)?;
            PersistentPoolElement::Single {
                location,
                processors,
                projection,
            }
        }
        "minecraft:legacy_single_pool_element" => {
            let (location, processors) = single_element(element)?;
            PersistentPoolElement::LegacySingle {
                location,
                processors,
                projection,
            }
        }
        "minecraft:feature_pool_element" => {
            let feature = nbt::string(element, "feature")
                .and_then(|feature| nbt::identifier(&feature))
                .ok_or("feature pool element without a feature id")?;
            if REGISTRY.placed_features.by_key(&feature).is_none() {
                return Err(format!("unknown placed feature {feature}"));
            }
            PersistentPoolElement::Feature {
                feature,
                projection,
            }
        }
        "minecraft:list_pool_element" => {
            let elements = compound_list(element, "elements")?
                .iter()
                .map(|element| pool_element(element).map(|(element, _)| element))
                .collect::<Result<Vec<_>, _>>()?;
            PersistentPoolElement::List {
                elements,
                projection,
            }
        }
        other => return Err(format!("unsupported pool element {other}")),
    };
    Ok((converted, projection))
}

/// Template location and processors of a (legacy) single pool element.
fn single_element(element: &NbtCompound) -> Result<(Identifier, PersistentProcessorList), String> {
    let location = nbt::string(element, "location")
        .and_then(|location| nbt::identifier(&location))
        .ok_or("single pool element without a template id")?;
    if vanilla_template_nbt_bytes(&location).is_none() {
        return Err(format!("structure template {location} is not bundled"));
    }
    // `StructureProcessorType.LIST_CODEC` writes a registered list by id and a direct list as
    // `{processors: [...]}`. Steel only represents the empty direct list.
    let processors = if let Some(id) = nbt::string(element, "processors") {
        let id = nbt::identifier(&id).ok_or("malformed processor list id")?;
        if REGISTRY.structure_processors.by_key(&id).is_none() {
            return Err(format!("unknown processor list {id}"));
        }
        PersistentProcessorList::Registry(id)
    } else {
        let direct = element
            .compound("processors")
            .ok_or("single pool element without processors")?;
        if !compound_list(direct, "processors")?.is_empty() {
            return Err("direct processor lists are not supported".to_owned());
        }
        PersistentProcessorList::Empty
    };
    Ok((location, processors))
}

/// Vanilla `JigsawJunction.deserialize`.
fn junction(junction: &NbtCompound) -> Result<PersistentJigsawJunction, String> {
    let dest_projection = match nbt::string(junction, "dest_proj").as_deref() {
        Some("rigid") => 0,
        Some("terrain_matching") => 1,
        other => return Err(format!("invalid junction projection {other:?}")),
    };
    Ok(PersistentJigsawJunction {
        source_x: int(junction, "source_x")?,
        source_ground_y: int(junction, "source_ground_y")?,
        source_z: int(junction, "source_z")?,
        delta_y: int(junction, "delta_y")?,
        dest_projection,
    })
}

/// Vanilla `BoundingBox.CODEC`: `[minX, minY, minZ, maxX, maxY, maxZ]`.
fn bounding_box(piece: &NbtCompound) -> Result<PersistentBoundingBox, String> {
    let Some(&[min_x, min_y, min_z, max_x, max_y, max_z]) = piece.int_array("BB") else {
        return Err("piece without a bounding box".to_owned());
    };
    if min_x > max_x || min_y > max_y || min_z > max_z {
        return Err("inverted piece bounding box".to_owned());
    }
    Ok(PersistentBoundingBox {
        min_x,
        min_y,
        min_z,
        max_x,
        max_y,
        max_z,
    })
}

fn int(compound: &NbtCompound, key: &str) -> Result<i32, String> {
    compound.int(key).ok_or_else(|| format!("missing {key}"))
}

/// Reads a list of compounds, where an empty list may have the end tag as its element type.
fn compound_list<'a>(compound: &'a NbtCompound, key: &str) -> Result<&'a [NbtCompound], String> {
    let list: &NbtList = compound.list(key).ok_or_else(|| format!("missing {key}"))?;
    if list.id() == END_TAG {
        return Ok(&[]);
    }
    list.compounds()
        .ok_or_else(|| format!("{key} is not a list of compounds"))
}
