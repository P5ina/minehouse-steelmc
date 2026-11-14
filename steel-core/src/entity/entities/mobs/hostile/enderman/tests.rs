use std::io::Cursor;
use std::sync::{Arc, Weak};

use glam::DVec3;
use simdnbt::borrow::read_compound;
use simdnbt::owned::NbtCompound;
use steel_registry::equipment::EquipmentSlot;
use steel_registry::item_stack::ItemStack;
use steel_registry::{
    init_vanilla_registry, vanilla_attributes, vanilla_blocks, vanilla_entities, vanilla_items,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos};

use super::{EndermanEntity, clamped_pack_difference_in_position};
use crate::behavior::init_behaviors;
use crate::entity::{Entity, LivingEntity as _, Mob, SharedEntity, init_entities, next_entity_id};
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};

fn enderman_at(position: DVec3) -> EndermanEntity {
    EndermanEntity::new(&vanilla_entities::ENDERMAN, 1, position, Weak::new())
}

/// A world with one loaded chunk and a stone floor at y = 63.
fn floor_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let world_fixture = fresh_test_world(name);
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    world_fixture
}

#[test]
fn carried_block_round_trips_and_keeps_the_enderman() {
    init_vanilla_registry();
    let enderman = enderman_at(DVec3::ZERO);
    assert!(!enderman.requires_custom_persistence());
    enderman.set_carried_block(Some(vanilla_blocks::GRASS_BLOCK.default_state()));
    assert!(
        enderman.requires_custom_persistence(),
        "an enderman holding a block never despawns"
    );

    let mut nbt = NbtCompound::new();
    enderman.save_additional(&mut nbt);
    assert_eq!(
        nbt.string("carriedBlockState").map(|name| name.to_str()),
        Some("minecraft:grass_block".into()),
        "vanilla writes a default carried state as its block id"
    );
    let mut bytes = Vec::new();
    nbt.write(&mut bytes);
    let borrowed = read_compound(&mut Cursor::new(&bytes))
        .unwrap_or_else(|error| panic!("reborrow failed: {error}"));
    let loaded = enderman_at(DVec3::ZERO);
    loaded.load_additional((&borrowed).into());
    assert_eq!(
        loaded.carried_block(),
        Some(vanilla_blocks::GRASS_BLOCK.default_state())
    );
}

#[test]
fn a_target_makes_it_creepy_and_faster() {
    let world_fixture = floor_world("enderman_target_speed");
    let world = &world_fixture.world;
    let enderman = EndermanEntity::new(
        &vanilla_entities::ENDERMAN,
        next_entity_id(),
        DVec3::new(8.5, 64.0, 8.5),
        Arc::downgrade(world),
    );
    let base_speed = enderman
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_SPEED);
    let player = TestPlayerBuilder::new(Arc::clone(world), "Starer", 2).build();
    player
        .base()
        .set_position_local(DVec3::new(10.5, 64.0, 8.5));
    let target: SharedEntity = player;

    assert!(enderman.set_target(Some(&target)));
    assert!(enderman.is_creepy());
    let speed = enderman
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_SPEED);
    assert!((speed - base_speed - 0.15).abs() < 1.0e-9);

    assert!(enderman.set_target(None));
    assert!(!enderman.is_creepy());
    assert!(!enderman.has_been_stared_at());
    let speed = enderman
        .attributes()
        .lock()
        .required_value(vanilla_attributes::MOVEMENT_SPEED);
    assert!((speed - base_speed).abs() < 1.0e-9);
}

#[test]
fn a_bare_headed_player_looking_at_its_eyes_stares() {
    let world_fixture = floor_world("enderman_stare");
    let world = &world_fixture.world;
    let enderman = EndermanEntity::new(
        &vanilla_entities::ENDERMAN,
        next_entity_id(),
        DVec3::new(8.5, 64.0, 12.5),
        Arc::downgrade(world),
    );
    let player = TestPlayerBuilder::new(Arc::clone(world), "Starer", 2).build();
    player.base().set_position_local(DVec3::new(8.5, 64.0, 2.5));
    // Face +Z, pitched up toward the enderman's eyes.
    let eye_rise = enderman.get_eye_y() - player.get_eye_y();
    let pitch = -(eye_rise.atan2(10.0).to_degrees() as f32);
    player.set_rotation((0.0, pitch));
    player.set_y_head_rot(0.0);
    assert!(enderman.is_being_stared_by(&player));

    player.set_rotation((90.0, pitch));
    player.set_y_head_rot(90.0);
    assert!(
        !enderman.is_being_stared_by(&player),
        "looking away is no stare"
    );

    player.set_rotation((0.0, pitch));
    player.set_y_head_rot(0.0);
    player.with_equipment_slot_mut(EquipmentSlot::Head, &mut |helmet| {
        *helmet = ItemStack::new(&vanilla_items::CARVED_PUMPKIN);
    });
    assert!(
        !enderman.is_being_stared_by(&player),
        "a carved pumpkin hides the gaze"
    );
}

#[test]
fn teleporting_lands_on_the_floor_and_avoids_water() {
    let world_fixture = floor_world("enderman_teleport");
    let world = &world_fixture.world;
    let enderman = EndermanEntity::new(
        &vanilla_entities::ENDERMAN,
        next_entity_id(),
        DVec3::new(2.5, 64.0, 2.5),
        Arc::downgrade(world),
    );
    assert!(enderman.teleport_enderman(DVec3::new(12.5, 70.0, 12.5)));
    let position = enderman.position();
    assert!((position.x - 12.5).abs() < 1.0e-9);
    assert!(
        (position.y - 64.0).abs() < 1.0e-9,
        "drops onto the floor: {position}"
    );

    world.set_block(
        BlockPos::new(6, 63, 6),
        vanilla_blocks::WATER.default_state(),
        UpdateFlags::UPDATE_ALL,
    );
    assert!(
        !enderman.teleport_enderman(DVec3::new(6.5, 64.0, 6.5)),
        "never onto water"
    );
}

#[test]
fn packs_the_teleport_offset_like_vanilla() {
    let from = BlockPos::new(10, 64, 10);
    assert_eq!(
        clamped_pack_difference_in_position(from, BlockPos::new(11, 62, 13), 127, 127, 127),
        (0x80 << 16) | (0x7D << 8) | 0x82
    );
    // Offsets beyond 127 blocks are clamped.
    assert_eq!(
        clamped_pack_difference_in_position(from, BlockPos::new(400, 64, -400), 127, 127, 127),
        (0xFE << 16) | (0x7F << 8)
    );
}
