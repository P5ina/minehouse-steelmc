//! Vanilla `Player.attack`: critical hits and sword sweep attacks.

use std::sync::Arc;

use glam::DVec3;
use steel_registry::item_stack::ItemStack;
use steel_registry::{init_vanilla_registry, vanilla_blocks, vanilla_entities, vanilla_items};
use steel_utils::types::UpdateFlags;
use steel_utils::{BlockPos, ChunkPos};

use super::Player;
use crate::behavior::init_behaviors;
use crate::entity::{Entity, LivingEntity, SharedEntity, create_entity_instance, init_entities};
use crate::test_support::{
    TestPlayerBuilder, TestWorld, fresh_test_world, insert_ready_full_chunk,
};
use crate::world::World;

fn arena(name: &'static str) -> TestWorld {
    init_vanilla_registry();
    init_behaviors();
    init_entities();
    let fixture = fresh_test_world(name);
    let world = &fixture.world;
    insert_ready_full_chunk(world, ChunkPos::from_block_pos(BlockPos::new(8, 64, 8)));
    for x in 0..16 {
        for z in 0..16 {
            world.set_block(
                BlockPos::new(x, 63, z),
                vanilla_blocks::STONE.default_state(),
                UpdateFlags::UPDATE_ALL,
            );
        }
    }
    fixture
}

fn zombie_at(world: &Arc<World>, position: DVec3) -> SharedEntity {
    let zombie =
        create_entity_instance(world, &vanilla_entities::ZOMBIE, position).expect("zombie factory");
    world
        .try_add_entity(Arc::clone(&zombie))
        .expect("zombie added");
    zombie.set_on_ground(true);
    zombie
}

/// A fully charged player on the ground at (8.5, 64, 8.5) holding `item`.
fn attacker(world: &Arc<World>, item: ItemStack) -> Arc<Player> {
    let player = TestPlayerBuilder::new(Arc::clone(world), "Attacker", 1).build();
    player.set_client_loaded(true);
    player
        .teleport_to(DVec3::new(8.5, 64.0, 8.5))
        .expect("player teleport");
    player.set_on_ground(true);
    player.inventory.lock().set_selected_item(item);
    LivingEntity::detect_equipment_updates(player.as_ref());
    charge(&player);
    player
}

fn charge(player: &Player) {
    for _ in 0..40 {
        player.tick_attack_strength();
    }
}

fn health(entity: &SharedEntity) -> f32 {
    entity
        .as_living_entity()
        .map_or(0.0, LivingEntity::get_health)
}

#[test]
fn full_strength_sword_hit_sweeps_nearby_mobs() {
    let fixture = arena("player_attack_sweep");
    let world = &fixture.world;
    let player = attacker(world, ItemStack::new(&vanilla_items::DIAMOND_SWORD));
    let target = zombie_at(world, DVec3::new(8.5, 64.0, 10.0));
    let neighbour = zombie_at(world, DVec3::new(9.6, 64.0, 10.0));
    let far = zombie_at(world, DVec3::new(13.5, 64.0, 10.0));
    let (neighbour_health, far_health) = (health(&neighbour), health(&far));

    assert!(player.attack(&target));

    assert!(health(&neighbour) < neighbour_health, "neighbour is swept");
    assert!(
        (health(&far) - far_health).abs() < f32::EPSILON,
        "mobs outside the sweep box keep their health"
    );
}

#[test]
fn sweep_needs_a_sword() {
    let fixture = arena("player_attack_no_sweep_axe");
    let world = &fixture.world;
    let player = attacker(world, ItemStack::new(&vanilla_items::DIAMOND_AXE));
    let target = zombie_at(world, DVec3::new(8.5, 64.0, 10.0));
    let neighbour = zombie_at(world, DVec3::new(9.6, 64.0, 10.0));
    let neighbour_health = health(&neighbour);

    assert!(player.attack(&target));

    assert!((health(&neighbour) - neighbour_health).abs() < f32::EPSILON);
}

#[test]
fn falling_hit_is_critical_and_does_not_sweep() {
    let fixture = arena("player_attack_crit");
    let world = &fixture.world;

    let grounded = attacker(world, ItemStack::new(&vanilla_items::WOODEN_SWORD));
    let normal_target = zombie_at(world, DVec3::new(4.5, 64.0, 4.5));
    let before = health(&normal_target);
    assert!(grounded.attack(&normal_target));
    let normal_damage = before - health(&normal_target);

    let falling = attacker(world, ItemStack::new(&vanilla_items::WOODEN_SWORD));
    falling.set_on_ground(false);
    falling.set_fall_distance(1.0);
    let crit_target = zombie_at(world, DVec3::new(8.5, 64.0, 10.0));
    let neighbour = zombie_at(world, DVec3::new(9.6, 64.0, 10.0));
    let (before, neighbour_health) = (health(&crit_target), health(&neighbour));
    assert!(falling.attack(&crit_target));
    let crit_damage = before - health(&crit_target);

    assert!(
        crit_damage > normal_damage * 1.3,
        "critical hit deals 1.5x base damage: {crit_damage} vs {normal_damage}"
    );
    assert!(
        (health(&neighbour) - neighbour_health).abs() < f32::EPSILON,
        "critical hits never sweep"
    );
}
