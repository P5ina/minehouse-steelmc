use super::*;
use crate::entity::entities::{ItemEntity, MinecartEntity};
use crate::entity::next_entity_id;
use crate::entity::snapshot_old_pos_and_rot_for_tick;
use crate::test_support::TestWorld;
use crate::test_support::dropped_items;

fn rail_world(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    let world_fixture = fresh_test_world(name);
    let world = &world_fixture.world;
    insert_ready_full_chunk(world, ChunkPos::new(0, 0));
    for x in 6..=10 {
        for z in 0..=15 {
            assert!(world.set_block(
                BlockPos::new(x, 64, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_NONE,
            ));
        }
    }
    for z in 0..=15 {
        assert!(world.set_block(
            BlockPos::new(8, 65, z),
            vanilla_blocks::RAIL.default_state(),
            UpdateFlags::UPDATE_NONE,
        ));
    }
    world_fixture
}

fn spawn_minecart(world: &Arc<World>, position: DVec3) -> Arc<MinecartEntity> {
    let cart = Arc::new(MinecartEntity::new(
        &vanilla_entities::MINECART,
        next_entity_id(),
        position,
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&cart) as SharedEntity)
        .expect("test minecart should attach to the loaded chunk");
    cart
}

#[test]
fn minecart_rolls_along_a_straight_rail() {
    let world_fixture = rail_world("minecart_rolls_along_rail");
    let world = &world_fixture.world;
    let cart = spawn_minecart(world, DVec3::new(8.5, 65.0, 4.5));
    cart.set_velocity(DVec3::new(0.0, 0.0, 0.3));

    for _ in 0..5 {
        snapshot_old_pos_and_rot_for_tick(cart.as_ref());
        Arc::clone(&cart).tick();
    }

    let position = cart.position();
    assert!(cart.is_on_rails(), "cart should stay on the rail");
    assert!(
        (position.x - 8.5).abs() < 1.0e-6,
        "cart left the rail axis: {position}"
    );
    // Vanilla rides 1/16 above the rail block (`OldMinecartBehavior.getPos`).
    assert!(
        (position.y - 65.0625).abs() < 1.0e-6,
        "cart left the rail height: {position}"
    );
    assert!(
        position.z > 5.5,
        "cart should have rolled south: {position}"
    );
}

#[test]
fn breaking_a_minecart_drops_its_item() {
    let world_fixture = rail_world("minecart_breaks_into_item");
    let world = &world_fixture.world;
    let position = DVec3::new(8.5, 65.0, 4.5);
    let cart = spawn_minecart(world, position);
    let source = DamageSource::environment(&vanilla_damage_types::GENERIC);

    assert!(cart.hurt(world, &source, 2.0));
    assert!(!cart.is_removed(), "20 damage should only wobble the cart");
    assert!(cart.hurt(world, &source, 3.0));

    assert!(cart.is_removed(), "50 damage should break the cart");
    let drops: Vec<ItemStack> = dropped_items(world, position)
        .iter()
        .filter_map(|entity| {
            entity
                .downcast_ref::<ItemEntity>()
                .map(ItemEntity::get_item)
        })
        .collect();
    assert_eq!(drops.len(), 1);
    assert!(drops[0].is(&vanilla_items::MINECART));
}

#[test]
fn passenger_getting_off_a_minecart_lands_on_the_first_free_offset() {
    let world_fixture = rail_world("minecart_dismount_beside_track");
    let world = &world_fixture.world;
    let cart = spawn_minecart(world, DVec3::new(8.5, 65.0, 8.5));
    let cart_shared: SharedEntity = Arc::clone(&cart) as SharedEntity;
    let pig: SharedEntity = Arc::new(PigEntity::new(
        &vanilla_entities::PIG,
        next_entity_id(),
        DVec3::new(8.5, 65.0, 8.5),
        Arc::downgrade(world),
    ));
    world
        .try_add_entity(Arc::clone(&pig))
        .expect("test pig should attach to the loaded chunk");
    assert!(start_riding_entities(&pig, &cart_shared));

    pig.stop_riding();

    assert!(!pig.is_passenger());
    // A cart facing south moves "west" (clockwise), so vanilla tries the block to its left,
    // the north one, first; rails have no collision, so the pig can stand on the track.
    assert_eq!(pig.position(), DVec3::new(8.5, 65.0, 7.5));
}
