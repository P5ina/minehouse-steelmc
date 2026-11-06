//! Conversion of vanilla entity chunks (`entities/r.<x>.<z>.mca`).

use simdnbt::owned::NbtCompound;
use steel_registry::{REGISTRY, RegistryExt};
use steel_utils::ChunkPos;

use super::chunk::check_data_version;
use super::nbt;
use super::report::ImportReport;
use crate::chunk_saver::PersistentEntity;

/// Keys written by vanilla `Entity.saveWithoutId` and `saveAsPassenger`. Steel stores them as
/// typed fields, so they are removed from the type-specific payload.
const ENTITY_BASE_KEYS: [&str; 21] = [
    "id",
    "Pos",
    "Motion",
    "Rotation",
    "fall_distance",
    "Fire",
    "Air",
    "OnGround",
    "Invulnerable",
    "PortalCooldown",
    "UUID",
    "CustomName",
    "CustomNameVisible",
    "Silent",
    "NoGravity",
    "Glowing",
    "TicksFrozen",
    "HasVisualFire",
    "Tags",
    "data",
    "Passengers",
];

/// Converts an entity chunk into Steel entities.
pub fn convert_entity_chunk(
    bytes: &[u8],
    pos: ChunkPos,
    report: &mut ImportReport,
) -> Result<Vec<PersistentEntity>, String> {
    let root = nbt::read_root(bytes)?;
    check_data_version(&root).map_err(|error| format!("entity chunk {error}"))?;
    if let Some(position) = root.int_array("Position")
        && position != [pos.0.x, pos.0.y]
    {
        return Err(format!(
            "entity chunk says it is at {position:?}, but it is stored at [{}, {}]",
            pos.0.x, pos.0.y
        ));
    }
    let entities = root.list("Entities").and_then(|list| list.compounds());
    Ok(entities
        .unwrap_or_default()
        .iter()
        .filter_map(|entity| convert_entity(entity, report))
        .collect())
}

/// Converts one entity and its passengers. Returns `None` for an entity Steel cannot load;
/// its passengers are dropped with it, as vanilla drops them with an unloadable vehicle.
fn convert_entity(entity: &NbtCompound, report: &mut ImportReport) -> Option<PersistentEntity> {
    let id = nbt::string(entity, "id").unwrap_or_default();
    let Some(entity_type) =
        nbt::identifier(&id).filter(|key| REGISTRY.entity_types.by_key(key).is_some())
    else {
        count_unknown(entity, report);
        return None;
    };

    let (Some(pos), Some(uuid)) = (nbt::doubles::<3>(entity, "Pos"), uuid(entity)) else {
        ImportReport::count(
            &mut report.dropped_records,
            "entity without position or UUID",
        );
        return None;
    };

    let custom_name_nbt = entity.get("CustomName").map_or_default(|name| {
        let mut root = NbtCompound::new();
        root.insert("CustomName", name.clone());
        nbt::compound_bytes(&root)
    });
    let tags = entity
        .list("Tags")
        .and_then(|tags| tags.strings().map(<[_]>::to_vec))
        .unwrap_or_default()
        .iter()
        .map(|tag| tag.to_str().into_owned())
        .collect();
    let custom_data_nbt = entity.compound("data").map_or_default(nbt::compound_bytes);

    let passengers = entity
        .list("Passengers")
        .and_then(|list| list.compounds())
        .unwrap_or_default()
        .iter()
        .filter_map(|passenger| convert_entity(passenger, report))
        .collect();

    report.entities += 1;
    Some(PersistentEntity {
        entity_type,
        uuid,
        pos,
        motion: nbt::doubles::<3>(entity, "Motion").unwrap_or_default(),
        rotation: nbt::floats::<2>(entity, "Rotation").unwrap_or_default(),
        fall_distance: entity.double("fall_distance").unwrap_or_default(),
        remaining_fire_ticks: i32::from(entity.short("Fire").unwrap_or_default()),
        ticks_frozen: entity.int("TicksFrozen").unwrap_or_default(),
        // Powder snow contact is recomputed every tick and is not saved by vanilla.
        is_in_powder_snow: false,
        was_in_powder_snow: false,
        has_visual_fire: nbt::boolean(entity, "HasVisualFire"),
        on_ground: nbt::boolean(entity, "OnGround"),
        no_gravity: nbt::boolean(entity, "NoGravity"),
        invulnerable: nbt::boolean(entity, "Invulnerable"),
        air_supply: i32::from(entity.short("Air").unwrap_or(300)),
        portal_cooldown: entity.int("PortalCooldown").unwrap_or_default(),
        custom_name_nbt,
        custom_name_visible: nbt::boolean(entity, "CustomNameVisible"),
        silent: nbt::boolean(entity, "Silent"),
        glowing: nbt::boolean(entity, "Glowing"),
        tags,
        custom_data_nbt,
        nbt_data: nbt::compound_bytes(&nbt::without_keys(entity, &ENTITY_BASE_KEYS)),
        passengers,
    })
}

fn count_unknown(entity: &NbtCompound, report: &mut ImportReport) {
    let id = nbt::string(entity, "id").unwrap_or_else(|| "<missing id>".to_owned());
    ImportReport::count(&mut report.unknown_entities, id);
    for passenger in entity
        .list("Passengers")
        .and_then(|list| list.compounds())
        .unwrap_or_default()
    {
        count_unknown(passenger, report);
    }
}

/// Reads vanilla's UUID int array (most significant int first).
fn uuid(entity: &NbtCompound) -> Option<[u8; 16]> {
    let ints: [i32; 4] = entity.int_array("UUID")?.try_into().ok()?;
    let mut bytes = [0u8; 16];
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(ints) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    Some(bytes)
}
