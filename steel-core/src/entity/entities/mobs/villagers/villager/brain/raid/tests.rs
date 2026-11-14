use std::sync::Arc;

use glam::DVec3;
use steel_registry::data_components::components::FireworkExplosionShape;
use steel_registry::data_components::vanilla_components::FIREWORKS;
use steel_registry::dye_color::DyeColor;
use steel_registry::{
    RegistryEntry as _, init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items,
    vanilla_mob_effects, vanilla_poi_types,
};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos, Downcast as _, GlobalPos};

use super::{
    CelebrateVillagersSurvivedRaid, has_no_blocks_above, locate_hiding_place,
    move_to_sky_seeing_spot, react_to_bell, reset_raid_status, set_hidden_state, set_raid_status,
};
use crate::behavior::blocks::BellBlock;
use crate::behavior::init_behaviors;
use crate::block_entity::init_block_entities;
use crate::entity::ai::brain::memory_types::{HEARD_BELL_TIME, HIDING_PLACE, HOME, WALK_TARGET};
use crate::entity::ai::brain::{Activity, BehaviorControl as _};
use crate::entity::entities::VillagerEntity;
use crate::entity::{
    Entity as _, LivingEntity as _, MobEffectInstance, SharedEntity, init_entities, next_entity_id,
};
use crate::player::ResetReason;
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};
use crate::world::World;

/// A stone floor at y = 63 over the chunks around the origin.
fn floored_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    init_block_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    for x in -1..=1 {
        for z in -1..=1 {
            insert_ready_full_chunk(world, ChunkPos::new(x, z));
        }
    }
    for x in -16..32 {
        for z in -16..32 {
            world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_CLIENTS,
            );
        }
    }
    fixture
}

fn villager_at(world: &Arc<World>, pos: DVec3) -> SharedEntity {
    let entity: SharedEntity = Arc::new(VillagerEntity::new(
        &vanilla_entities::VILLAGER,
        next_entity_id(),
        pos,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&entity))
        .expect("villager attaches to the loaded chunk");
    entity
}

fn as_villager(entity: &SharedEntity) -> &VillagerEntity {
    entity
        .downcast_ref::<VillagerEntity>()
        .expect("villager is concrete")
}

fn add_home(world: &World, pos: BlockPos) {
    let home = vanilla_poi_types::HOME
        .try_id()
        .expect("vanilla home POI type is registered");
    world
        .poi_storage
        .lock()
        .add(pos, home, vanilla_poi_types::HOME.ticket_count);
}

#[test]
fn a_ringing_bell_sends_villagers_without_a_raid_into_hiding() {
    let fixture = floored_world("villager_raid_bell");
    let world = &fixture.world;
    let bell = BlockPos::new(8, 64, 8);
    world.set_block(
        bell,
        vanilla_blocks::BELL.default_state(),
        UpdateFlags::UPDATE_CLIENTS,
    );
    let near = villager_at(world, DVec3::new(12.5, 64.0, 8.5));
    // Inside the 48-block search box but past the 32-block hearing range.
    let far = villager_at(world, DVec3::new(-15.5, 64.0, 31.5));

    assert!(BellBlock::attempt_to_ring(None, world, bell, None));
    world.run_block_events();

    let heard = |entity: &SharedEntity| {
        as_villager(entity)
            .brain
            .memories()
            .lock()
            .get(&HEARD_BELL_TIME)
    };
    assert_eq!(heard(&near), Some(world.game_time()));
    assert_eq!(heard(&far), None, "only villagers within 32 blocks hear it");
    assert!(
        !BellBlock::attempt_to_ring(None, world, BlockPos::new(0, 64, 0), None),
        "no bell, no ring"
    );

    let villager = as_villager(&near);
    let mut react = react_to_bell();
    assert!(react.try_start(world, villager, world.game_time()));
    assert!(villager.brain.is_active(Activity::HIDE));
}

#[test]
fn hiding_villagers_head_home_and_give_up_after_a_while() {
    let fixture = floored_world("villager_raid_hide");
    let world = &fixture.world;
    let entity = villager_at(world, DVec3::new(8.5, 64.0, 8.5));
    let villager = as_villager(&entity);
    let home = BlockPos::new(18, 64, 8);
    add_home(world, home);

    let mut locate = locate_hiding_place(32, 0.625, 2);
    assert!(locate.try_start(world, villager, 0));
    {
        let memories = villager.brain.memories().lock();
        assert_eq!(
            memories.get(&HIDING_PLACE),
            Some(GlobalPos::new(world.key.clone(), home))
        );
        assert!(memories.has_value(&WALK_TARGET), "walks to the far home");
    }

    // With nothing in range it falls back to its own home.
    villager.brain.memories().lock().erase(&WALK_TARGET);
    world.poi_storage.lock().remove(home);
    let own_home = BlockPos::new(2, 64, 2);
    villager
        .brain
        .memories()
        .lock()
        .set(&HOME, Some(GlobalPos::new(world.key.clone(), own_home)));
    assert!(locate.try_start(world, villager, 1));
    assert_eq!(
        villager.brain.memories().lock().get(&HIDING_PLACE),
        Some(GlobalPos::new(world.key.clone(), own_home))
    );

    villager
        .brain
        .memories()
        .lock()
        .set(&HEARD_BELL_TIME, Some(100));
    let mut hidden = set_hidden_state(15, 3);
    assert!(hidden.try_start(world, villager, 399));
    assert!(villager.brain.memories().lock().has_value(&HEARD_BELL_TIME));
    assert!(
        hidden.try_start(world, villager, 400),
        "300 ticks after the bell it stops hiding"
    );
    let memories = villager.brain.memories().lock();
    assert!(!memories.has_value(&HEARD_BELL_TIME));
    assert!(!memories.has_value(&HIDING_PLACE));
}

#[test]
fn villagers_follow_a_raid_and_go_back_once_it_stops() {
    let fixture = floored_world("villager_raid_status");
    let world = &fixture.world;
    let center = BlockPos::new(8, 64, 8);
    add_home(world, center);
    assert!(world.poi_storage.lock().reserve_ticket(center));
    let player = TestPlayerBuilder::new(Arc::clone(world), "Omen", next_entity_id()).build();
    player.snap_to(DVec3::new(8.5, 64.0, 8.5), 0.0, 0.0);
    assert!(world.add_player(Arc::clone(&player), ResetReason::InitialJoin));
    assert!(player.add_mob_effect(MobEffectInstance::with_duration(
        vanilla_mob_effects::RAID_OMEN,
        600,
        0
    )));
    let raid = world
        .raids()
        .create_or_extend_raid(world, &player, center)
        .expect("the omen starts a raid");
    let entity = villager_at(world, DVec3::new(9.5, 64.0, 8.5));
    let villager = as_villager(&entity);

    // Vanilla rolls 1 in 20 each tick.
    let mut status = set_raid_status();
    assert!((0..2000).any(|time| status.try_start(world, villager, time)));
    assert!(
        villager.brain.is_active(Activity::PRE_RAID),
        "before the first wave the village gathers"
    );

    villager
        .brain
        .memories()
        .lock()
        .set(&HEARD_BELL_TIME, Some(0));
    assert!(react_to_bell().try_start(world, villager, 0));
    assert!(
        villager.brain.is_active(Activity::PRE_RAID),
        "a bell during a raid does not send it into hiding"
    );

    raid.stop();
    let mut reset = reset_raid_status();
    assert!((0..2000).any(|time| reset.try_start(world, villager, time)));
    assert!(!villager.brain.is_active(Activity::PRE_RAID));
}

#[test]
fn villagers_under_a_roof_walk_out_to_open_sky() {
    let fixture = floored_world("villager_raid_sky");
    let world = &fixture.world;
    let entity = villager_at(world, DVec3::new(8.5, 64.0, 8.5));
    let villager = as_villager(&entity);
    assert!(has_no_blocks_above(
        world,
        villager,
        villager.block_position()
    ));
    let mut to_sky = move_to_sky_seeing_spot(0.5);
    assert!(
        !to_sky.try_start(world, villager, 0),
        "already under the sky"
    );

    for x in 7..=9 {
        for z in 7..=9 {
            world.set_block(
                BlockPos::new(x, 66, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    // The world tick loop drains queued light updates; tests do it by hand.
    world.chunk_map.broadcast_changed_chunks();
    assert!(!has_no_blocks_above(
        world,
        villager,
        villager.block_position()
    ));
    // Ten random tries in a 20x6x20 box nearly always find open sky.
    assert!((0..20).any(|time| {
        to_sky.try_start(world, villager, time)
            && villager.brain.memories().lock().has_value(&WALK_TARGET)
    }));
}

#[test]
fn celebration_fireworks_burst_in_one_color() {
    init_vanilla_registry();
    let rocket =
        CelebrateVillagersSurvivedRaid::firework(DyeColor::Lime, 2).expect("a valid firework");
    assert!(rocket.is(&vanilla_items::FIREWORK_ROCKET));
    let fireworks = rocket.get(FIREWORKS).expect("fireworks component");
    assert_eq!(fireworks.flight_duration(), 2);
    let [explosion] = fireworks.explosions() else {
        panic!("one explosion");
    };
    assert_eq!(explosion.shape(), FireworkExplosionShape::Burst);
    assert_eq!(explosion.colors(), [DyeColor::Lime.firework_color()]);
    assert_eq!(explosion.fade_colors(), &[] as &[i32]);
    assert!(!explosion.has_trail() && !explosion.has_twinkle());
}
