//! Conversion of vanilla pieces that are built in code rather than from a template: mineshafts,
//! nether fortresses, strongholds, ocean monuments, desert pyramids, jungle temples, swamp huts
//! and buried treasure.
//!
//! Each piece keeps the fields its vanilla `(CompoundTag)` constructor reads. Missing fields get
//! the same defaults as vanilla's `getIntOr` / `getBooleanOr`.

use glam::IVec3;
use simdnbt::owned::NbtCompound;
use steel_utils::{BoundingBox, Direction};
use steel_worldgen::structure::desert_pyramid::DesertPyramidPieceData;
use steel_worldgen::structure::fortress::FortressPieceData;
use steel_worldgen::structure::jungle_temple::JungleTemplePieceData;
use steel_worldgen::structure::mineshaft::{
    MineshaftPieceKind, MineshaftPiecePayload, MineshaftType,
};
use steel_worldgen::structure::ocean_monument::regenerate_monument_building;
use steel_worldgen::structure::stronghold::{StrongholdPieceData, StrongholdSmallDoorType};
use steel_worldgen::structure::swamp_hut::SwampHutPieceData;
use steel_worldgen::structure::{ProceduralPieceData, StructurePiecePayload};

use super::nbt;
use crate::chunk_saver::{ChunkStorage, PersistentBoundingBox, PersistentStructurePiecePayload};

/// NBT list type id of an empty list.
const END_TAG: u8 = 0;

/// What a piece needs from its structure start.
pub(super) struct StartContext {
    /// World seed, used to rebuild ocean monuments.
    pub seed: i64,
    /// `ChunkX` of the start.
    pub chunk_x: i32,
    /// `ChunkZ` of the start.
    pub chunk_z: i32,
}

/// A converted procedural piece.
pub(super) struct ProceduralPiece {
    pub payload: PersistentStructurePiecePayload,
    pub bounding_box: PersistentBoundingBox,
    pub orientation: i8,
}

/// Converts a procedural piece, or returns `None` when `piece_type` is not one.
pub(super) fn procedural_piece(
    piece_type: &str,
    piece: &NbtCompound,
    bounding_box: PersistentBoundingBox,
    orientation: i8,
    start: &StartContext,
) -> Option<Result<ProceduralPiece, String>> {
    if piece_type == "minecraft:omb" {
        return Some(Ok(ocean_monument(bounding_box, orientation, start)));
    }
    let data = match piece_type {
        "minecraft:btp" => Ok(ProceduralPieceData::BuriedTreasure),
        "minecraft:tedp" => Ok(desert_pyramid(piece)),
        "minecraft:tejp" => Ok(jungle_temple(piece)),
        "minecraft:tesh" => Ok(swamp_hut(piece)),
        "minecraft:msroom"
        | "minecraft:mscorridor"
        | "minecraft:mscrossing"
        | "minecraft:msstairs" => mineshaft(piece_type, piece),
        _ => {
            if let Some(data) = fortress(piece_type, piece) {
                Ok(ProceduralPieceData::NetherFortress(data))
            } else {
                stronghold(piece_type, piece)?.map(ProceduralPieceData::Stronghold)
            }
        }
    };
    Some(data.map(|data| ProceduralPiece {
        payload: ChunkStorage::structure_piece_payload_to_persistent(
            &StructurePiecePayload::Procedural(data),
        ),
        bounding_box,
        orientation,
    }))
}

/// Vanilla `OceanMonumentStructure.regeneratePiecesAfterLoad`: the saved `omb` piece has no
/// rooms, so the building is generated again from its saved corner and orientation.
fn ocean_monument(
    saved_box: PersistentBoundingBox,
    orientation: i8,
    start: &StartContext,
) -> ProceduralPiece {
    let piece = regenerate_monument_building(
        start.seed,
        start.chunk_x,
        start.chunk_z,
        saved_box.min_x,
        saved_box.min_z,
        direction_from_2d(orientation),
    );
    ProceduralPiece {
        payload: ChunkStorage::structure_piece_payload_to_persistent(&piece.payload),
        bounding_box: PersistentBoundingBox::from_bounding_box(piece.bounding_box),
        orientation: piece.orientation.map_or(-1, direction_to_2d),
    }
}

/// `ScatteredFeaturePiece` saves `HPos`; vanilla uses -1 until the piece found its ground.
fn height_position(piece: &NbtCompound) -> Option<i32> {
    let height = int_or(piece, "HPos", 0);
    (height >= 0).then_some(height)
}

/// Vanilla `DesertPyramidPiece(CompoundTag)`.
fn desert_pyramid(piece: &NbtCompound) -> ProceduralPieceData {
    ProceduralPieceData::DesertPyramid(DesertPyramidPieceData {
        height_position: height_position(piece),
        has_placed_chest: [0, 1, 2, 3].map(|i| nbt::boolean(piece, &format!("hasPlacedChest{i}"))),
        ..DesertPyramidPieceData::new()
    })
}

/// Vanilla `JungleTemplePiece(CompoundTag)`.
fn jungle_temple(piece: &NbtCompound) -> ProceduralPieceData {
    ProceduralPieceData::JungleTemple(JungleTemplePieceData {
        height_position: height_position(piece),
        placed_main_chest: nbt::boolean(piece, "placedMainChest"),
        placed_hidden_chest: nbt::boolean(piece, "placedHiddenChest"),
        placed_trap1: nbt::boolean(piece, "placedTrap1"),
        placed_trap2: nbt::boolean(piece, "placedTrap2"),
    })
}

/// Vanilla `SwampHutPiece(CompoundTag)`.
fn swamp_hut(piece: &NbtCompound) -> ProceduralPieceData {
    ProceduralPieceData::SwampHut(SwampHutPieceData {
        height_position: height_position(piece),
        spawned_witch: nbt::boolean(piece, "Witch"),
        spawned_cat: nbt::boolean(piece, "Cat"),
    })
}

/// Vanilla `MineshaftPieces.MineShaftPiece(type, CompoundTag)` and its subclasses.
fn mineshaft(piece_type: &str, piece: &NbtCompound) -> Result<ProceduralPieceData, String> {
    // `MineshaftStructure.Type.byId` clamps unknown ids to `NORMAL`.
    let mineshaft_type = match int_or(piece, "MST", 0) {
        1 => MineshaftType::Mesa,
        _ => MineshaftType::Normal,
    };
    let kind = match piece_type {
        "minecraft:msroom" => MineshaftPieceKind::Room {
            child_entrance_boxes: entrances(piece)?,
        },
        "minecraft:mscorridor" => MineshaftPieceKind::Corridor {
            has_rails: nbt::boolean(piece, "hr"),
            spider_corridor: nbt::boolean(piece, "sc"),
            has_placed_spider: nbt::boolean(piece, "hps"),
            num_sections: int_or(piece, "Num", 0),
        },
        "minecraft:mscrossing" => MineshaftPieceKind::Crossing {
            direction: crossing_direction(piece)?,
            is_two_floored: nbt::boolean(piece, "tf"),
        },
        _ => MineshaftPieceKind::Stairs,
    };
    Ok(ProceduralPieceData::Mineshaft(MineshaftPiecePayload {
        mineshaft_type,
        kind,
    }))
}

/// `MineShaftRoom` saves its child entrances as a list of `BoundingBox.CODEC` int arrays.
fn entrances(piece: &NbtCompound) -> Result<Vec<BoundingBox>, String> {
    let Some(list) = piece.list("Entrances") else {
        return Ok(Vec::new());
    };
    if list.id() == END_TAG {
        return Ok(Vec::new());
    }
    list.int_arrays()
        .ok_or("Entrances is not a list of bounding boxes")?
        .iter()
        .map(|values| match values.as_slice() {
            &[min_x, min_y, min_z, max_x, max_y, max_z]
                if min_x <= max_x && min_y <= max_y && min_z <= max_z =>
            {
                Ok(BoundingBox::new(
                    IVec3::new(min_x, min_y, min_z),
                    IVec3::new(max_x, max_y, max_z),
                ))
            }
            _ => Err("malformed mineshaft room entrance".to_owned()),
        })
        .collect()
}

/// `MineShaftCrossing` saves `D` with `Direction.LEGACY_ID_CODEC_2D`, defaulting to south.
fn crossing_direction(piece: &NbtCompound) -> Result<Direction, String> {
    let value = match (piece.byte("D"), piece.int("D")) {
        (Some(value), _) => i32::from(value),
        (None, Some(value)) => value,
        (None, None) => return Ok(Direction::South),
    };
    i8::try_from(value)
        .ok()
        .and_then(direction_from_2d)
        .ok_or_else(|| format!("invalid mineshaft crossing direction {value}"))
}

/// Vanilla `NetherFortressPieces` pieces. `NeStart` is a bridge crossing.
fn fortress(piece_type: &str, piece: &NbtCompound) -> Option<FortressPieceData> {
    Some(match piece_type {
        "minecraft:nestart" | "minecraft:nebcr" => FortressPieceData::BridgeCrossing,
        "minecraft:nebef" => FortressPieceData::BridgeEndFiller {
            self_seed: int_or(piece, "Seed", 0),
        },
        "minecraft:nebs" => FortressPieceData::BridgeStraight,
        "minecraft:neccs" => FortressPieceData::CastleCorridorStairs,
        "minecraft:nectb" => FortressPieceData::CastleCorridorTBalcony,
        "minecraft:nece" => FortressPieceData::CastleEntrance,
        "minecraft:nescsc" => FortressPieceData::CastleSmallCorridorCrossing,
        "minecraft:nesclt" => FortressPieceData::CastleSmallCorridorLeftTurn {
            is_needing_chest: nbt::boolean(piece, "Chest"),
        },
        "minecraft:nesc" => FortressPieceData::CastleSmallCorridor,
        "minecraft:nescrt" => FortressPieceData::CastleSmallCorridorRightTurn {
            is_needing_chest: nbt::boolean(piece, "Chest"),
        },
        "minecraft:necsr" => FortressPieceData::CastleStalkRoom,
        "minecraft:nemt" => FortressPieceData::MonsterThrone {
            has_placed_spawner: nbt::boolean(piece, "Mob"),
        },
        "minecraft:nerc" => FortressPieceData::RoomCrossing,
        "minecraft:nesr" => FortressPieceData::StairsRoom,
        _ => return None,
    })
}

/// Vanilla `StrongholdPieces` pieces, or `None` when `piece_type` is not a stronghold piece.
fn stronghold(
    piece_type: &str,
    piece: &NbtCompound,
) -> Option<Result<StrongholdPieceData, String>> {
    let entry_door = || entry_door(piece);
    let data = match piece_type {
        "minecraft:shstart" | "minecraft:shsd" => entry_door().and_then(|entry_door| {
            let is_source = nbt::boolean(piece, "Source");
            // Steel tells the start apart from other stairs by `is_source` alone.
            if is_source == (piece_type == "minecraft:shstart") {
                Ok(StrongholdPieceData::StairsDown {
                    entry_door,
                    is_source,
                })
            } else {
                Err(format!("{piece_type} with Source {is_source}"))
            }
        }),
        "minecraft:shs" => entry_door().map(|entry_door| StrongholdPieceData::Straight {
            entry_door,
            left_child: nbt::boolean(piece, "Left"),
            right_child: nbt::boolean(piece, "Right"),
        }),
        "minecraft:shph" => {
            entry_door().map(|entry_door| StrongholdPieceData::PrisonHall { entry_door })
        }
        "minecraft:shlt" => {
            entry_door().map(|entry_door| StrongholdPieceData::LeftTurn { entry_door })
        }
        "minecraft:shrt" => {
            entry_door().map(|entry_door| StrongholdPieceData::RightTurn { entry_door })
        }
        "minecraft:shrc" => entry_door().map(|entry_door| StrongholdPieceData::RoomCrossing {
            entry_door,
            crossing_type: int_or(piece, "Type", 0),
        }),
        "minecraft:shssd" => {
            entry_door().map(|entry_door| StrongholdPieceData::StraightStairsDown { entry_door })
        }
        "minecraft:sh5c" => entry_door().map(|entry_door| StrongholdPieceData::FiveCrossing {
            entry_door,
            left_low: nbt::boolean(piece, "leftLow"),
            left_high: nbt::boolean(piece, "leftHigh"),
            right_low: nbt::boolean(piece, "rightLow"),
            right_high: nbt::boolean(piece, "rightHigh"),
        }),
        "minecraft:shcc" => entry_door().map(|entry_door| StrongholdPieceData::ChestCorridor {
            entry_door,
            has_placed_chest: nbt::boolean(piece, "Chest"),
        }),
        "minecraft:shli" => entry_door().map(|entry_door| StrongholdPieceData::Library {
            entry_door,
            is_tall: nbt::boolean(piece, "Tall"),
        }),
        // The portal room and the filler corridor never draw their entry door.
        "minecraft:shpr" => Ok(StrongholdPieceData::PortalRoom {
            has_placed_spawner: nbt::boolean(piece, "Mob"),
        }),
        "minecraft:shfc" => Ok(StrongholdPieceData::FillerCorridor {
            steps: int_or(piece, "Steps", 0),
        }),
        _ => return None,
    };
    Some(data)
}

/// `StrongholdPiece` saves `EntryDoor` with `SmallDoorType.LEGACY_CODEC`, which reads the enum
/// name or its ordinal. Vanilla throws when it is missing.
fn entry_door(piece: &NbtCompound) -> Result<StrongholdSmallDoorType, String> {
    let door = match (nbt::string(piece, "EntryDoor"), piece.int("EntryDoor")) {
        (Some(name), _) => match name.as_str() {
            "OPENING" => Some(StrongholdSmallDoorType::Opening),
            "WOOD_DOOR" => Some(StrongholdSmallDoorType::WoodDoor),
            "GRATES" => Some(StrongholdSmallDoorType::Grates),
            "IRON_DOOR" => Some(StrongholdSmallDoorType::IronDoor),
            _ => None,
        },
        (None, Some(ordinal)) => match ordinal {
            0 => Some(StrongholdSmallDoorType::Opening),
            1 => Some(StrongholdSmallDoorType::WoodDoor),
            2 => Some(StrongholdSmallDoorType::Grates),
            3 => Some(StrongholdSmallDoorType::IronDoor),
            _ => None,
        },
        (None, None) => return Err("stronghold piece without EntryDoor".to_owned()),
    };
    door.ok_or_else(|| "invalid stronghold EntryDoor".to_owned())
}

/// Vanilla `Direction.get2DDataValue` order.
const fn direction_from_2d(value: i8) -> Option<Direction> {
    match value {
        0 => Some(Direction::South),
        1 => Some(Direction::West),
        2 => Some(Direction::North),
        3 => Some(Direction::East),
        _ => None,
    }
}

const fn direction_to_2d(direction: Direction) -> i8 {
    match direction {
        Direction::South => 0,
        Direction::West => 1,
        Direction::North => 2,
        Direction::East => 3,
        Direction::Down | Direction::Up => -1,
    }
}

fn int_or(compound: &NbtCompound, key: &str, default: i32) -> i32 {
    compound.int(key).unwrap_or(default)
}
