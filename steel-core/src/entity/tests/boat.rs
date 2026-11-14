use super::*;
use crate::entity::entities::objects::vehicles::{AbstractBoat, BoatStatus};
use crate::entity::entities::{BoatEntity, ChestBoatEntity, ItemEntity};
use crate::entity::next_entity_id;
use crate::entity::snapshot_old_pos_and_rot_for_tick;
use crate::inventory::lock::ContainerLockGuard;
use crate::test_support::TestWorld;
use crate::test_support::dropped_items;

/// A 16×16 still-water pool three blocks deep (y 61..=63) on a stone floor,
/// with a stone and an ice strip beside it at x 0..=15, z 20..=21.
fn boat_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world(name);
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    insert_ready_full_chunk(world, ChunkPos::new(0, 1));
    for x in 0..=15 {
        for z in 0..=15 {
            assert!(world.set_block(
                BlockPos::new(x, 60, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_NONE,
            ));
            for y in 61..=63 {
                assert!(world.set_block(
                    BlockPos::new(x, y, z),
                    vanilla_blocks::WATER.default_state(),
                    UpdateFlags::UPDATE_NONE,
                ));
            }
        }
        for (z, block) in [(20, &vanilla_blocks::STONE), (21, &vanilla_blocks::ICE)] {
            assert!(world.set_block(
                BlockPos::new(x, 64, z),
                block.default_state(),
                UpdateFlags::UPDATE_NONE,
            ));
        }
    }
    world_fixture
}

fn spawn_boat(world: &Arc<World>, position: DVec3) -> Arc<BoatEntity> {
    let boat = Arc::new(BoatEntity::new(
        &vanilla_entities::OAK_BOAT,
        next_entity_id(),
        position,
        Arc::downgrade(world),
        &vanilla_items::OAK_BOAT,
    ));
    world
        .try_add_entity(Arc::clone(&boat) as SharedEntity)
        .expect("test boat should attach to the loaded chunk");
    boat
}

fn spawn_pig(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let pig: SharedEntity = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&pig))
        .expect("test pig should attach to the loaded chunk");
    pig
}

fn tick_boat(boat: &Arc<impl Entity>, ticks: usize) {
    for _ in 0..ticks {
        snapshot_old_pos_and_rot_for_tick(boat.as_ref());
        Arc::clone(boat).tick();
    }
}

#[test]
fn boat_dropped_onto_water_floats_at_the_surface() {
    let world_fixture = boat_world("boat_floats");
    let world = &world_fixture.world;
    let boat = spawn_boat(world, DVec3::new(8.5, 66.0, 8.5));

    tick_boat(&boat, 200);

    assert_eq!(boat.boat_status(), Some(BoatStatus::InWater));
    let y = boat.position().y;
    // A source block's surface is 8/9 above its base, at y ≈ 63.89.
    assert!(
        (63.3..64.0).contains(&y),
        "the boat should rest on the water surface, not at {y}"
    );
    assert!(
        boat.velocity().y.abs() < 0.01,
        "a resting boat should barely bob: {}",
        boat.velocity()
    );
}

#[test]
fn a_boat_slides_much_farther_on_ice_than_on_stone() {
    let world_fixture = boat_world("boat_slides_on_ice");
    let world = &world_fixture.world;
    let on_stone = spawn_boat(world, DVec3::new(2.5, 65.0, 20.5));
    let on_ice = spawn_boat(world, DVec3::new(2.5, 65.0, 21.5));
    for boat in [&on_stone, &on_ice] {
        tick_boat(boat, 5);
        boat.set_velocity(DVec3::new(0.5, 0.0, 0.0));
    }

    tick_boat(&on_stone, 40);
    tick_boat(&on_ice, 40);

    assert_eq!(on_stone.boat_status(), Some(BoatStatus::OnLand));
    assert_eq!(on_ice.boat_status(), Some(BoatStatus::OnLand));
    let stone_distance = on_stone.position().x - 2.5;
    let ice_distance = on_ice.position().x - 2.5;
    // Friction 0.6 stops a stone boat within ~1.25 blocks; ice (0.98) keeps it going.
    assert!(stone_distance < 1.5, "stone boat slid {stone_distance}");
    assert!(ice_distance > 8.0, "ice boat only slid {ice_distance}");
}

#[test]
fn a_pig_bumping_a_boat_climbs_in_until_it_is_full() {
    let world_fixture = boat_world("boat_takes_pigs");
    let world = &world_fixture.world;
    let boat = spawn_boat(world, DVec3::new(8.5, 64.0, 8.5));
    tick_boat(&boat, 40);
    let position = boat.position();
    let pigs: Vec<SharedEntity> = (0..3).map(|_| spawn_pig(world, position)).collect();

    tick_boat(&boat, 1);

    let passengers = boat.passengers();
    assert_eq!(passengers.len(), 2, "a boat seats two");
    assert!(pigs[2].vehicle().is_none(), "the third pig stays outside");
    assert!(
        boat.controlling_passenger().is_none(),
        "only a player steers a boat"
    );
    assert!(
        !boat.paddle_state(0),
        "paddles never move without a player at the helm"
    );
}

#[test]
fn a_chest_boat_seats_one_and_spills_its_chest_when_broken() {
    let world_fixture = boat_world("chest_boat_breaks");
    let world = &world_fixture.world;
    let position = DVec3::new(8.5, 64.0, 8.5);
    let boat = Arc::new(ChestBoatEntity::new(
        &vanilla_entities::OAK_CHEST_BOAT,
        next_entity_id(),
        position,
        Arc::downgrade(world),
        &vanilla_items::OAK_CHEST_BOAT,
    ));
    world
        .try_add_entity(Arc::clone(&boat) as SharedEntity)
        .expect("test chest boat should attach to the loaded chunk");
    let first = spawn_pig(world, position);
    let second = spawn_pig(world, position);
    assert!(first.start_riding(&(Arc::clone(&boat) as SharedEntity)));
    assert!(!second.start_riding(&(Arc::clone(&boat) as SharedEntity)));

    let chest = boat.container_ref();
    {
        let mut guard = ContainerLockGuard::lock_all(&[&chest]);
        guard.set_item(
            chest.container_id(),
            4,
            ItemStack::with_count(&vanilla_items::DIAMOND, 3),
        );
    }
    let source = DamageSource::environment(&vanilla_damage_types::GENERIC);
    assert!(boat.hurt(world, &source, 5.0));

    assert!(boat.is_removed(), "50 damage breaks the boat");
    let drops: Vec<ItemStack> = dropped_items(world, boat.position())
        .iter()
        .filter_map(|entity| {
            entity
                .downcast_ref::<ItemEntity>()
                .map(ItemEntity::get_item)
        })
        .collect();
    assert!(
        drops
            .iter()
            .any(|item| item.is(&vanilla_items::OAK_CHEST_BOAT))
    );
    assert!(
        drops
            .iter()
            .any(|item| item.is(&vanilla_items::DIAMOND) && item.count() == 3)
    );
}

#[test]
fn a_downward_bubble_column_sinks_the_boat_and_throws_out_its_riders() {
    let world_fixture = boat_world("boat_bubble_column");
    let world = &world_fixture.world;
    let boat = spawn_boat(world, DVec3::new(8.5, 64.0, 8.5));
    tick_boat(&boat, 40);
    let pig = spawn_pig(world, boat.position());
    assert!(pig.start_riding(&(Arc::clone(&boat) as SharedEntity)));

    for _ in 0..59 {
        boat.on_above_bubble_column(true, BlockPos::new(8, 63, 8));
        tick_boat(&boat, 1);
        assert!(pig.vehicle().is_some(), "riders stay until the column wins");
    }
    boat.on_above_bubble_column(true, BlockPos::new(8, 63, 8));
    snapshot_old_pos_and_rot_for_tick(boat.as_ref());
    Arc::clone(&boat).tick();

    assert!(
        pig.vehicle().is_none(),
        "the sinking boat throws its rider out"
    );
    assert!(
        boat.velocity().y < -0.5,
        "the column drags the boat down: {}",
        boat.velocity()
    );
}

#[test]
fn boats_save_their_leash_free_state_and_never_take_fall_damage() {
    let world_fixture = boat_world("boat_fall");
    let world = &world_fixture.world;
    let boat = spawn_boat(world, DVec3::new(2.5, 80.0, 20.5));

    tick_boat(&boat, 80);

    assert_eq!(boat.boat_status(), Some(BoatStatus::OnLand));
    assert!(!boat.is_removed(), "a falling boat survives the landing");
    assert!(boat.fall_distance().abs() < f64::EPSILON);
    let mut nbt = NbtCompound::new();
    boat.save_additional(&mut nbt);
    assert!(nbt.compound("leash").is_none());
}
